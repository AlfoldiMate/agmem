#!/usr/bin/env nu
# PreToolUse(Bash): in a bare-worktree layout, raw `git worktree add/remove/
# move` skips everything the layout depends on — profile application, the
# root-.claude symlink, the state manifest — so the worktree comes up without
# its env files or framework, and `which`/`discard` no longer tell the truth.
# The deny routes to the framework script instead. Outside a bare layout the
# hook stays silent: a plain repo has no profiles to skip, and read-only
# subcommands (list, prune, lock) pass everywhere.
#
# Same safety rule as every hook here: wrapped in try, exit 0 — a bug costs
# one uncaught worktree command, never a broken session.
const COMMON = path self "_common.nu"
use $COMMON *

# Anchored to the start of a command or a separator, so a quoted mention
# (`echo "git worktree add"`) does not trip it; quotes are stripped first.
const RE = '(^|[;&|(]\s*)git\b[^;&|]*\bworktree\s+(add|remove|move)\b'

def bare-layout? [cwd: string]: nothing -> bool {
    let r = try { ^git -C $cwd rev-parse --git-common-dir | complete }
    if ($r.exit_code? | default 1) != 0 { return false }
    ($r.stdout | str trim | path basename) == ".bare"
}

def main []: any -> nothing {
    let p = $in | payload
    try {
        let cmd = $p.tool_input?.command? | default ""
        let stripped = $cmd
            | str replace -ra `"[^"]*"|'[^']*'` ""
            | str replace -ra '\s+' " "
        if not ($stripped =~ $RE) { return }
        if not (bare-layout? (cwd-of $p)) { return }
        print -n ({ hookSpecificOutput: {
            hookEventName: "PreToolUse"
            permissionDecision: "deny"
            permissionDecisionReason: ("This project uses the bare-worktree layout: raw `git worktree "
                + "add/remove/move` skips profile application, the root-.claude symlink, and the "
                + "state manifest. Use the framework path instead: "
                + "nu \"${CLAUDE_PROJECT_DIR:-.}/.claude/scripts/bare-worktree.nu\" add|remove|apply|which <args> "
                + "(worktree list/prune/lock are fine to run raw).")
        } } | to json -r)
    }
}
