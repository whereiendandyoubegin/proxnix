const primitives = [
  bool,
  char,
  f32,
  f64,
  i8,
  i16,
  i32,
  i64,
  i128,
  isize,
  str,
  u8,
  u16,
  u32,
  u64,
  u128,
  usize,
]

export def findprimitive [dir?: path] {
  let current_dir = $dir | default ("~/cloned/proxnix/nix-deployments-rs/" | path expand)
  ls $current_dir | each --flatten {|entry| match $entry.type {
      file => {
        if ($entry.name | str contains ".rs") {
          let content = (open $entry.name)
          return {
            name: $entry.name,
            output: ($primitives | each --flatten {|type| $content | find $type}  )
          }
        }
      }   
      dir => {
        if not ($entry.name | str contains "target") {
          findprimitive $entry.name
        }
      }
    }
  } 
}

export def main [] {
  findprimitive | where ($in | each --flatten {|e| $e | is-not-empty})
}
