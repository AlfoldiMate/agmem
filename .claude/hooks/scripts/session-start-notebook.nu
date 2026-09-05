#!/usr/bin/env nu
# SessionStart: put Claude's notebook into context, whole.
#
# The notebook (`.claude/notebook.md` at the shared root, so every worktree
# sees one copy) is the undistilled counterpart to agmem: open questions,
# changed minds, taste, complaints, drafts. Its value is that nothing rewrites
# it — so this hook prints it verbatim, with no budget and no trimming. If it
# outgrows what a session can carry, that is a finding for Claude to act on by
# pruning the file, not for a hook to hide by summarising.
#
# The file's last section is "Want to ask Matthew but haven't", so it lands
# as the freshest thing in context before the first prompt.
#
# Silent when the file is absent or empty: a checkout without a notebook is
# not an error. Like every hook here, nothing in this file can break a session.
const COMMON = path self "_common.nu"
use $COMMON *

const NOTEBOOK_REL = ".claude/notebook.md"

def main []: any -> nothing {
    let p = $in | payload
    try {
        let path = shared-root (cwd-of $p) | path join $NOTEBOOK_REL
        if ($path | path type) != "file" { return }
        let body = open --raw $path | str trim
        if ($body | is-empty) { return }
        context "SessionStart" ($"NOTEBOOK \(($NOTEBOOK_REL), Claude's own; write to it whenever "
            + "something is noticed, mid-task, not saved for a checkpoint\):\n\n" + $body)
    }
}
