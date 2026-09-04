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
use $COMMON [layout-check notes-check shared-root]

# agmem's version string under-reports (a build can carry space derivation
# while still saying 0.1.0), so probe behaviour, not the version: --doctor
# logs the space it derived for this directory. A binary without derivation
# lands every project in the literal space `default` — silently, collapsing
# per-project memory into one bucket, which is why this row exists.
def agmem-row []: nothing -> record {
    let bins = try { which -a agmem } | default []
    if ($bins | is-empty) {
        return { dep: "agmem", status: "MISSING", detail: ""
            fix: "brew install AlfoldiMate/tap/agmem; then: claude plugin marketplace add AlfoldiMate/agmem && claude plugin install agmem@agmem" }
    }

    # The plugin's hooks are `agmem hook <event>`, a subcommand that arrived in
    # 0.1.10; an older binary serves MCP fine and answers every hook with a
    # usage error, so the briefing never arrives and nothing says why. Probe
    # the subcommand, not the version: a build from the branch carries it
    # under the previous version string.
    let hook = (try { ^agmem hook --help | complete | get exit_code } | default 1)
    if $hook != 0 {
        let v = (try { ^agmem --version | complete | get stdout | str trim } | default "unknown version")
        return { dep: "agmem", status: "OLD", detail: $"($v), no `agmem hook`"
            fix: "brew upgrade agmem — without `agmem hook` (0.1.10+) the plugin's hooks are silent" }
    }

    # Subagents hand back long output as documents through `agmem doc put`
    # (scripts/doc-put.nu); without the subcommand every such write fails and
    # the agent falls back to a /tmp path nothing else reads.
    let doc = (try { ^agmem doc --help | complete | get exit_code } | default 1)
    if $doc != 0 {
        let v = (try { ^agmem --version | complete | get stdout | str trim } | default "unknown version")
        return { dep: "agmem", status: "OLD", detail: $"($v), no `agmem doc`"
            fix: "brew upgrade agmem — without `agmem doc` (0.2.0+) subagents cannot write documents" }
    }

    let r = try { ^agmem --doctor | complete }
    let m = ($"($r.stdout? | default '')\n($r.stderr? | default '')"
        | parse -r 'space=(?<s>[A-Za-z0-9_-]+)')
    let space = if ($m | is-empty) { "" } else { $m | first | get s }
    let extra = if ($bins | length) > 1 { $", ($bins | length) binaries on PATH" } else { "" }

    if $space == "" {
        { dep: "agmem", status: "BROKEN", detail: "--doctor reports no space"
          fix: "run `agmem --doctor` by hand and read its log" }
    } else if $space == "default" and ($env.PWD | path basename) != "default" {
        { dep: "agmem", status: "STALE", detail: $"derived space: default($extra)"
          fix: ("no space derivation — needs >= v0.1.1: brew upgrade agmem, and check "
              + "`which -a agmem` for a stale duplicate earlier on PATH") }
    } else {
        { dep: "agmem", status: "ok", detail: $"space=($space)($extra)"
          fix: (if $extra == "" { "" } else {
              "keep one binary — `which -a agmem`; a stale extra silently loses space derivation" }) }
    }
}

# Memory reaches a session through the agmem plugin — its SessionStart hook
# injects the briefing, its PostToolUse hook logs recalls and nudges at seams.
# This framework registers none of that itself any more, so a missing plugin
# is a session with no memory in front of it and nothing to say so.
def agmem-plugin-row []: nothing -> record {
    let listing = (try { ^claude plugin list | complete | get stdout } | default "")
    if ($listing | str contains "agmem") {
        { dep: "agmem plugin", status: "ok", detail: "claude plugin list shows it", fix: "" }
    } else {
        { dep: "agmem plugin", status: "MISSING", detail: ""
          fix: "claude plugin marketplace add AlfoldiMate/agmem && claude plugin install agmem@agmem" }
    }
}

# In a linked worktree, the framework resolves to the shared root — verify a
# .claude actually exists there, or profiles and hooks come from a stray copy.
def layout-row []: nothing -> record {
    let broken = try { layout-check $env.PWD }
    if $broken == null {
        { dep: "worktree layout", status: "ok", detail: "shared .claude reachable", fix: "" }
    } else {
        { dep: "worktree layout", status: "BROKEN", detail: "", fix: $broken }
    }
}

# `.claude/notes/` is retired: subagents write agmem documents. A file that
# lands there is invisible to memory, so a populated directory is a finding.
def notes-row []: nothing -> record {
    let warning = try { notes-check (shared-root $env.PWD) }
    if $warning == null {
        { dep: "notes", status: "ok", detail: "no .claude/notes dropbox", fix: "" }
    } else {
        { dep: "notes", status: "STALE", detail: "retired dropbox holds files", fix: $warning }
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
        (agmem-row)
        (agmem-plugin-row)
        (layout-row)
        (notes-row)
        (dep "acli" false (probe acli "--version") "https://developer.atlassian.com/cloud/acli/ — only needed for Jira")
    ]

    let langs = project-langs

    print ($deps | table -i false --width 160)
    print ""
    print ($langs | rename extension files ast-grep | table -i false --width 160)
}
