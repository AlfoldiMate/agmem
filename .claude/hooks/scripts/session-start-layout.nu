#!/usr/bin/env nu
# SessionStart: the one check that is about this framework's files rather
# than about memory. Memory itself — the briefing, the branch tag, the
# post-compaction warning — arrives through the agmem plugin's own
# SessionStart hook (`agmem hook session-start`), so nothing here touches the
# store; a second briefing would only be noise on top of the plugin's.
#
# What remains is two checks about this checkout's files, which the plugin
# cannot know: the worktree layout — in a bare layout the real `.claude` lives
# at the shared root and is symlinked into each worktree, and a worktree
# carrying its own copy has silently diverged — and the retired
# `.claude/notes/` dropbox, which subagents no longer write and nothing reads,
# so a file landing there is a regression worth one line.
const COMMON = path self "_common.nu"
use $COMMON *

def main []: any -> nothing {
    let p = $in | payload
    try {
        let cwd = cwd-of $p
        let findings = [(layout-check $cwd) (notes-check (shared-root $cwd))] | compact
        if not ($findings | is-empty) { context "SessionStart" ($findings | str join "\n\n") }
    }
}
