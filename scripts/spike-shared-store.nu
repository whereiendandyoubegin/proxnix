#!/usr/bin/env nu

const ID = 999
const SPIKE = "/ZFS/proxnix-spike1"
const IDMAP = 100000
const BIN = "/run/current-system/sw/bin"

def --wrapped inside [...argv: string]: nothing -> record {
  do { ^pct exec $ID -- ...$argv } | complete
}

def conf [toplevel: string]: nothing -> string {
  [
    "arch: amd64"
    "cores: 1"
    "features: nesting=1"
    "hostname: spike-shared-store"
    "memory: 1024"
    $"mp0: ($SPIKE)/logs,mp=/var/log/journal"
    "mp1: /var/lib/proxnix/sops,mp=/var/lib/sops-key,ro=1"
    $"mp2: ($SPIKE)/store/nix/store,mp=/nix/store,ro=1"
    "net0: name=eth0,bridge=vmbr0,hwaddr=02:70:78:00:03:E7,type=veth"
    "ostype: unmanaged"
    "protection: 0"
    $"rootfs: ZFS:subvol-($ID)-disk-0,size=2G"
    "tags: proxnix-spike"
    "unprivileged: 1"
  ] | str join (char nl) | $"($in)(char nl)"
}

def stub [rootfs: string, toplevel: string] {
  for dir in [sbin etc proc sys dev nix/store var/log/journal] { mkdir ($rootfs | path join $dir) }
  ^ln -s $"($toplevel)/init" ($rootfs | path join sbin/init)
  ^cp -L $"($toplevel)/etc/os-release" ($rootfs | path join etc/os-release)
  ^chown -R $"($IDMAP):($IDMAP)" $rootfs
}

def booted []: nothing -> string {
  let states = 1..60 | each {|_|
    sleep 2sec
    inside $"($BIN)/systemctl" is-system-running | get stdout | str trim
  } | take until {|state| $state in [running degraded] }
  inside $"($BIN)/systemctl" is-system-running | get stdout | str trim
}

def checks [toplevel: string]: nothing -> table {
  let owner = inside $"($BIN)/stat" -c "%u" $toplevel
  let sudo = inside $"($BIN)/runuser" -u dan -- /run/wrappers/bin/sudo -n "true"
  let journal = inside $"($BIN)/ls" /var/log/journal
  let secrets = inside $"($BIN)/ls" /run/secrets
  [
    {check: "system state", result: (booted)}
    {check: "failed units", result: (inside $"($BIN)/systemctl" --failed --no-legend --plain | get stdout | str trim)}
    {check: "sshd active", result: (inside $"($BIN)/systemctl" is-active sshd | get stdout | str trim)}
    {check: "store owner uid (65534 = nobody)", result: ($owner.stdout | str trim)}
    {check: "sudo as dan via setuid wrapper", result: (if $sudo.exit_code == 0 { "works" } else { $"exit ($sudo.exit_code): ($sudo.stderr | str trim)" })}
    {check: "journal on the logs mount", result: (if ($journal.stdout | str trim | is-empty) { "empty" } else { $journal.stdout | str trim })}
    {check: "sops secrets decrypted", result: (if $secrets.exit_code == 0 { $secrets.stdout | lines | length | $"($in) entries" } else { $secrets.stderr | str trim })}
  ]
}

def cleanup [] {
  print "==> cleaning up"
  do { ^pct stop $ID } | complete | ignore
  do { ^pct destroy $ID --purge } | complete | ignore
  do { ^zfs destroy $"ZFS/subvol-($ID)-disk-0" } | complete | ignore
  if ($SPIKE | path exists) {
    ^chmod -R u+w $SPIKE
    rm -rf $SPIKE
  }
}

def main [] {
  if (^id -u | str trim) != "0" { error make {msg: "run this as root on pve01"} }
  if (do { ^pct status $ID } | complete).exit_code == 0 { error make {msg: $"CT ($ID) exists; this spike needs it free"} }

  print "==> the current test-container toplevel, from the host store"
  let toplevel = ^nix build --no-link --print-out-paths /root/nixology#nixosConfigurations.build-lxc.config.system.build.toplevel | str trim
  print $toplevel

  let result = try {
    print "==> seeding the scratch store from the host store"
    mkdir $"($SPIKE)/store"
    timeit { ^nix copy --to $"local?root=($SPIKE)/store" $toplevel } | print

    print "==> a stub rootfs"
    ^pvesm alloc ZFS $ID $"subvol-($ID)-disk-0" 2G
    stub $"/ZFS/subvol-($ID)-disk-0" $toplevel
    mkdir $"($SPIKE)/logs"
    ^chown $"($IDMAP):($IDMAP)" $"($SPIKE)/logs"

    print "==> the container config M11 will write"
    let staged = $"/etc/pve/lxc/($ID).conf.tmp.(random int 1000..9999)"
    conf $toplevel | save $staged
    mv $staged $"/etc/pve/lxc/($ID).conf"
    print (open --raw $"/etc/pve/lxc/($ID).conf")

    print "==> booting"
    ^pct start $ID
    checks $toplevel
  } catch {|e|
    [{check: "spike", result: $"failed: ($e.msg)"}]
  }

  cleanup
  print ($result | table --expand)
}
