#!/usr/bin/env nu
# bare-worktree: manage a bare-repo + sibling-worktrees layout with profiles.
#
# The layout this script creates and expects:
#
#   proj/                  <- "root": the container
#   |- .bare/              <- the one real git dir (bare)
#   |- .git                <- file "gitdir: ./.bare", so git works from root
#   |- .claude/            <- optional; symlinked into every worktree on apply
#   |- .profiles/          <- the gitignored files each worktree needs
#   |  |- .state/          <- one manifest per worktree: what apply did
#   |  |- dflt/            <- default profile, always applied first
#   |  |- <name>/          <- any other dir = a named profile
#   |- <worktree>/         <- one sibling dir per branch
#
# Every file in a profile dir is symlinked into the worktree at the same
# relative path. An optional profile.yaml (never propagated) overrides entries
# and declares hooks:
#
#   entries:
#     - source: .env.local      # relative to the profile dir, or absolute
#       type: copy              # symlink (default) | copy | none (exclude)
#       override: true          # false: never replace something already there
#       target: .env.local      # required only when source is absolute
#   hooks:
#     before-apply:             # one record or a list of them
#       - command: scripts/seed.nu   # relative -> resolved in the profile dir
#         args: ["--fast"]
#     after-apply: ...          # other points: after-add (worktree stands,
#                               # profiles applied), before-remove and
#                               # before-discard (entries still exist),
#                               # after-init (dflt only, once per transform).
#                               # cwd = the worktree; BW_ROOT, BW_WORKTREE,
#                               # BW_PROFILE in the environment.
#
# Profiles apply in order, dflt always first. A later profile's entry replaces
# an earlier one's unless it says override: false; type: none removes the
# target from the final plan entirely. Git-tracked files are never overwritten.
# Root .claude, when present and untracked, is always symlinked unless a
# profile overrides the ".claude" target. A bare `apply` refreshes the set the
# state manifest records; --profiles changes the set, --reset returns to dflt.
#
# Standalone by design — no imports from the ctx-flow hooks — so it can later
# move into a nushell config unchanged.

const PROFILES = ".profiles"
const DFLT = "dflt"
const STATE = ".state"
const JUNK = [node_modules .venv venv target dist build out .next .cache __pycache__ .pytest_cache .DS_Store]

# Lifecycle points a profile.yaml `hooks:` block may declare. apply runs the
# apply pair; add runs after-add once the worktree stands; remove/discard run
# their "before" while the entries still exist; after-init fires once, from
# dflt only, when a repo transform completes.
const PHASES = ["before-apply" "after-apply" "after-add" "before-remove" "before-discard" "after-init"]

def fail [msg: string] { error make --unspanned { msg: $"bare-worktree: ($msg)" } }
def warn [msg: string] { print $"(ansi yellow)warn(ansi reset) ($msg)" }
def note [msg: string] { print $"  ($msg)" }

# Trimmed stdout, or an error carrying git's stderr.
def run-git [dir: string, ...args: string]: nothing -> string {
    let r = ^git -C $dir ...$args | complete
    if $r.exit_code != 0 { fail $"git ($args | str join ' '): ($r.stderr | str trim)" }
    $r.stdout | str trim
}

# Trimmed stdout, or null on failure — for probes where failing is an answer.
def try-git [dir: string, ...args: string]: nothing -> any {
    let r = try { ^git -C $dir ...$args | complete }
    if ($r.exit_code? | default 1) == 0 { $r.stdout | str trim } else { null }
}

# Path type that treats "missing" as null rather than as an error.
def ptype [p: string]: nothing -> any { try { $p | path type } }

def slug [s: string]: nothing -> string {
    $s | str replace -ra '[^A-Za-z0-9._-]+' "-" | str trim --char "-" | str substring ..<80
}

# Relative path from inside `from_dir` to `to` — what a portable symlink stores.
def rel-path [from_dir: string, to: string]: nothing -> string {
    let f = $from_dir | path expand -n | path split
    let t = $to | path expand -n | path split
    let max = [($f | length) ($t | length)] | math min
    mut i = 0
    while $i < $max and ($f | get $i) == ($t | get $i) { $i = $i + 1 }
    let ups = if (($f | length) - $i) == 0 { [] } else { 1..(($f | length) - $i) | each {|| ".."} }
    $ups | append ($t | skip $i) | path join
}

# --- context -----------------------------------------------------------------
#
# {root, worktree} — worktree is null when the cwd is the container itself (or
# anything under it that is not a checkout). Git discovers the .git pointer
# file by walking up, so this works from any depth.

def ctx []: nothing -> record {
    let cwd = $env.PWD
    let common = try-git $cwd rev-parse "--git-common-dir"
    if $common != null {
        let abs = (if ($common starts-with "/") { $common } else { $cwd | path join $common })
            | path expand -n
        if ($abs | path basename) == ".bare" {
            let wt = if (try-git $cwd rev-parse "--is-inside-work-tree") == "true" {
                try-git $cwd rev-parse "--show-toplevel"
            } else { null }
            return { root: ($abs | path dirname), worktree: $wt }
        }
    }
    if ($cwd | path join ".bare" | path type) == "dir" { return { root: $cwd, worktree: null } }
    fail "not inside a bare-worktree layout (no .bare found here or above) — see `bare-worktree init`"
}

# A worktree named on the CLI (--to/--on/remove) resolved and verified: linked
# worktrees carry a .git *file*, which is what separates them from stray dirs.
def named-worktree [root: string, name: string]: nothing -> string {
    let p = $root | path join $name
    if ($p | path join ".git" | path type) != "file" {
        fail $"($name) is not a worktree under ($root)"
    }
    $p
}

def wt-name [root: string, wt: string]: nothing -> string {
    $wt | path expand -n | path relative-to ($root | path expand -n)
}

def split-profiles [s: string]: nothing -> list<string> {
    $s | split row "," | each { str trim } | where ($it | is-not-empty)
}

# --- profiles ----------------------------------------------------------------

def profiles-root [root: string]: nothing -> string { $root | path join $PROFILES }

def list-profiles [root: string]: nothing -> list<string> {
    let pr = profiles-root $root
    if ($pr | path type) != "dir" { return [] }
    ls -a $pr | where type == dir | get name | path basename | where not ($it starts-with ".")
}

# profile.yaml allows one hook record or a list of them.
def as-list [v: any]: nothing -> list {
    if ($v | describe) starts-with "record" { [$v] } else { $v }
}

# One profile fully resolved: auto-scanned entries merged with profile.yaml.
# Config entries win over scanned ones for the same target; each entry is
# {source (abs), target (rel), type, override}.
def load-profile [root: string, name: string]: nothing -> record {
    let dir = profiles-root $root | path join $name
    if ($dir | path type) != "dir" { fail $"no profile '($name)' in (profiles-root $root)" }

    let cfg_file = $dir | path join "profile.yaml"
    let cfg = if ($cfg_file | path type) == "file" { open $cfg_file } else { {} }
    let hooks = $PHASES | reduce -f {} {|ph, acc|
        $acc | insert $ph (as-list ($cfg.hooks? | default {} | get -o $ph | default []))
    }

    # profile machinery never propagates: profile.yaml, and any hook script
    # that lives inside the profile dir
    let machinery = $PHASES | each {|ph| $hooks | get $ph } | flatten
        | each {|h| $h.command? } | compact
        | each {|c| $dir | path join $c } | where (ptype $it) == "file"
        | append $cfg_file

    let scanned = glob ($dir | path join "**" "*") --no-dir --no-symlink
        | append (glob ($dir | path join "**" "*") --no-dir --no-file)  # symlinked sources stay symlinks' targets
        | uniq
        | where $it not-in $machinery
        | each {|f| { source: $f, target: ($f | path relative-to $dir), type: "symlink", override: true } }

    let declared = $cfg.entries? | default [] | each {|e|
        let src = $e.source?
        if $src == null { fail $"($name)/profile.yaml: entry without source" }
        let abs = ($src starts-with "/") or ($src =~ '^[A-Za-z]:[\\/]')
        if $abs and $e.target? == null {
            fail $"($name)/profile.yaml: absolute source ($src) needs an explicit target"
        }
        let target = $e.target? | default $src
        {
            source: (if $abs { $src } else { $dir | path join $src })
            target: $target
            type: ($e.type? | default "symlink")
            override: ($e.override? | default true)
        }
    }

    # declared beats scanned per target; scanned rows not mentioned pass through
    let mentioned = $declared | get target
    {
        name: $name
        dir: $dir
        entries: (($scanned | where target not-in $mentioned) | append $declared)
        hooks: $hooks
    }
}

# Fold profiles (in order) into the final target -> entry plan. Later profiles
# replace earlier entries unless override: false; type none drops the target.
# The implicit root-.claude symlink seeds the plan so any profile can override
# or exclude it like a normal entry.
def build-plan [root: string, wt: string, profs: list, tracked: list<string>]: nothing -> list {
    let root_claude = $root | path join ".claude"
    mut plan = {}
    let claude_tracked = $tracked | any {|f| $f == ".claude" or ($f starts-with ".claude/") }
    if ($root_claude | path type) == "dir" and not $claude_tracked {
        $plan = { ".claude": { source: $root_claude, target: ".claude", type: "symlink", override: true, profile: "(root)" } }
    }

    for p in $profs {
        for e in $p.entries {
            let t = $e.target
            if $e.type == "none" {
                $plan = ($plan | reject -o $t)
            } else if ($plan | get -o $t) != null and not $e.override {
                # an earlier profile placed it and this entry defers
            } else if ($plan | get -o $t) == null and not $e.override and ($wt | path join $t | path exists -n) {
                # something is already in the worktree and this entry defers
            } else {
                $plan = ($plan | upsert $t ($e | insert profile $p.name))
            }
        }
    }
    $plan | values
}

# --- hooks -------------------------------------------------------------------

def run-hooks [profs: list, phase: string, root: string, wt: string] {
    for p in $profs {
        for h in ($p.hooks | get -o $phase | default []) {
            let cmd = $h.command?
            if $cmd == null { fail $"($p.name)/profile.yaml: ($phase) hook without command" }
            let local = $p.dir | path join $cmd
            let resolved = if (ptype $local) == "file" { $local } else { $cmd }
            let args = $h.args? | default [] | each { into string }
            note $"hook [($p.name)] ($phase): ($cmd)"
            let r = do {
                cd $wt
                with-env { BW_ROOT: $root, BW_WORKTREE: $wt, BW_PROFILE: $p.name } {
                    if ($resolved | str ends-with ".nu") { ^nu $resolved ...$args | complete
                    } else { ^$resolved ...$args | complete }
                }
            }
            if $r.exit_code != 0 {
                fail $"hook ($cmd) \(($p.name), ($phase)) failed:\n($r.stderr | str trim)"
            }
        }
    }
}

# --- materialize / discard ---------------------------------------------------

def state-file [root: string, wtname: string]: nothing -> string {
    profiles-root $root | path join $STATE $"((slug $wtname)).nuon"
}

def load-state [root: string, wtname: string]: nothing -> any {
    let f = state-file $root $wtname
    if ($f | path type) == "file" { open $f } else { null }
}

# The recorded set's profiles, loaded for their hooks — silently skipping any
# deleted since the apply, so teardown never fails on a missing profile.
def state-profiles [root: string, st: record]: nothing -> list {
    $st.applied | where $it in (list-profiles $root) | each {|n| load-profile $root $n }
}

# Undo recorded entries. Symlinks go unconditionally (they carry no edits);
# copies only when their content still matches the source — a diverged copy is
# the user's work now. Returns targets it refused to delete.
def discard-entries [wt: string, entries: list]: nothing -> list<string> {
    mut kept = []
    for e in $entries {
        let t = $wt | path join $e.target
        let kind = try { $t | path type }
        if $kind == null { continue }
        if $e.type == "symlink" {
            if $kind == "symlink" { rm $t }
        } else if $kind == "file" and ($e.source | path type) == "file" {
            if (open --raw $t | hash sha256) == (open --raw $e.source | hash sha256) { rm $t
            } else { $kept = ($kept | append $e.target) }
        } else {
            $kept = ($kept | append $e.target)
        }
    }
    # sweep dirs the entries may have created, deepest first, empties only
    glob ($wt | path join "**") --no-file --no-symlink --exclude [".git/**" ".git"] | where $it != $wt
        | sort-by { $in | path split | length } --reverse
        | where (ls -a $it | is-empty)
        | each {|d| rm $d }
    $kept
}

def materialize [root: string, wt: string, plan: list, tracked: list<string>]: nothing -> list {
    mut done = []
    for e in $plan {
        let target = $wt | path join $e.target
        if $e.target in $tracked {
            warn $"($e.target): git-tracked, never overwritten — skipped \(profile ($e.profile))"
            continue
        }
        let existing = try { $target | path type }
        if $existing == "dir" and ($e.source | path type) != "dir" {
            warn $"($e.target): a real directory is in the way — skipped"
            continue
        }
        mkdir ($target | path dirname)
        if $existing != null { rm -rf $target }
        if $e.type == "symlink" {
            ^ln -s (rel-path ($target | path dirname) $e.source) $target
        } else if $e.type == "copy" {
            if ($e.source | path type) == "dir" { cp -r $e.source $target } else { cp $e.source $target }
        } else {
            fail $"unknown entry type '($e.type)' for ($e.target)"
        }
        $done = ($done | append { target: $e.target, type: $e.type, source: $e.source, profile: $e.profile })
    }
    $done
}

# dflt plus the requested extras, in order — erroring on names that don't exist.
def resolve-names [root: string, extra: list<string>]: nothing -> list<string> {
    let available = list-profiles $root
    for p in $extra { if $p not-in $available { fail $"no profile '($p)' — available: ($available | str join ', ')" } }
    if $DFLT in $available { [$DFLT] } else { [] } | append ($extra | where $it != $DFLT)
}

# The whole apply pipeline for one worktree — shared by `apply` and `add`.
# `names` is the final ordered profile list, dflt included.
def do-apply [root: string, wt: string, names: list<string>] {
    let profs = $names | each {|n| load-profile $root $n }

    let tracked = run-git $wt ls-files | lines
    let plan = build-plan $root $wt $profs $tracked
    let wtname = wt-name $root $wt

    # entries from a previous apply that the new plan no longer produces would
    # linger as stale symlinks — retire them first
    let old = load-state $root $wtname
    if $old != null {
        let stale = $old.entries | where target not-in ($plan | get target)
        if ($stale | is-not-empty) { discard-entries $wt $stale | ignore }
    }

    run-hooks $profs "before-apply" $root $wt
    let done = materialize $root $wt $plan $tracked
    run-hooks $profs "after-apply" $root $wt

    # a placed file that git does not ignore shows up as untracked noise —
    # commonly because a dir pattern like `.claude/` cannot match a symlink
    if ($done | is-not-empty) {
        let ignored = ^git -C $wt check-ignore "--" ...($done | get target) | complete | get stdout | lines
        for t in ($done | get target | where $it not-in $ignored) {
            warn $"($t): not gitignored — will show as untracked \(a dir pattern like '($t)/' does not match a symlink; ignore '($t)' instead)"
        }
    }

    mkdir (profiles-root $root | path join $STATE)
    { worktree: $wtname, applied: $names, at: (date now | format date "%Y-%m-%dT%H:%M:%S%z"), entries: $done }
        | to nuon -i 2 | save -f (state-file $root $wtname)

    print $"applied [($names | str join ', ')] to ($wtname) — ($done | length) entries"
    if ($done | is-not-empty) { print ($done | select target type profile | table -i false) }
}

# --- subcommands -------------------------------------------------------------

# Apply profiles to a worktree. Bare `apply` refreshes the recorded set —
# whatever the last apply put there; naming --profiles changes the set; --reset
# returns to dflt-only.
def "main apply" [
    --profiles (-p): string = ""  # comma-separated, applied in order after dflt
    --to: string = ""             # target worktree name; only from the root
    --reset                       # drop the recorded set, back to dflt-only
] {
    let c = ctx
    let wt = if ($to | is-not-empty) {
        if $c.worktree != null { fail "--to only makes sense from the root; inside a worktree just run apply" }
        named-worktree $c.root $to
    } else {
        if $c.worktree == null { fail "in the root — name a target with --to <worktree>" }
        $c.worktree
    }
    let extra = split-profiles $profiles
    let recorded = load-state $c.root (wt-name $c.root $wt)
    let names = if ($extra | is-not-empty) or $reset or $recorded == null {
        resolve-names $c.root $extra
    } else {
        # refresh what is already applied; a profile deleted since then is
        # dropped with a warning rather than failing the whole refresh
        let available = list-profiles $c.root
        let gone = $recorded.applied | where $it not-in $available
        for g in $gone { warn $"recorded profile '($g)' no longer exists — dropped from the set" }
        $recorded.applied | where $it in $available
    }
    do-apply $c.root $wt $names
}

# Remove what apply placed. Symlinks always; copies only if still identical to
# their source.
def "main discard" [] {
    let c = ctx
    if $c.worktree == null { fail "run discard inside the worktree to discard" }
    let wtname = wt-name $c.root $c.worktree
    let st = load-state $c.root $wtname
    if $st == null { fail $"no applied profiles recorded for ($wtname)" }
    run-hooks (state-profiles $c.root $st) "before-discard" $c.root $c.worktree
    let kept = discard-entries $c.worktree $st.entries
    rm (state-file $c.root $wtname)
    print $"discarded [($st.applied | str join ', ')] from ($wtname)"
    for k in $kept { warn $"kept ($k): copy has diverged from its source" }
}

# Show which profiles a worktree has applied, and what exists to apply.
def "main which" [
    --on: string = ""  # worktree name; only from the root
] {
    let c = ctx
    let wt = if ($on | is-not-empty) {
        if $c.worktree != null { fail "--on only makes sense from the root" }
        named-worktree $c.root $on
    } else { $c.worktree }

    if $wt != null {
        let wtname = wt-name $c.root $wt
        let st = load-state $c.root $wtname
        if $st == null { print $"($wtname): no profiles applied"
        } else {
            print $"($wtname): [($st.applied | str join ', ')] applied ($st.at) — ($st.entries | length) entries"
            print ($st.entries | select target type profile | table -i false)
        }
    }
    print $"available profiles: ((list-profiles $c.root) | str join ', ')"
}

# New worktree named <name> (branch of the same name), based on the current
# worktree's HEAD when run inside one — including its gitignored files — then
# profiles applied.
def "main add" [
    name: string
    --profiles (-p): string = ""  # extra profiles for the initial apply
] {
    let c = ctx
    let dest = $c.root | path join $name
    if ($dest | path exists -n) { fail $"($dest) already exists" }

    let unborn = (try-git $c.root rev-parse "--verify" HEAD) == null
    let branch_exists = (try-git $c.root show-ref "--verify" $"refs/heads/($name)") != null
    if $unborn {
        run-git $c.root worktree add "--orphan" "-b" $name $dest | ignore
    } else if $branch_exists {
        run-git $c.root worktree add $dest $name | ignore
    } else {
        # -C into the current worktree so the new branch starts at ITS head
        run-git ($c.worktree | default $c.root) worktree add "-b" $name $dest | ignore
    }

    let names = resolve-names $c.root (split-profiles $profiles)

    # base worktree's gitignored files come along so local env survives the
    # split — except symlinks into the root (apply recreates those) and
    # targets the profiles are about to manage (apply places those; copying
    # first would just churn)
    if $c.worktree != null {
        let profs = $names | each {|n| load-profile $c.root $n }
        let tracked = run-git $dest ls-files | lines
        let managed = build-plan $c.root $dest $profs $tracked | get target
        let ignored = run-git $c.worktree status "--porcelain" "--ignored" | lines
            | where ($it starts-with "!! ") | each { str substring 3.. | str trim --char "/" }
        mut carried = 0
        for rel in $ignored {
            if $rel in $managed { continue }
            let src = $c.worktree | path join $rel
            if (try { $src | path type }) == "symlink" {
                let dst = try { ^readlink $src | complete | get stdout | str trim }
                let abs = if ($dst | path split | first) == "/" { $dst } else { ($src | path dirname) | path join $dst }
                if ($abs | path expand -n | str starts-with ($c.root | path expand -n)) { continue }
            }
            mkdir ($dest | path join $rel | path dirname)
            cp -r $src ($dest | path join $rel)
            $carried = $carried + 1
        }
        if $carried > 0 { note $"carried over ($carried) ignored paths from (wt-name $c.root $c.worktree)" }
    }

    do-apply $c.root $dest $names
    run-hooks ($names | each {|n| load-profile $c.root $n }) "after-add" $c.root $dest
}

# Remove a worktree: discard its profile entries, then git worktree remove.
def "main remove" [
    name: string
    --force (-f)  # pass through to git; needed when untracked files remain
] {
    let c = ctx
    let wt = named-worktree $c.root $name
    let wtname = wt-name $c.root $wt

    let st = load-state $c.root $wtname
    if $st != null {
        run-hooks (state-profiles $c.root $st) "before-remove" $c.root $wt
        discard-entries $wt $st.entries | ignore
        rm (state-file $c.root $wtname)
    }

    let args = if $force { [worktree remove "--force" $wt] } else { [worktree remove $wt] }
    let r = ^git -C $c.root ...$args | complete
    if $r.exit_code != 0 {
        fail $"($r.stderr | str trim)\nhint: --force removes it along with any remaining untracked files"
    }
    print $"removed worktree ($wtname)"
}

# In an empty folder: a fresh bare layout, no remote. In an existing repo:
# transform it — history to .bare, a worktree for the current branch, ignored
# files propagated to the dflt profile.
def "main init" [] {
    let cwd = $env.PWD
    if ($cwd | path join ".bare" | path exists -n) { fail "already a bare-worktree layout" }

    let dotgit = $cwd | path join ".git"
    if ($dotgit | path type) == "dir" { return (transform $cwd) }
    if ($dotgit | path type) == "file" { fail ".git is already a pointer file — layout half-made? inspect by hand" }

    run-git $cwd init "--bare" ".bare" | ignore
    "gitdir: ./.bare\n" | save ($cwd | path join ".git")
    mkdir ($cwd | path join $PROFILES $DFLT)
    print "initialized empty bare-worktree layout"
    print "next:"
    print "  1. bare-worktree add main            # first worktree (orphan branch)"
    print "  2. git remote add origin <url>       # when you have a remote, then:"
    print "     git config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*'"
    print "     # bare clones fetch only the default branch without this refspec"
}

def transform [root: string] {
    let branch = run-git $root branch "--show-current"
    if ($branch | is-empty) { fail "detached HEAD — check out a branch before transforming" }
    if (run-git $root status "--porcelain" | is-not-empty) {
        fail "working tree not clean — commit or stash everything (incl. untracked) first; only gitignored files may remain"
    }

    let ignored = run-git $root status "--porcelain" "--ignored" | lines
        | where ($it starts-with "!! ") | each { str substring 3.. | str trim --char "/" }
    let tracked = run-git $root ls-files | lines

    mv ($root | path join ".git") ($root | path join ".bare")
    ^git --git-dir ($root | path join ".bare") config core.bare true
    "gitdir: ./.bare\n" | save -f ($root | path join ".git")
    if "origin" in (run-git $root remote | lines) {
        run-git $root config remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*" | ignore
        note "set the bare-clone fetch refspec for origin (all branches, not just the default)"
    }

    let wt = $root | path join $branch
    run-git $root worktree add $wt $branch | ignore
    note $"worktree ($branch) checked out"

    # gitignored leftovers become the dflt profile; .claude stays at the root,
    # where apply symlinks it from
    let dflt = profiles-root $root | path join $DFLT
    mkdir $dflt
    for rel in ($ignored | where $it != ".claude") {
        mkdir ($dflt | path join $rel | path dirname)
        mv ($root | path join $rel) ($dflt | path join $rel)
    }
    note $"moved ($ignored | length) ignored paths into .profiles/dflt"

    # tracked files now live in the worktree; their originals in the container
    # are plain duplicates
    for f in $tracked { rm -f ($root | path join $f) }
    # .claude is deliberately NOT excluded: pruning only removes empty dirs, so
    # a real .claude survives while the hollowed-out shell of a tracked one goes
    let excl = [".bare/**" ".bare" ".profiles/**" ".profiles" $"($branch)/**" $branch]
    glob ($root | path join "**") --no-file --no-symlink --exclude $excl
        | where $it != $root
        | sort-by { $in | path split | length } --reverse
        | where (ls -a $it | is-empty)
        | each {|d| rm $d } | ignore

    let junk = ls -a $dflt | get name | path basename | where $it in $JUNK
    if ($junk | is-not-empty) {
        warn $"junk landed in .profiles/dflt: ($junk | str join ', ') — delete what a fresh checkout should rebuild"
    }
    if (ptype (profiles-root $root | path join $DFLT "profile.yaml")) == "file" {
        run-hooks [(load-profile $root $DFLT)] "after-init" $root $wt
    }

    print $"transformed: history in .bare, branch ($branch) in ./($branch), profiles in .profiles/dflt"
    print $"next: review .profiles/dflt, then `bare-worktree apply --to ($branch)`"
}

def main [] {
    print "bare-worktree <init|add|remove|apply|discard|which>"
    print ""
    print "  init                              new bare layout here, or transform this repo"
    print "  add <name> [-p a,b]               new worktree + branch, dflt (+listed) profiles applied"
    print "  remove <name> [--force]           discard profile entries, then git worktree remove"
    print "  apply [-p a,b] [--to <wt>] [--reset]  refresh recorded set; -p changes it, --reset back to dflt"
    print "  discard                           undo what apply placed (diverged copies survive)"
    print "  which [--on <wt>]                 show applied + available profiles"
}
