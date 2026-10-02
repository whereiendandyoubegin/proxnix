const not_in_tests = {
  not: { inside: { stopBy: end, kind: mod_item, has: { field: name, regex: '^tests$' } } }
}

const leaves = [
  [id rule];
  [process { any: [
    { pattern: 'Command::new($$$)' }
    { pattern: 'std::process::Command::new($$$)' }
    { pattern: '$C.kill()' }
    { pattern: '$C.try_wait()' }
    { pattern: '$C.wait_with_output()' }
  ] }]
  [proxmox-api { pattern: '$P.call($$$)' }]
  [async-runtime { pattern: '$R.block_on($$$)' }]
  [sozu-socket { any: [
    { pattern: '$C.write_message($$$)' }
    { pattern: '$C.read_message()' }
    { pattern: 'Channel::from_path($$$)' }
  ] }]
  [clock { any: [
    { pattern: 'Instant::now()' }
    { pattern: 'SystemTime::now()' }
    { pattern: '$S.elapsed()' }
  ] }]
  [sleep { any: [
    { pattern: 'std::thread::sleep($$$)' }
    { pattern: 'thread::sleep($$$)' }
  ] }]
  [network { kind: call_expression, has: { field: function, regex: '^(std::net::)?(TcpStream|UdpSocket|TcpListener)::' } }]
  [filesystem { kind: call_expression, has: { field: function, regex: '^(std::)?(os::unix::)?fs::|OpenOptions::new' } }]
  [git { kind: call_expression, has: { field: function, regex: '^(git2::|Repository::|RepoBuilder::)' } }]
  [global-lock { pattern: '$L.lock()' }]
  [process-env { kind: call_expression, has: { field: function, regex: '^(std::)?env::' } }]
  [parallel { any: [
    { pattern: '$X.into_par_iter()' }
    { pattern: '$X.par_iter()' }
  ] }]
  [log { kind: macro_invocation, has: { field: macro, regex: '^(info|warn|debug|error|trace)$' } }]
]

const mutations = [
  [id rule];
  [let-mut { kind: let_declaration, has: { kind: mutable_specifier } }]
  [mut-self { kind: self_parameter, regex: 'mut' }]
  [mut-param { kind: parameter, has: { stopBy: end, kind: mutable_specifier } }]
  [mut-borrow { kind: reference_expression, has: { kind: mutable_specifier } }]
  [static-item { kind: static_item }]
  [interior-type { kind: type_identifier, regex: '^(Mutex|RwLock|Cell|RefCell|OnceCell|OnceLock|LazyLock|Atomic[A-Za-z0-9]+)$' }]
  [process-global { kind: call_expression, has: { field: function, regex: '^(std::)?env::(set_var|remove_var|set_current_dir)$' } }]
]

const graph = [
  [id rule];
  [fn-def { kind: function_item, has: { field: name, pattern: '$NAME' } }]
  [impl-type { kind: impl_item, has: { field: type, pattern: '$NAME' } }]
  [impl-trait { kind: impl_item, has: { field: trait, pattern: '$NAME' } }]
  [trait-def { kind: trait_item, has: { field: name, pattern: '$NAME' } }]
  [mut-method { kind: function_item, has: { field: name, pattern: '$NAME' }, all: [{ has: { field: parameters, has: { kind: self_parameter, regex: 'mut' } } }] }]
  [call { kind: call_expression, has: { field: function, pattern: '$F' } }]
  [fn-ref { any: [{ kind: scoped_identifier } { kind: identifier }], inside: { kind: arguments } }]
  [binding { kind: identifier, any: [
    { inside: { kind: parameter, field: pattern, stopBy: { kind: parameter } } }
    { inside: { kind: closure_parameters, stopBy: end } }
    { inside: { kind: let_declaration, field: pattern, stopBy: { kind: let_declaration } } }
    { inside: { kind: let_condition, field: pattern, stopBy: { kind: let_condition } } }
    { inside: { kind: match_pattern, stopBy: { kind: match_arm } } }
    { inside: { kind: for_expression, field: pattern, stopBy: { kind: for_expression } } }
  ] }]
]

def default-root [] {
  "~/cloned/proxnix/nix-deployments-rs/nix-deployments-rs/proxnix/src" | path expand
}

def rules [table: list] {
  $table
  | each {|r| { id: $r.id, language: rust, rule: { all: [$r.rule $not_in_tests] } } | to yaml }
  | str join "---\n"
}

def scan [table: list, root: path] {
  ^ast-grep scan --inline-rules (rules $table) --json=compact $root
  | from json
  | each {|m| {
      rule: $m.ruleId
      file: ($m.file | path basename)
      line: ($m.range.start.line + 1)
      start: $m.range.byteOffset.start
      end: $m.range.byteOffset.end
      text: ($m.text | lines | first | str trim)
      body: $m.text
      name: ($m | get --optional metaVariables.single.NAME.text)
      callee: ($m | get --optional metaVariables.single.F.text)
    } }
}

def type-name [text: string] {
  $text | str replace --regex '::<.*$' '' | str replace --regex '<.*$' '' | str trim | split row '::' | last
}

def callee [text: string] {
  let flat = $text | str replace --all --regex '\s*\n\s*' '' | str replace --regex '::<(?:[^<>]|<[^<>]*>)*>$' '' | str trim
  let dot = $flat | str index-of --end '.'
  let path = $flat | str index-of --end '::'
  if $dot > $path {
    { name: ($flat | str substring ($dot + 1)..), via: method, qualifier: ($flat | str substring 0..<$dot) }
  } else if $path >= 0 {
    { name: ($flat | str substring ($path + 2)..), via: path, qualifier: ($flat | str substring 0..<$path) }
  } else {
    { name: $flat, via: bare, qualifier: null }
  }
}

const std_names = [find map get filter filter_map find_map iter into_iter next collect into from clone len is_empty contains insert remove push extend and_then or_else ok err unwrap_or unwrap_or_else map_err parse split join trim as_str as_ref to_owned first last any all fold take skip chain zip try_from try_into default eq cmp hash fmt drop wait kill write read flush lines]

def generic [segment: string] {
  $segment =~ '^[A-Z][A-Za-z]?$'
}

def resolve [world, caller, target] {
  let named = $world.defs | where name == $target.name
  match $target.via {
    method => {
      let receiver = $target.qualifier
      if $receiver == self {
        $named | where {|d| ($d.owners | any {|o| $o in $caller.owners }) }
      } else if $receiver in $world.owner_names {
        let as = $world.traits | where type == $receiver | get trait | append $receiver
        $named | where {|d| ($d.owners | any {|o| $o in $as }) }
      } else if $target.name in $std_names {
        []
      } else {
        $named | where assoc
      }
    }
    bare => (if $target.name in $caller.bound { [] } else { $named | where not assoc })
    _ => {
      let q = $target.qualifier
      if ($q =~ '\sas\s') {
        let tr = $q | parse --regex '\sas\s+([A-Za-z_][A-Za-z0-9_:]*)' | get 0.capture0 | split row '::' | last
        $named | where {|d| $tr in $d.owners }
      } else {
        let first = $q | str replace --regex '^<' '' | split row '::' | first | type-name $in
        let segment = type-name $q
        if $segment == Self {
          $named | where {|d| ($d.owners | any {|o| $o in $caller.owners }) }
        } else if (generic $first) {
          if $target.name in $std_names { [] } else { $named | where assoc }
        } else if $segment in $world.owner_names {
          $named | where {|d| $segment in $d.owners }
        } else if $segment in $world.modules {
          $named | where {|d| $d.file == $"($segment).rs" and not $d.assoc }
        } else {
          []
        }
      }
    }
  }
  | get key
}

def enclosing [defs: list, m] {
  $defs
  | where file == $m.file and start <= $m.start and end >= $m.end
  | sort-by {|d| $d.end - $d.start }
  | get --optional 0
}

def build [root: path] {
  let found = scan $graph $root
  let blocks = $found | where rule in [impl-type impl-trait trait-def] | each {|b| $b | update name (type-name $b.name) }
  let bindings = $found | where rule == binding
  let bare_defs = $found
    | where rule == fn-def
    | each {|d|
        let owners = $blocks | where file == $d.file and start <= $d.start and end >= $d.end | get name | uniq
        let owner = $owners | first --strict 1 | get --optional 0 | default ""
        let label = if $owner == "" { $d.name } else { $"($owner)::($d.name)" }
        { key: $"($d.file):($d.line):($label)", name: $d.name, file: $d.file, line: $d.line, start: $d.start, end: $d.end, owners: $owners, assoc: (($owners | length) > 0) }
      }
  let defs = $bare_defs | each {|d|
    let inner = $bindings | where file == $d.file and start >= $d.start and end <= $d.end
    $d | insert bound ($inner | where {|b| (enclosing $bare_defs $b | get key) == $d.key } | get text | uniq)
  }
  let types = $found | where rule == impl-type
  let world = {
    traits: ($found | where rule == impl-trait | each {|t| { trait: (type-name $t.name), type: ($types | where file == $t.file and start == $t.start | get --optional 0.name | default "" | type-name $in) } })
    defs: $defs
    owner_names: ($blocks | get name | uniq)
    modules: ($defs | get file | uniq | each {|f| $f | str replace '.rs' '' })
  }
  let edges = $found
    | where rule in [call fn-ref]
    | each {|c|
        let target = if $c.rule == call { callee $c.callee } else { callee $c.text }
        let from = enclosing $defs $c
        if $from == null { [] } else {
          resolve $world $from $target | each {|to| { from: $from.key, to: $to, line: $c.line } }
        }
      }
    | flatten
    | where from != to
  let hits = scan $leaves $root
    | each {|h| $h | insert key (enclosing $defs $h | get --optional key) }
    | where key != null
  { defs: $defs, edges: $edges, hits: $hits, mut_methods: ($found | where rule == mut-method | get name | uniq) }
}

def propagate [world] {
  let direct = $world.hits
    | group-by key
    | items {|key, hs| { key: $key, reach: ($hs | get rule | uniq | each {|r| { effect: $r, via: null } }) } }
  let callees = $world.edges | group-by from | items {|k, es| { key: $k, to: ($es | get to | uniq) } }
  mut reach = $world.defs | each {|d| { key: $d.key, reach: ($direct | where key == $d.key | get --optional 0.reach | default []) } }
  mut changed = true
  while $changed {
    let before = $reach
    let index = $before | reduce --fold {} {|r, acc| $acc | insert $r.key $r.reach }
    let next = $before | each {|r|
      let known = $r.reach | get effect
      let inherited = $callees
        | where key == $r.key
        | get --optional 0.to
        | default []
        | each {|to| $index | get $to | each {|e| { effect: $e.effect, via: $to } } }
        | flatten
        | where effect not-in $known
        | uniq-by effect
      { key: $r.key, reach: ($r.reach | append $inherited) }
    }
    $changed = ($next | get reach | each {|x| $x | length } | math sum) != ($before | get reach | each {|x| $x | length } | math sum)
    $reach = $next
  }
  $reach
}

def short [key: string] {
  $key | split row --number 3 ':' | $"($in.2) \(($in.0):($in.1)\)"
}

const macros = [
  [id rule];
  [macro { kind: macro_invocation }]
]

export def "main blind" [file: string = "deployments.rs", root?: path] {
  let root = $root | default (default-root)
  let world = build $root
  let reach = propagate $world
  let effectful = $reach | where {|r| ($r.reach | where effect != log | is-not-empty) } | get key
  let effectful_names = $world.defs | where key in $effectful | get name | uniq
  let shadowed = $world.defs
    | where key in $effectful and name in $std_names
    | each {|d| { blind_spot: std-named, line: $d.line, text: (short $d.key) } }
  let hidden = scan $macros $root
    | where file == $file
    | each {|m|
        let inner = $m.body
          | str replace --regex '^[a-z_]+!' ''
          | parse --regex '([A-Za-z_][A-Za-z0-9_]*)\s*\('
          | get capture0
          | where $it in $effectful_names
          | uniq
        if ($inner | is-empty) { [] } else { [{ blind_spot: in-macro, line: $m.line, text: ($inner | str join ", ") }] }
      }
    | flatten
  $shadowed | append $hidden
}

export def "main leaves" [file: string = "deployments.rs", root?: path] {
  scan $leaves ($root | default (default-root)) | where file == $file | select rule line text
}

export def "main mutation" [file: string = "deployments.rs", root?: path] {
  let root = $root | default (default-root)
  let world = build $root
  let calls = scan $graph $root
    | where rule == call and file == $file
    | each {|c| $c | insert target (callee $c.callee) }
    | where target.via == method and target.name in $world.mut_methods
    | each {|c| { rule: mut-method-call, line: $c.line, text: $c.callee } }
  scan $mutations $root
  | where file == $file
  | select rule line text
  | append $calls
  | sort-by line
}

export def "main chain" [function: string, effect: string, root?: path] {
  let world = build ($root | default (default-root))
  let reach = propagate $world
  let index = $reach | reduce --fold {} {|r, acc| $acc | insert $r.key $r.reach }
  let start = $world.defs | where name == $function | get key
  $start | each {|key|
    mut path = [(short $key)]
    mut at = $key
    mut steps = 0
    while $at != null and $steps < 64 {
      let hop = $index | get $at | where effect == $effect | get --optional 0
      if $hop == null { $at = null; $path = ($path | append "(does not reach it)"); break }
      if $hop.via == null {
        let lines = $world.hits | where key == $at and rule == $effect | get line | str join ","
        $path = ($path | append $"direct at line ($lines)")
        $at = null
      } else {
        $path = ($path | append (short $hop.via))
        $at = $hop.via
      }
      $steps += 1
    }
    $path | str join " -> "
  }
}

export def main [file: string = "deployments.rs", root?: path] {
  let world = build ($root | default (default-root))
  let reach = propagate $world
  $world.defs
  | where file == $file
  | each {|d|
      let r = $reach | where key == $d.key | get 0.reach
      {
        function: $d.name
        line: $d.line
        direct: ($r | where via == null | get effect | sort)
        inherited: ($r | where via != null | each {|e| $"($e.effect) via (short $e.via | split row ' ' | first)" } | sort)
      }
    }
}
