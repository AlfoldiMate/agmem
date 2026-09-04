#!/usr/bin/env nu
# Cases for prompt-context-nudge.nu, run from anywhere:
#
#     nu .claude/hooks/tests/context-nudge.nu
#
# Each case writes a synthetic transcript — assistant lines carrying the
# API's `usage` block, as Claude Code records them — and pipes a
# UserPromptSubmit payload through the hook. Checks: silent below the
# threshold, one nudge at it, silence on the next prompt, a second nudge only
# after a further step, a fresh transcript under the same session id starts
# over, and the nudge text stays under 120 chars.
const HOOK = path self "../scripts/prompt-context-nudge.nu"

def transcript [name: string, tokens: int]: nothing -> string {
    let path = $nu.temp-dir | path join $"ctx-flow-test-($name).jsonl"
    let filler = { type: "user", message: { role: "user", content: "x" } } | to json -r
    let usage = { type: "assistant", message: { role: "assistant", usage: {
        input_tokens: 40, cache_read_input_tokens: ($tokens - 1040), cache_creation_input_tokens: 1000, output_tokens: 5
    } } } | to json -r
    [$filler $usage $filler] | str join "\n" | save -f $path
    $path
}

def fire [session: string, transcript: string]: nothing -> record {
    let out = { session_id: $session, transcript_path: $transcript, cwd: $env.PWD, prompt: "hi" }
        | to json -r | nu --stdin $HOOK | complete
    let text = try { $out.stdout | from json | get hookSpecificOutput.additionalContext } catch { "" }
    { nudged: ($text | is-not-empty), text: $text, exit: $out.exit_code, stderr: $out.stderr }
}

def main [] {
    rm -f ($nu.temp-dir | path join "ctx-flow-context-ctxtest*")
    let s = "ctxtest-a"
    let low = transcript low 80000
    let at = transcript at 125000
    let up = transcript up 170000
    let fresh = transcript fresh 130000

    let cases = [
        [name, result, expect];
        ["silent below threshold"          (fire $s $low)    false]
        ["nudges at threshold"             (fire $s $at)     true]
        ["silent on the next prompt"       (fire $s $at)     false]
        ["nudges again one step up"        (fire $s $up)     true]
        ["silent at the same step"         (fire $s $up)     false]
        ["fresh transcript, same session"  (fire $s $fresh)  true]
        ["no transcript_path"              (fire $s "")      false]
    ]

    mut failed = 0
    for c in $cases {
        let ok = ($c.result.nudged == $c.expect) and ($c.result.exit == 0)
        if not $ok { $failed += 1 }
        print $"(if $ok { 'ok  ' } else { 'FAIL' }) ($c.name)  → nudged=($c.result.nudged) expected=($c.expect)"
        if not $ok and ($c.result.stderr | is-not-empty) { print $"     stderr: ($c.result.stderr)" }
        if $c.result.nudged and ($c.result.text | str length) > 120 {
            $failed += 1
            print $"FAIL ($c.name): text is (($c.result.text | str length)) chars \(cap 120\)"
        }
    }
    rm -f ($nu.temp-dir | path join "ctx-flow-test-*.jsonl") ($nu.temp-dir | path join "ctx-flow-context-ctxtest*")
    if $failed > 0 { print $"($failed) failure\(s\)"; exit 1 }
    print $"all (($cases | length)) cases pass"
}
