#!/usr/bin/env nu
# Cases for session-start-notebook.nu, run from anywhere:
#
#     nu .claude/hooks/tests/notebook-load.nu
#
# Each case builds a throwaway git repo (the hook resolves the notebook through
# the shared root) and pipes a SessionStart payload through the hook. Checks:
# silent with no notebook, silent with an empty one, the whole body verbatim
# when one exists, and the last section is the "want to ask" one.
const HOOK = path self "../scripts/session-start-notebook.nu"

def repo [name: string, body: any]: nothing -> string {
    let dir = $nu.temp-dir | path join $"ctx-flow-test-notebook-($name)"
    rm -rf $dir
    mkdir ($dir | path join ".claude")
    ^git -C $dir init -q
    if $body != null { $body | save -f ($dir | path join ".claude" "notebook.md") }
    $dir
}

def fire [cwd: string]: nothing -> record {
    let out = { session_id: "nbtest", cwd: $cwd, source: "startup" }
        | to json -r | nu --stdin $HOOK | complete
    let text = try { $out.stdout | from json | get hookSpecificOutput.additionalContext } catch { "" }
    { text: $text, exit: $out.exit_code, stderr: $out.stderr }
}

def main [] {
    let body = "# Notebook\n\n## Open questions\n\n- 2026-09-05. one $ { [ ? | thing\n\n## Want to ask Matthew but haven't\n\n- 2026-09-05. last\n"
    let none = fire (repo none null)
    let empty = fire (repo empty "  \n")
    let full = fire (repo full $body)

    let cases = [
        [name, ok];
        ["silent with no notebook"      ($none.text | is-empty)]
        ["silent with an empty one"     ($empty.text | is-empty)]
        ["body verbatim"                ($full.text | str contains ($body | str trim))]
        ["want-to-ask lands last"       ($full.text | str ends-with "- 2026-09-05. last")]
        ["names the file"               ($full.text | str contains ".claude/notebook.md")]
        ["exit 0 throughout"            ([$none $empty $full] | all {|r| $r.exit == 0 })]
    ]

    mut failed = 0
    for c in $cases {
        let mark = if $c.ok { "ok  " } else { "FAIL" }
        print $"($mark) ($c.name)"
        if not $c.ok { $failed += 1 }
    }
    rm -rf ($nu.temp-dir | path join "ctx-flow-test-notebook-*")
    if $failed > 0 { print $"($failed) failed"; exit 1 }
}
