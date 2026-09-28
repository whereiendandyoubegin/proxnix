const secret_keys = [cipassword sshkeys description]

def default-out [] {
  "~/cloned/proxnix/nix-deployments-rs/nix-deployments-rs/proxnix/fixtures/api" | path expand
}

def fetch [host: string, socket: path, header: string, path: string] {
  $header
  | ^ssh -o $"ControlPath=($socket)" $host curl --silent --show-error --fail --cacert /etc/pve/pve-root-ca.pem --header @- $"https://localhost:8006/api2/json($path)"
  | from json
  | get data
}

def redact [] {
  match ($in | describe | str replace --regex '<.*' '') {
    record => ($in | reject --optional ...$secret_keys)
    _ => $in
  }
}

def store [out: path, relative: string, value: any] {
  let file = $out | path join $relative
  mkdir ($file | path dirname)
  $value | redact | to json --indent 2 | save --force $file
  $relative
}

def guest-reads [kind: string, guest: record] {
  let base = $"($kind)/($guest.vmid)"
  let running = $guest.status == "running"
  [
    { file: $"($base)/config.json", path: $"($base)/config", wanted: true }
    { file: $"($base)/agent/network-get-interfaces.json", path: $"($base)/agent/network-get-interfaces", wanted: ($kind == qemu and $running) }
    { file: $"($base)/interfaces.json", path: $"($base)/interfaces", wanted: ($kind == lxc and $running) }
  ]
  | where wanted
}

export def main [
  host: string
  --nixology: path = "~/cloned/nixology"
  --out: path
] {
  let out = $out | default (default-out) | path expand
  let api = ^nix eval $"($nixology | path expand)#proxnixcfg.proxmox" --json | from json
  let control = mktemp --directory
  let socket = $control | path join ssh.sock
  ^ssh -o ControlMaster=yes -o $"ControlPath=($socket)" -o ControlPersist=300 -fN $host
  let token = ^ssh -o $"ControlPath=($socket)" $host cat $api.token_file | str trim
  let header = $"Authorization: PVEAPIToken=($api.user)@($api.realm)!($api.token_id)=($token)"
  let get = {|path| fetch $host $socket $header $"/nodes/($api.node)/($path)" }

  let tasks = do $get "tasks?limit=5"
  let failed = do $get "tasks?errors=1&limit=10"
  let task_files = [(store $out "tasks.json" $tasks) (store $out "failed-tasks.json" $failed)]
    | append ($tasks | first 3 | append ($failed | first 5) | each {|task|
        try { store $out $"tasks/($task.upid | str replace --all ':' '_')/status.json" (do $get $"tasks/($task.upid)/status") } catch {|e| $"skipped task ($task.upid): ($e.msg)" }
      })

  let guest_files = [qemu lxc]
  | each {|kind|
      let guests = do $get $kind
      [(store $out $"($kind).json" $guests)]
      | append ($guests | each {|guest|
          guest-reads $kind $guest
          | each {|read|
              try { store $out $read.file (do $get $read.path) } catch {|e| $"skipped ($read.file): ($e.msg)" }
            }
        } | flatten)
    }
  | flatten

  ^ssh -o $"ControlPath=($socket)" -O exit $host
  rm --recursive $control
  $task_files | append $guest_files
}
