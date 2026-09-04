#!/usr/bin/env nu
# Cases for pre-read-guard.nu, run from the repo root:
#
#     nu .claude/hooks/tests/read-guard.nu
#
# Each case pipes a PreToolUse payload through the hook exactly as Claude Code
# would and checks whether it came back with a deny. The files named are this
# repo's own: docs/design.md and tests/protocol.rs are floods, Cargo.toml is
# not. Exits 1 on the first mismatch, so it can sit in a pre-push check.
const HOOK = path self "../scripts/pre-read-guard.nu"

const BIG = "crates/agmem-server/tests/protocol.rs"

def hook [tool: string, input: record]: nothing -> record {
    let payload = { session_id: "t", cwd: $env.PWD, tool_name: $tool, tool_input: $input } | to json -r
    let out = $payload | nu --stdin $HOOK | complete
    let denied = ($out.stdout | str contains '"deny"')
    let reason = try { $out.stdout | from json | get hookSpecificOutput.permissionDecisionReason } catch { "" }
    { denied: $denied, reason: $reason, exit: $out.exit_code, stderr: $out.stderr }
}

def read [file: string, ...rest: any]: nothing -> record { hook Read ({ file_path: ($env.PWD | path join $file) } | merge ($rest | first | default {})) }
def bash [command: string]: nothing -> record { hook Bash { command: $command } }

def main [] {
    let cases = [
        # --- Read ---
        [name, result, expect];
        ["bare Read of design.md"                (read docs/design.md)                       true]
        ["Read design.md with offset+limit"      (read docs/design.md {offset: 40, limit: 120}) false]
        ["Read design.md with limit only"        (read docs/design.md {limit: 2000})         false]
        ["bare Read of Cargo.toml"               (read Cargo.toml)                           false]
        ["bare Read of a missing file"           (read does/not/exist.rs)                    false]
        ["bare Read of a png"                    (hook Read {file_path: "/tmp/x.png"})        false]
        # --- Bash ---
        ["cat protocol.rs"                       (bash $"cat ($BIG)")                        true]
        ["cat Cargo.toml"                        (bash "cat Cargo.toml")                     false]
        ["cat protocol.rs | wc -l"               (bash $"cat ($BIG) | wc -l")                false]
        ["cat with a quoted path"                (bash $"cat '($BIG)'")                      true]
        ["cd then cat protocol.rs"               (bash $"cd crates && cat ($BIG)")            true]
        ["sed -n windowed"                       (bash $"sed -n '40,120p' ($BIG)")           false]
        ["sed -n wide window"                    (bash $"sed -n '1,900p' ($BIG)")            true]
        ["sed -n to end"                         (bash $"sed -n '10,$p' ($BIG)")             true]
        ["sed -n regex print"                    (bash $"sed -n '/fn /p' ($BIG)")            false]
        ["sed substitution preview"              (bash $"sed 's/a/b/' ($BIG)")               true]
        ["sed -i edit"                           (bash $"sed -i '' 's/a/b/' ($BIG)")         false]
        ["head default"                          (bash $"head ($BIG)")                       false]
        ["head -n 50"                            (bash $"head -n 50 ($BIG)")                 false]
        ["head -500"                             (bash $"head -500 ($BIG)")                  true]
        ["tail -n 20"                            (bash $"tail -n 20 ($BIG)")                 false]
        ["tail -n +1"                            (bash $"tail -n +1 ($BIG)")                 true]
        ["heredoc write"                         (bash $"cat > /tmp/x.rs <<'EOF'\nfn main\(\) {}\nEOF")  false]
        ["redirect"                              (bash $"cat ($BIG) > /tmp/copy.rs")         false]
        ["cat via variable"                      (bash 'cat "$CLAUDE_PROJECT_DIR/docs/design.md"') false]
        ["grep unaffected"                       (bash $"grep -n fn ($BIG)")                 false]
    ]

    mut failed = 0
    for c in $cases {
        let ok = ($c.result.denied == $c.expect) and ($c.result.exit == 0)
        if not $ok { $failed += 1 }
        let mark = if $ok { "ok  " } else { "FAIL" }
        print $"($mark) ($c.name)  → denied=($c.result.denied) expected=($c.expect)"
        if not $ok and ($c.result.stderr | is-not-empty) { print $"     stderr: ($c.result.stderr)" }
        if $c.result.denied and ($c.result.reason | str length) > 200 {
            $failed += 1
            print $"FAIL ($c.name): reason is (($c.result.reason | str length)) chars \(cap 200\)"
        }
    }
    if $failed > 0 { print $"($failed) failure\(s\)"; exit 1 }
    print $"all (($cases | length)) cases pass"
}
