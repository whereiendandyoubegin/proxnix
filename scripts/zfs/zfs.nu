def exists [name: string] {
    (do { ^zfs list -H -o name $name } | complete).exit_code == 0
}

def mount-of [dataset: string] {
    ^zfs get -H -o value mountpoint $dataset | str trim
}

def has-file [dataset: string, name: string] {
    mount-of $dataset | path join $name | path exists
}

def origins [root: string] {
    ^zfs get -H -o name,value -t filesystem origin -r $root | lines | split column "\t" name origin
}

let root = "ZFS/scratch"
let blue = $"($root)/blue"
let green = $"($root)/green"

if (exists $root) {
    ^zfs destroy -R $root
}

^zfs create $root
^zfs create $blue

let t_start = timeit { ^zfs snapshot $"($blue)@start" }
let t_seed = timeit { ^zfs clone $"($blue)@start" $green }

"rehearsal" | save (mount-of $green | path join note)
"written after start" | save (mount-of $blue | path join late)

let rehearsal_isolated = not (has-file $blue note)
let green_frozen_at_start = not (has-file $green late)

let t_discard = timeit { ^zfs destroy $green }
let t_final = timeit { ^zfs snapshot $"($blue)@final" }
let t_transfer = timeit { ^zfs clone $"($blue)@final" $green }

let green_has_final_data = has-file $green late
let green_lost_rehearsal = not (has-file $green note)

let early_destroy = do { ^zfs destroy -r $blue } | complete

let t_promote = timeit { ^zfs promote $green }

let snapshots_after_promote = ^zfs list -H -o name -t snapshot -r $root | lines
let origins_after_promote = origins $root

let t_drop = timeit { ^zfs destroy -r $blue }

let leftover_datasets = ^zfs list -H -o name -t filesystem -r $root | lines
let leftover_snapshots = ^zfs list -H -o name -t snapshot -r $root | lines

print ([
    { step: "snapshot @start", took: $t_start }
    { step: "clone @start (seed)", took: $t_seed }
    { step: "destroy rehearsal clone", took: $t_discard }
    { step: "snapshot @final", took: $t_final }
    { step: "clone @final (transfer)", took: $t_transfer }
    { step: "promote green", took: $t_promote }
    { step: "destroy blue", took: $t_drop }
] | table)

print ({
    rehearsal_isolated: $rehearsal_isolated
    green_frozen_at_start: $green_frozen_at_start
    green_has_final_data: $green_has_final_data
    green_lost_rehearsal: $green_lost_rehearsal
    early_destroy_refused: ($early_destroy.exit_code != 0)
    early_destroy_error: ($early_destroy.stderr | str trim)
    snapshots_after_promote: $snapshots_after_promote
    origins_after_promote: $origins_after_promote
    leftover_datasets: $leftover_datasets
    leftover_snapshots: $leftover_snapshots
} | table -e)
