const legacy_rules = '
id: legacy-command
language: rust
rule:
  pattern: Command::new($TOOL)
constraints:
  TOOL:
    regex: ^"(qm|pct|lxc-info)"$
---
id: legacy-cli-fn-call
language: rust
rule:
  kind: call_expression
  has:
    field: function
    regex: ^((crate::)?(qm|pct)::)?(qm|pct)_[a-z_]+$
---
id: legacy-cli-fn-def
language: rust
rule:
  kind: function_item
  has:
    field: name
    regex: ^(qm|pct)_[a-z_]+$
---
id: legacy-wrapper-call
language: rust
rule:
  any:
    - pattern: $T::$M($$$)
    - pattern: $R.apply_in_place($$$)
constraints:
  T:
    regex: ^T$
  M:
    regex: ^(stop|destroy|start|set_tags|set_protection|get_ip|exists|tags|load_deployed|guest_check|requires_rebuild)$
---
id: legacy-module
language: rust
rule:
  any:
    - pattern: mod $M;
    - kind: use_declaration
      regex: crate::(qm|pct)\b
constraints:
  M:
    regex: ^(qm|pct)$
---
id: legacy-type
language: rust
rule:
  kind: type_identifier
  regex: ^(DeployedVM|DeployedContainer|DeployedState|ContainerFieldChange|PctArgs|PctListEntry)$
---
id: untyped-guest-id
language: rust
rule:
  any:
    - kind: parameter
      has:
        field: pattern
        regex: ^(id|vm_id|ct_id|deployed_id|vmid|target_id)$
    - kind: field_declaration
      has:
        field: name
        regex: ^(id|vm_id|ct_id|deployed_id|vmid|target_id)$
  has:
    field: type
    regex: ^&?u32$
  not:
    inside:
      stopBy: end
      kind: impl_item
      has:
        field: type
        regex: ^Vmid$
'

const test_rule = '
id: test-module
language: rust
rule:
  kind: mod_item
  has:
    field: name
    regex: ^tests$
  follows:
    pattern: "#[cfg(test)]"
'

const current_rules = '
id: exec
language: rust
rule:
  pattern: Command::new($TOOL)
---
id: exec-statement
language: rust
rule:
  any:
    - kind: let_declaration
    - kind: expression_statement
  has:
    stopBy: end
    pattern: Command::new($TOOL)
---
id: read
language: rust
rule:
  pattern: stdout($TOOL, $ARGS)
---
id: api-read
language: rust
rule:
  pattern: $PVE.call($REQ)
---
id: op
language: rust
rule:
  any:
    - pattern: GuestOp::<$K>::$OP($$$)
    - pattern: GuestOp::<$K>::$OP
  not:
    inside:
      kind: call_expression
      has:
        field: function
        pattern: GuestOp::<$K>::$OP
---
id: tool-const
language: rust
rule:
  pattern: "const TOOL: &\x27static str = $VALUE;"
  inside:
    stopBy: end
    kind: impl_item
    has:
      field: type
      pattern: $IMPL
---
id: enclosing-impl
language: rust
rule:
  kind: impl_item
  has:
    field: type
    pattern: $IMPL
---
id: verb-words
language: rust
rule:
  kind: match_arm
  all:
    - has:
        field: pattern
        has:
          pattern: Verb::$V
    - has:
        field: value
        pattern: $BODY
  inside:
    stopBy: end
    kind: impl_item
    has:
      field: type
      pattern: $IMPL
---
id: constructor
language: rust
rule:
  kind: function_item
  has:
    field: name
    pattern: $F
  inside:
    stopBy: end
    kind: impl_item
    has:
      field: type
      regex: ^GuestOp
---
id: constructor-call
language: rust
rule:
  any:
    - pattern: GuestOp::$V($$$)
    - pattern: Self::$V($$$)
'

def default-root [] {
  "~/cloned/proxnix/nix-deployments-rs/nix-deployments-rs" | path expand
}

def span [m] {
  { start: $m.range.byteOffset.start, end: $m.range.byteOffset.end }
}

def within [inner, outer] {
  $inner.file == $outer.file and (span $outer).start <= (span $inner).start and (span $outer).end >= (span $inner).end
}

def meta [m, name: string] {
  $m.metaVariables.single | get --optional $name | get --optional text
}

def scan [rules: string, root: path] {
  ^ast-grep scan --inline-rules $rules --json=compact $root | from json
}

def outside-tests [root: path] {
  let tests = scan $test_rule $root
  {|m| not ($tests | any {|t| within $m $t }) }
}

export def "main legacy" [dir?: path, --include-tests] {
  let root = $dir | default (default-root) | path expand
  let tests = scan $test_rule $root
  scan $legacy_rules $root
  | each {|m| {
      rule: $m.ruleId
      file: ($m.file | path relative-to $root)
      line: ($m.range.start.line + 1)
      in_tests: ($tests | any {|t| within $m $t })
      text: ($m.lines | lines | first | str trim)
    } }
  | where {|hit| $include_tests or not $hit.in_tests }
}

def expand-constructor [name: string, ctors: list, calls: list] {
  let ctor = $ctors | where {|c| (meta $c F) == $name } | get --optional 0
  if $ctor == null { return [$name] }
  $calls
  | where {|c| within $c $ctor }
  | sort-by {|c| (span $c).start }
  | each {|c|
      let target = meta $c V
      if ($target | str substring 0..0) =~ "[A-Z]" { [$target] } else { expand-constructor $target $ctors $calls }
    }
  | flatten
}

def words-for [kind: string, verb: string, arms: list] {
  let arm = $arms | where {|a| (meta $a V) == $verb and (meta $a IMPL) == $kind } | get --optional 0
    | default ($arms | where {|a| (meta $a V) == $verb and (meta $a IMPL) == "Verb" } | get --optional 0)
  if $arm == null { [($verb | str lowercase)] } else { meta $arm BODY | parse --regex '"(?<w>[^"]+)"' | get w }
}

def enclosing-impl [m, impls: list] {
  $impls
  | where {|i| within $m $i }
  | sort-by {|i| (span $i).end - (span $i).start }
  | get --optional 0
  | if $in == null { null } else { meta $in IMPL }
}

def kinds-of [expr: string, m, impls: list, tools: record] {
  let owner = $expr | str replace --regex '::TOOL$' ''
  match $expr {
    $e if ($e | str starts-with "\"") => [{ kind: null, tool: ($e | str trim --char "\"") }]
    "Self::TOOL" => {
      let kind = enclosing-impl $m $impls
      [{ kind: $kind, tool: ($tools | get --optional $kind | default "?") }]
    }
    $e if ($owner in $tools) => [{ kind: $owner, tool: ($tools | get $owner) }]
    $e if ($e | str ends-with "::TOOL") or $e == "invocation.program" or $e =~ "^(T::)?Kind$" or $e == "K" => ($tools | transpose kind tool)
    $e => [{ kind: null, tool: $"<dynamic ($e)>" }]
  }
}

export def "main current" [dir?: path, --include-tests] {
  let root = $dir | default (default-root) | path expand
  let keep = if $include_tests { {|m| true } } else { outside-tests $root }
  let matches = scan $current_rules $root
  let impls = $matches | where ruleId == "enclosing-impl"
  let statements = $matches | where ruleId == "exec-statement"
  let tools = $matches
    | where ruleId == "tool-const"
    | reduce --fold {} {|c, acc| $acc | upsert (meta $c IMPL) (meta $c VALUE | str trim --char "\"") }
  let arms = $matches | where ruleId == "verb-words"
  let ctors = $matches | where ruleId == "constructor"
  let calls = $matches | where ruleId == "constructor-call"
  let row = {|m, via, key| {
      file: ($m.file | path relative-to $root)
      line: ($m.range.start.line + 1)
      via: $via
      key: $key
    } }

  let execs = $matches | where ruleId == "exec" | where $keep | each {|m|
    kinds-of (meta $m TOOL) $m $impls $tools
    | each {|k|
        let rest = if (meta $m TOOL) == "invocation.program" { "<any GuestOp>" } else {
          $statements
          | where {|s| within $m $s }
          | sort-by {|s| (span $s).end - (span $s).start }
          | get --optional 0.text
          | default $m.text
          | parse --regex '\.arg\("(?<a>[^"]+)"\)'
          | get --optional 0.a
          | default ""
        }
        do $row $m $"exec (meta $m TOOL)" ([$k.tool $rest] | str join " " | str trim)
      }
  } | flatten
  let reads = $matches | where ruleId == "read" | where $keep | each {|m|
    let words = meta $m ARGS | parse --regex '"(?<w>[^"]+)"' | get w | where {|w| not ($w | str starts-with "-") }
    kinds-of (meta $m TOOL) $m $impls $tools
    | each {|k| do $row $m "read" ([$k.tool] | append $words | str join " ") }
  } | flatten
  let api_reads = $matches | where ruleId == "api-read" | where $keep | each {|m|
    let calls = meta $m REQ | parse --regex '\.(?<name>[a-z_]+)\(' | get name | where $it != "node"
    let path = $calls | drop 1 | each {|c| if $c == "vmid" { "{vmid}" } else { $c | str replace --all "_" "-" } } | str join "/"
    do $row $m "api" $"($calls | last | str uppercase) /nodes/{node}/($path)"
  }
  let ops = $matches | where ruleId == "op" | where $keep | each {|m|
    let op = meta $m OP
    let variants = expand-constructor $op $ctors $calls
    kinds-of (meta $m K) $m $impls $tools
    | each {|k|
        $variants | each {|v| do $row $m $"GuestOp::($op)" ([$k.tool] | append (words-for $k.kind $v $arms) | str join " ") }
      }
    | flatten
  } | flatten

  $execs | append $reads | append $api_reads | append $ops | sort-by file line key
}

export def "main prove" [] {
  let repo = ^git -C (default-root) rev-parse --show-toplevel | str trim
  let snapshot = mktemp --directory
  ^git -C $repo archive HEAD nix-deployments-rs/proxnix/src nix-deployments-rs/proxnix-core/src
  | ^tar -x -C $snapshot
  let before = main legacy ($snapshot | path join nix-deployments-rs)
  rm --recursive $snapshot
  let after = main legacy
  let skipped = main legacy --include-tests | where in_tests

  let ids = $legacy_rules | parse --regex '(?m)^id: (?<id>\S+)$' | get id
  let summary = $ids | each {|id| {
    rule: $id
    head: ($before | where rule == $id | length)
    working_tree: ($after | where rule == $id | length)
  } }
  print ($summary | table)

  let pve = main current | where {|s| $s.key =~ "^(qm|pct|lxc-info|GET|POST|PUT|DELETE)\\b" }
  print ($pve | group-by key | transpose key sites | each {|g| {
    key: $g.key
    sites: ($g.sites | each {|s| $"($s.file | path basename):($s.line)" } | uniq | str join " ")
  } } | sort-by key | table)

  let vacuous = $summary | where head == 0
  if ($vacuous | is-not-empty) {
    error make { msg: $"rules that match nothing at HEAD prove nothing: ($vacuous | get rule | str join ', ')" }
  }
  if ($after | is-not-empty) {
    print ($after | table)
    error make { msg: $"($after | length) legacy call sites remain" }
  }
  print $"no legacy call sites remain outside tests \(HEAD had ($before | length); ($skipped | length) test-only matches skipped, see `main legacy --include-tests`\)"
}

export def main [dir?: path] {
  main current $dir
}
