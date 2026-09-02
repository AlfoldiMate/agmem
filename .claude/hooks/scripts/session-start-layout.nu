#!/usr/bin/env nu
# SessionStart: the one check that is about this framework's files rather
# than about memory. Memory itself — the briefing, the branch tag, the
# post-compaction warning — arrives through the agmem plugin's own
# SessionStart hook (`agmem hook session-start`), so nothing here touches the
# store; a second briefing would only be noise on top of the plugin's.
#
# What remains is the worktree layout check: in a bare layout the real
# `.claude` lives at the shared root and is symlinked into each worktree, and
# a worktree carrying its own copy has silently diverged. That is a fact
# about this checkout, which the plugin cannot know.
const COMMON = path self "_common.nu"
use $COMMON *

def main []: any -> nothing {
    let p = $in | payload
    try {
        let mismatch = layout-check (cwd-of $p)
        if $mismatch != null { context "SessionStart" $mismatch }
    }
}
