use callsites.nu

const pve_tools = [pct qm lxc-info]

const endpoint_map = {
  cli: {
    "qm list": { method: GET, path: "/nodes/{node}/qemu", positional: [] }
    "qm config": { method: GET, path: "/nodes/{node}/qemu/{vmid}/config", positional: [vmid] }
    "qm create": { method: POST, path: "/nodes/{node}/qemu", positional: [vmid] }
    "qm set": { method: PUT, path: "/nodes/{node}/qemu/{vmid}/config", positional: [vmid] }
    "qm disk resize": { method: PUT, path: "/nodes/{node}/qemu/{vmid}/resize", positional: [vmid disk size] }
    "qm start": { method: POST, path: "/nodes/{node}/qemu/{vmid}/status/start", positional: [vmid] }
    "qm stop": { method: POST, path: "/nodes/{node}/qemu/{vmid}/status/stop", positional: [vmid] }
    "qm destroy": { method: DELETE, path: "/nodes/{node}/qemu/{vmid}", positional: [vmid] }
    "qm agent network-get-interfaces": { method: GET, path: "/nodes/{node}/qemu/{vmid}/agent/network-get-interfaces", positional: [vmid] }
    "pct list": { method: GET, path: "/nodes/{node}/lxc", positional: [] }
    "pct config": { method: GET, path: "/nodes/{node}/lxc/{vmid}/config", positional: [vmid] }
    "pct create": { method: POST, path: "/nodes/{node}/lxc", positional: [vmid ostemplate] }
    "pct set": { method: PUT, path: "/nodes/{node}/lxc/{vmid}/config", positional: [vmid] }
    "pct start": { method: POST, path: "/nodes/{node}/lxc/{vmid}/status/start", positional: [vmid] }
    "pct stop": { method: POST, path: "/nodes/{node}/lxc/{vmid}/status/stop", positional: [vmid] }
    "pct destroy": { method: DELETE, path: "/nodes/{node}/lxc/{vmid}", positional: [vmid] }
    "pct exec": null
    "lxc-info": { method: GET, path: "/nodes/{node}/lxc/{vmid}/interfaces", positional: [vmid] }
  }
  extra: [
    { method: GET, path: "/cluster/resources" }
    { method: GET, path: "/cluster/status" }
    { method: GET, path: "/nodes/{node}/tasks/{upid}/status" }
  ]
}

export def "main calls" [dir?: path] {
  callsites $dir
  | where {|site| ($site.key | split row " " | first) in $pve_tools }
  | where {|site| not ($site.key | str contains "<any GuestOp>") }
  | select file line key
  | uniq
  | insert flags []
  | insert dynamic_flags []
  | insert spread false
}

def schema-param [names: list, flag: string] {
  if $flag in $names { return $flag }
  let indexed = $flag | str replace --regex '\d+$' '[n]'
  if $indexed in $names { $indexed } else { null }
}

const crate_rules = '
id: field
language: rust
rule:
  kind: field_declaration
  all:
    - has: { field: name, pattern: $NAME }
    - has: { field: type, pattern: $TY }
  inside:
    kind: field_declaration_list
    inside:
      kind: struct_item
      has: { field: name, pattern: $S, regex: ^(Get|Post|Put|Delete)Params$ }
---
id: renamed
language: rust
rule:
  kind: field_declaration
  follows:
    stopBy: { kind: field_declaration }
    pattern: "#[serde(rename = $R)]"
---
id: method
language: rust
rule:
  kind: function_item
  regex: ^pub async fn
  has: { field: name, pattern: $M, regex: ^(get|post|put|delete)$ }
---
id: method-doc
language: rust
rule:
  pattern: "#[doc = $D]"
  precedes:
    stopBy: { not: { kind: attribute_item } }
    kind: function_item
    regex: ^pub async fn
    has: { field: name, pattern: $M, regex: ^(get|post|put|delete)$ }
'

def crate-root [] {
  ^cargo metadata --format-version 1 --offline --filter-platform x86_64-unknown-linux-gnu --manifest-path ("~/cloned/proxnix/nix-deployments-rs/nix-deployments-rs/Cargo.toml" | path expand)
  | from json
  | get packages
  | where name == "proxmox-api"
  | first
  | get manifest_path
  | path dirname
}

def segments [path: string] {
  $path | split row "/" | where $it != "" | each {|s|
    let param = $s | str starts-with "{"
    { name: ($s | str trim --char "{" | str trim --char "}" | str replace --all "-" "_"), param: $param }
  }
}

# Reads the generated client module for an endpoint and returns its params struct.
def crate-endpoint [root: path, endpoint: record] {
  let segs = segments $endpoint.path
  let module = $segs | get name
  let file = $root | path join src generated ($module | str join "/" | $in + ".rs")
  if not ($file | path exists) { error make { msg: $"($endpoint.path): no module at ($file)" } }

  let method = $endpoint.method | str lowercase
  let struct = ($method | str capitalize) + "Params"
  let matches = ^ast-grep scan --inline-rules $crate_rules --json=compact $file | from json

  let fn = $matches | where ruleId == "method" | where {|m| $m.metaVariables.single.M.text == $method }
  if ($fn | is-empty) { error make { msg: $"($endpoint.path): crate has no `($method)`" } }
  let fn = $fn | first
  let takes_params = $fn.text | str contains $"params: ($struct)"

  let renames = $matches | where ruleId == "renamed"
    | each {|r| { at: $r.range.byteOffset.start, name: ($r.metaVariables.single.R.text | str trim --char '"') } }
  let fields = $matches
    | where ruleId == "field"
    | where {|f| $f.metaVariables.single.S.text == $struct }
    | where {|f| $f.metaVariables.single.NAME.text != "additional_properties" }
    | each {|f|
        let ident = $f.metaVariables.single.NAME.text
        let ty = $f.metaVariables.single.TY.text
        let rename = $renames | where at == $f.range.byteOffset.start
        {
          name: (if ($rename | is-empty) { $ident } else { $rename.0.name })
          field: $ident
          type: $ty
          required: (not ($ty | str starts-with "Option<") and not ($ty | str contains "HashMap"))
        }
      }

  # Docs are: summary, "", then the permission text.
  let permission = $matches
    | where ruleId == "method-doc"
    | where {|d| $d.metaVariables.single.M.text == $method }
    | each {|d| $d.metaVariables.single.D.text | str substring 1..-2 | str replace --all '\"' '"' | str replace --all '\\' '\' }
    | skip until {|d| $d == "" }
    | skip 1
    | str join " "
    | str replace "Permission check: " ""

  let chain = $segs | skip 1 | each {|s| if $s.param { $".($s.name)\(($s.name)\)" } else { $".($s.name)\(\)" } } | str join ""
  let top = $module | first
  let params_path = $"proxmox_api::($module | str join '::')::($struct)"
  {
    rust: ($"proxmox_api::($top)::($top | str capitalize)Client::new\(client\)($chain).($method)\(" + (if $takes_params { $"($struct) { .. }" } else { "" }) + ")")
    params: (if $takes_params { $params_path } else { null })
    fields: (if $takes_params { $fields } else { [] })
    path_params: ($segs | where param | get name)
    permission: $permission
  }
}

def endpoints [] {
  main calls
  | each {|c|
      if not ($c.key in ($endpoint_map.cli | columns)) {
        error make { msg: $"($c.file):($c.line): no endpoint mapped for `($c.key)`" }
      }
      $c | insert endpoint ($endpoint_map.cli | get $c.key)
    }
}

# Checks every pct/qm call against the proxmox-api crate and lists its replacement.
export def "main report" [--crate: path] {
  let root = $crate | default (crate-root)
  let calls = endpoints
  let api = $calls
    | where endpoint != null
    | get endpoint
    | select method path
    | uniq
    | each {|e| $e | insert api (crate-endpoint $root $e) }

  $calls | each {|c|
    if $c.endpoint == null {
      return {
        file: $c.file, line: $c.line, key: $c.key
        method: "-", path: "no API equivalent, stays CLI"
        rust: null, params: null, mapped: [], unknown_flags: [], dynamic_flags: $c.dynamic_flags
        unknown_positional: [], missing_required: [], spread: $c.spread, permission: ""
      }
    }
    let api = $api | where method == $c.endpoint.method and path == $c.endpoint.path | first | get api
    let names = $api.fields | get name
    let mapped = $c.flags | each {|f| { flag: $f, param: (schema-param $names $f) } }
    let positional = $c.endpoint.positional
      | each {|p| { arg: $p, in: (if $p in $api.path_params { "path" } else if $p in $names { "body" } else { null }) } }
    let covered = $mapped | get param | append ($positional | get arg)
    {
      file: $c.file, line: $c.line, key: $c.key
      method: $c.endpoint.method, path: $c.endpoint.path
      rust: $api.rust
      params: $api.params
      mapped: ($mapped | where param != null | each {|m| if $m.flag == $m.param { $m.flag } else { $"($m.flag)→($m.param)" } })
      unknown_flags: ($mapped | where param == null | get flag)
      dynamic_flags: $c.dynamic_flags
      unknown_positional: ($positional | where in == null | get arg)
      missing_required: ($api.fields | where required | get name | where $it not-in $covered)
      spread: $c.spread
      permission: $api.permission
    }
  }
}

def prune-nodes [nodes: list, wanted: list] {
  $nodes | each {|n|
    let kids = prune-nodes ($n.children? | default []) $wanted
    let methods = $wanted | where path == $n.path | get method | uniq
    if ($kids | is-empty) and ($methods | is-empty) { null } else {
      let base = $n | upsert info ($n.info | select ...$methods)
      if ($kids | is-empty) {
        $base | reject --optional children | upsert leaf 1
      } else {
        $base | upsert children $kids | upsert leaf 0
      }
    }
  } | compact
}

export def "main prune" [--schema: path, --out: path = "pruned-schema.json"] {
  let wanted = endpoints
    | where endpoint != null
    | get endpoint
    | append $endpoint_map.extra
    | uniq

  prune-nodes (open $schema) $wanted | to json | save --force $out
  print $"wrote ($out): ($wanted | length) endpoints"
}

export def "main generate" [--schema: path, --generator-repo: path, --crate-dir: path] {
  let schema = $schema | path expand
  let repo = $generator_repo | path expand
  let crate_dir = $crate_dir | path expand
  let work = mktemp --directory
  let pruned = $work | path join "pruned-schema.json"

  main prune --schema $schema --out $pruned

  cd ($repo | path join "generator")
  ^cargo run --quiet -- recursive $pruned ($work | path join "generated.rs")
  glob ($work | path join "**/*.rs") | each {|f| ^rustfmt --edition 2024 $f } | ignore

  if ($crate_dir | path exists) { rm --recursive $crate_dir }
  cp --recursive ($repo | path join "proxmox-api") $crate_dir
  rm --recursive ($crate_dir | path join "src/generated") ($crate_dir | path join "src/generated.rs") ($crate_dir | path join "src/specialized.rs")
  open --raw ($crate_dir | path join "src/lib.rs")
    | str replace "#[cfg(feature = \"nodes\")]\nmod specialized;\n" ""
    | save --force ($crate_dir | path join "src/lib.rs")
  cp --recursive ($work | path join "generated") ($work | path join "generated.rs") ($crate_dir | path join "src")

  rm --recursive $work
  print $"vendored pruned bindings into ($crate_dir)"
}

export def main [] {
  main calls
}
