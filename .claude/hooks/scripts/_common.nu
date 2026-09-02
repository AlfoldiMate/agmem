# Shared helpers for ctx-flow hooks.
#
# One rule governs everything here: a hook must never break a session. Every
# entry point wraps its work in `try` and exits 0, and every lookup has a
# fallback rather than a failure mode.
#
# Nushell rather than Python: JSON is a built-in format, so the payload that
# needed `json.load` is one word.

export const NOTES_REL = ".claude/notes"

# --- payload -------------------------------------------------------------

# The hook payload as a record, or {} when stdin is absent or malformed.
export def payload []: any -> record {
    let parsed = try { $in | from json }
    if (($parsed | describe) starts-with "record") { $parsed } else { {} }
}

export def cwd-of [p: record]: nothing -> string {
    $p.cwd? | default $env.CLAUDE_PROJECT_DIR? | default $env.PWD
}

# --- git -----------------------------------------------------------------

# Trimmed stdout of a git command, or null if it failed or git is absent.
# Quote any argument starting with `-`, or the parser reads it as a flag of
# this command rather than as an argument to pass through.
def git-out [cwd: string, ...args: string]: nothing -> any {
    let r = try { ^git -C $cwd ...$args | complete }
    if ($r.exit_code? | default 1) == 0 { $r.stdout | str trim } else { null }
}

def absolute? [p: string]: nothing -> bool {
    ($p starts-with "/") or ($p =~ '^[A-Za-z]:[\\/]')
}

# Root of the MAIN worktree — shared by every linked worktree of this repo.
#
# `git rev-parse --git-common-dir` points at the one real .git directory from
# anywhere in the repo, including linked worktrees, so durable notes written
# here follow you across every branch and every worktree — with or without a
# symlinked .claude. Outside a repo this degrades to the cwd.
export def shared-root [cwd: string]: nothing -> string {
    let common = git-out $cwd rev-parse "--git-common-dir"
    if ($common | is-empty) { return $cwd }

    let root = (if (absolute? $common) { $common } else { $cwd | path join $common }
        | path expand | path dirname)
    if ($root | path type) == "dir" { $root } else { $cwd }
}

# Current branch, or null when detached or outside a repo.
#
# `branch --show-current` rather than `rev-parse --abbrev-ref HEAD`: it resolves
# on an unborn branch — a repo initialised but not yet committed to — where
# rev-parse fails outright, and returns empty on a detached HEAD instead of the
# literal string "HEAD". The closure form of `default` keeps the fallback lazy.
export def branch-of [cwd: string]: nothing -> any {
    let b = git-out $cwd branch "--show-current"
        | default {|| git-out $cwd rev-parse "--abbrev-ref" HEAD }
    if ($b | is-empty) or $b == "HEAD" { null } else { $b }
}

# A branch name reduced to something safe to use as a filename.
export def slug [b: string]: nothing -> string {
    $b
    | str replace -ra '[^A-Za-z0-9._-]+' "-"
    | str trim --char "-"
    | str substring ..<80
    | default --empty "detached"
}

# {root, ledger, branch, state, tag} — root and tag serve the live flow (the
# .claude/notes artifact dropbox and the agmem branch tag, one slug rule for
# the hook that announces it and the /checkpoint that writes it); ledger and
# state are the LEGACY file locations, still resolved so /agmem import can find
# what a pre-agmem checkout wrote. `state` and `tag` are null on a detached
# HEAD.
export def paths [p: record]: nothing -> record {
    let cwd = cwd-of $p
    let root = shared-root $cwd
    let branch = branch-of $cwd

    {
        root: $root
        ledger: ($root | path join $NOTES_REL "LEDGER.md")
        branch: $branch
        state: (if $branch == null { null } else {
            $root | path join $NOTES_REL "state" $"((slug $branch)).md"
        })
        tag: (if $branch == null { null } else { $"branch:(slug $branch)" })
    }
}

# Warning text when the worktree layout strands the shared .claude, or null
# when the layout is sound. The framework and the .claude/notes artifact
# dropbox resolve through `shared-root` — the main worktree, or with a bare
# repo the directory holding the bare git dir — so a `.claude` that exists only
# inside one linked worktree has diverged from the copy every other worktree
# shares. (Durable memory itself lives in agmem, keyed off the same shared git
# dir, so it is immune to this — the stakes here are profiles and artifacts.)
export def layout-check [cwd: string]: nothing -> any {
    let top = git-out $cwd rev-parse "--show-toplevel"
    if ($top | is-empty) { return null }                                # not in a work tree
    let root = shared-root $cwd
    if ($root | path expand) == ($top | path expand) { return null }    # the main worktree itself
    if ($root | path join ".claude" | path exists) { return null }      # follows symlinks by design
    if not ($top | path join ".claude" | path exists) { return null }   # no .claude anywhere — nothing stranded
    ($"WORKTREE LAYOUT MISMATCH: ($root)/.claude does not exist, but this worktree carries its "
        + "own .claude — in this layout the real .claude lives at the shared root and is "
        + "symlinked into each worktree, so profiles and the .claude/notes artifact dropbox "
        + "stay one copy. Fix: `bare-worktree apply`. /ctx-flow-doctor explains the layout.")
}

# --- tunables ------------------------------------------------------------
#
# One knob, and it is an environment variable over a built-in default. A hook
# that reads a config file has to parse it, validate it, and fall back silently
# when it is wrong — three more ways to break the session it was supposed to
# be helping.

export def env-int [name: string, fallback: int]: nothing -> int {
    try { $env | get $name | into int } catch { $fallback }
}

# --- output --------------------------------------------------------------

export def context [event: string, text: string] {
    print -n ({ hookSpecificOutput: { hookEventName: $event, additionalContext: $text } } | to json -r)
}
