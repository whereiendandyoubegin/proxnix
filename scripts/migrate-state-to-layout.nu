#!/usr/bin/env nu

const STATE = [
  [workload from dataset];
  [forgejo /var/lib/proxnix/forgejo ZFS/proxnix/state/forgejo/forgejo]
  [monitoring /var/lib/proxnix/monitoring ZFS/proxnix/state/monitoring/monitoring]
  [postgres /var/lib/proxnix/postgres ZFS/proxnix/state/postgres/postgresql]
  [flake-updater /var/lib/proxnix/updater ZFS/proxnix/state/flake-updater/updater]
  [hydra /var/lib/proxnix/hydra ZFS/proxnix/state/hydra/hydra]
  [hydra /var/lib/proxnix/harmonia ZFS/proxnix/state/hydra/nix-cache-key]
  [nixflix /var/lib/proxnix/nixflix ZFS/proxnix/state/nixflix/state]
  [nixflix /var/lib/proxnix/nixflix-sabnzbd ZFS/proxnix/state/nixflix/sabnzbd]
  [neon-broker /var/lib/proxnix/neon-broker ZFS/proxnix/state/neon-broker/neon]
  [neon-storcon /var/lib/proxnix/neon-storcon ZFS/proxnix/state/neon-storcon/neon]
  [neon-safekeeper-1 /var/lib/proxnix/neon-safekeeper-1 ZFS/proxnix/state/neon-safekeeper-1/neon]
  [neon-safekeeper-2 /var/lib/proxnix/neon-safekeeper-2 ZFS/proxnix/state/neon-safekeeper-2/neon]
  [neon-safekeeper-3 /var/lib/proxnix/neon-safekeeper-3 ZFS/proxnix/state/neon-safekeeper-3/neon]
  [neon-pageserver /var/lib/proxnix/neon-pageserver ZFS/proxnix/state/neon-pageserver/neon]
]

const LOGGED = [
  cloudflared flake-updater forgejo hydra monitoring nixflix postgres test-container
  neon-broker neon-storcon neon-safekeeper-1 neon-safekeeper-2 neon-safekeeper-3 neon-pageserver
]

const LOGS = "ZFS/proxnix/logs"

def moves []: nothing -> table {
  let state = $STATE | each {|row| $row | insert to $"/($row.dataset)" }
  let logs = $LOGGED | each {|workload|
    {workload: $workload, from: $"/var/lib/proxnix/logs/($workload)", dataset: $LOGS, to: $"/($LOGS)/($workload)"}
  }
  $state | append $logs | where {|move| $move.from | path exists }
}

def ancestors [dataset: string]: nothing -> list<string> {
  let parts = $dataset | split row "/"
  2..($parts | length) | each {|n| $parts | first $n | str join "/" }
}

def guests []: nothing -> table {
  ^pct list | lines | skip 1 | parse -r '^\s*(?<id>\d+)\s+(?<status>\S+)' | each {|row|
    let id = $row.id | into int
    let config = ^pct config $id | lines
    {
      id: $id
      running: ($row.status == "running")
      protected: ("protection: 1" in $config)
      mounts: ($config | parse -r '^(?<key>mp\d+): (?<host>[^,]+),(?<rest>.*)$')
    }
  }
}

def repoint_all [repoint: table, protected: list<int>] {
  for id in ($repoint | get id | uniq) {
    if $id in $protected { ^pct set $id -protection 0 }
    for mount in ($repoint | where id == $id) { ^pct set $id $"-($mount.key)" $mount.value }
    if $id in $protected { ^pct set $id -protection 1 }
  }
}

def ensure_dataset [dataset: string] {
  if (do { ^zfs list -H -o name $dataset } | complete).exit_code != 0 {
    ^zfs create -o acltype=posixacl -o xattr=sa -o atime=off $dataset
  }
  let mounted = ^zfs get -H -o value mountpoint $dataset | str trim
  if $mounted != $"/($dataset)" {
    error make {msg: $"($dataset) is mounted at ($mounted), expected /($dataset); fix that before migrating"}
  }
}

def copy [move: record] {
  mkdir $move.to
  ^rsync -aHAX --numeric-ids $"($move.from)/" $"($move.to)/"
  let drift = ^rsync -aHAXn --numeric-ids --delete --checksum --itemize-changes $"($move.from)/" $"($move.to)/" | lines | where ($it | str trim | is-not-empty)
  if ($drift | is-not-empty) {
    error make {msg: $"($move.to) differs from ($move.from): ($drift | first 5 | str join ', ')"}
  }
}

def main [--apply] {
  if (^id -u | str trim) != "0" { error make {msg: "run this as root on pve01"} }
  for tool in [zfs pct rsync] {
    if (which $tool | is-empty) { error make {msg: $"($tool) not found"} }
  }
  if (do { ^pgrep -x proxnix } | complete).exit_code == 0 {
    error make {msg: "proxnix is running; stop it first"}
  }

  let moves = moves
  let sources = $moves | get from
  let affected = guests | where {|guest| $guest.mounts | any {|mount| $mount.host in $sources } }
  let repoint = $affected | each {|guest|
    $guest.mounts | where host in $sources | each {|mount|
      let move = $moves | where from == $mount.host | first
      {id: $guest.id, key: $mount.key, from: $mount.host, to: $move.to, value: $"($move.to),($mount.rest)"}
    }
  } | flatten

  print "moves:"
  print ($moves | select workload from to)
  print "mount points to repoint:"
  print ($repoint | select id key from to)
  print $"guests to stop and restart: ($affected | where running | get id | str join ' ')"
  print $"protected guests, unprotected only while their mounts are repointed: ($affected | where protected | get id | str join ' ')"

  if not $apply {
    print "dry run; nothing changed. Run again with --apply to migrate."
    return
  }

  let running = $affected | where running | get id
  for id in $running { ^pct stop $id }

  let copied = try {
    $moves | get dataset | uniq | each {|dataset| ancestors $dataset } | flatten | uniq | each {|dataset| ensure_dataset $dataset }
    for move in $moves { copy $move }
    true
  } catch {|e|
    print $"copy failed, originals untouched: ($e.msg)"
    false
  }
  if not $copied {
    for id in $running { ^pct start $id }
    error make {msg: "migration aborted before any guest was changed; guests restarted"}
  }

  let protected = $affected | where protected | get id
  let repointed = try {
    repoint_all $repoint $protected
    true
  } catch {|e|
    print $"repointing failed: ($e.msg)"
    for id in $protected { do { ^pct set $id -protection 1 } | complete }
    false
  }
  if $repointed {
    for move in $moves { mv $move.from $"($move.from).pre-layout" }
  }
  for id in $running { ^pct start $id }
  if not $repointed {
    error make {msg: "some mount points were not repointed; every original is still in place and every guest was restarted. Compare pct config with the plan above before running again"}
  }

  print "migrated. Old data is kept beside each source as *.pre-layout; remove it once everything is healthy:"
  print ($moves | each {|move| $"  rm -rf ($move.from).pre-layout" } | str join (char nl))
}
