#!/usr/bin/env nu
# ctx-flow doctor: verify the dependencies the framework leans on, and whether
# ast-grep can parse what this project is written in. Read-only.
#
# Prints two tables: one row per dependency with a fix hint, then the project's
# top source extensions and which ast-grep grammar handles each — built in,
# buildable from a tree-sitter repo, or absent.
# The /ctx-flow-doctor command runs this and interprets the result.

# First line of a command's stdout if it exits 0, else null.
def probe [cmd: string, ...args: string]: nothing -> any {
    let r = try { ^$cmd ...$args | complete }
    if ($r.exit_code? | default 1) == 0 {
        $r.stdout | str trim | lines | first | default ""
    } else { null }
}

def dep [name: string, required: bool, found: any, fix: string]: nothing -> record {
    {
        dep: $name
        status: (if $found != null { "ok" } else if $required { "MISSING" } else { "missing (optional)" })
        detail: ($found | default "")
        fix: (if $found == null { $fix } else { "" })
    }
}

# The rtk hook ships in this framework's settings.json (`rtk hook claude`); a
# user who also ran `rtk init -g` has it registered twice, and each Bash call
# would be rewritten twice. Report all three states.
const PROJECT_SETTINGS = path self "../settings.json"
const GRAMMARS = path self "grammars.nu"
const COMMON = path self "../hooks/scripts/_common.nu"
use $GRAMMARS *
use $COMMON [layout-check]

# In a linked worktree, durable memory resolves to the shared root — verify a
# .claude actually exists there, or checkpoints land where nothing reads.
def layout-row []: nothing -> record {
    let broken = try { layout-check $env.PWD }
    if $broken == null {
        { dep: "worktree layout", status: "ok", detail: "memory root reachable", fix: "" }
    } else {
        { dep: "worktree layout", status: "BROKEN", detail: "", fix: $broken }
    }
}

def rtk-hook-row []: nothing -> record {
    let here = (try { open --raw $PROJECT_SETTINGS } | default "" | str contains "rtk hook")
    let global = (try { open --raw ($nu.home-dir | path join .claude settings.json) } | default "" | str contains "rtk hook")

    if $here and $global {
        { dep: "rtk hook", status: "DOUBLED", detail: "in project AND ~/.claude settings"
          fix: "remove one registration — rtk rewrites every Bash call twice" }
    } else if $here or $global {
        { dep: "rtk hook", status: "ok"
          detail: (if $here { "project settings.json" } else { "~/.claude/settings.json" }), fix: "" }
    } else {
        { dep: "rtk hook", status: "MISSING", detail: ""
          fix: "restore the PreToolUse `rtk hook claude` entry in .claude/settings.json" }
    }
}

def main []: nothing -> nothing {
    let nu_mcp = (try { ^nu --help | complete | get stdout } | default "" | str contains "--mcp")

    let deps = [
        (dep "nu" true (version).version "brew install nushell")
        (dep "nu --mcp" true (if $nu_mcp { "supported" } else { null })
            "nu too old for MCP — upgrade: brew upgrade nushell; then register: claude mcp add nu -- nu --mcp")
        (dep "ast-grep" true (probe ast-grep "--version") "brew install ast-grep")
        (dep "gh" true (probe gh "--version") "brew install gh && gh auth login")
        (dep "gh auth" true (if (probe gh auth "status") != null { "authenticated" } else { null }) "gh auth login")
        (dep "playwright-cli" true (probe playwright-cli "--version")
            "npm i -g playwright-cli   # https://github.com/microsoft/playwright-cli")
        (dep "rtk" true (probe rtk "--version") "brew install rtk")
        (rtk-hook-row)
        (layout-row)
        (dep "acli" false (probe acli "--version") "https://developer.atlassian.com/cloud/acli/ — only needed for Jira")
    ]

    let langs = project-langs

    print ($deps | table -i false --width 160)
    print ""
    print ($langs | rename extension files ast-grep | table -i false --width 160)
}
