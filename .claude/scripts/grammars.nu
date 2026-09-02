#!/usr/bin/env nu
# What ast-grep can do with this project's languages — the one module that knows.
#
# ast-grep bundles a fixed set of grammars and loads anything else from a
# dynamic library. So "no grammar" was reporting a missing *binary* as a missing
# *capability*: for most tree-sitter languages it is one build away, not a dead
# end. Both halves read this file — build-grammar.nu compiles and registers,
# doctor.nu reports — because a second copy of any of it would drift, and the
# drift is silent: doctor claiming "built" for a language sgconfig.yml never
# registered, and every pattern failing as unsupported.

# --- what ast-grep already handles ---------------------------------------

# BUILT-IN grammars, keyed by file extension. Absent here does not mean absent
# from ast-grep — see CUSTOM_GRAMMARS for the ones this framework can compile.
export const AST_GREP_LANGS = {
    rs: rust, js: javascript, mjs: javascript, cjs: javascript, jsx: javascript
    ts: typescript, tsx: tsx, py: python, go: go, java: java, kt: kotlin
    c: c, h: c, cpp: cpp, cc: cpp, hpp: cpp, cs: csharp, rb: ruby
    swift: swift, scala: scala, php: php, lua: lua, dart: dart
    ex: elixir, exs: elixir, hs: haskell, html: html, css: css
    json: json, yaml: yaml, yml: yaml, sh: bash, bash: bash
}

# ext -> everything needed to build and register one grammar.
#
# `expando` is load-bearing, not decoration. ast-grep parses the *pattern* with
# the target grammar, so `$VAR` has to be valid syntax in that language. In nu
# `$` is the variable sigil: `def $N [$$$P] { }` is a parse error, and every
# metavariable pattern dies with "Multiple AST nodes are detected". expandoChar
# swaps `$` for a character the grammar accepts as a plain identifier, and
# ast-grep maps it back after matching.
#
# `subdir` is optional and only matters for monorepo grammars that keep
# grammar.js one level down (tree-sitter-typescript ships typescript/ and tsx/).
export const CUSTOM_GRAMMARS = {
    nu: {
        lang: "nu"
        exts: [nu]
        repo: "https://github.com/nushell/tree-sitter-nu"
        expando: "_"
    }
}

# --- where things live ----------------------------------------------------

# Compiled grammars live outside the repo. A .dylib/.so is architecture-specific
# so it can never be committed, and one cache shared by every checkout beats one
# build per worktree.
export def grammar-dir []: nothing -> string {
    $nu.home-dir | path join .cache ctx-flow grammars
}

export def dylib-path [lang: string]: nothing -> string {
    let suffix = match $nu.os-info.name {
        "macos" => "dylib"
        "windows" => "dll"
        _ => "so"
    }
    grammar-dir | path join $"($lang).($suffix)"
}

# Where ast-grep looks for project config: it walks up from the directory it is
# *run* in, which is not necessarily where .claude resolves to. Deriving this
# from `path self` would follow a symlinked .claude into the framework repo and
# write sgconfig.yml into the wrong project.
export def project-root []: nothing -> string {
    $env.CLAUDE_PROJECT_DIR? | default $env.PWD
}

export def sgconfig-path []: nothing -> string {
    project-root | path join "sgconfig.yml"
}

# --- current state --------------------------------------------------------

export def built? [lang: string]: nothing -> bool {
    (dylib-path $lang | path type) == "file"
}

# customLanguages from the project's sgconfig.yml, or {} if there is no usable
# config. A missing file, unreadable YAML and a config without the key are all
# the same answer, so all three collapse into the one `try`.
def sg-custom-langs []: nothing -> record {
    try { open (sgconfig-path) | get customLanguages } | default {}
}

# ext -> lang for every custom language sgconfig.yml registers, including ad-hoc
# ones this registry has never heard of. Without it doctor would report a
# hand-built, working grammar as "no grammar".
def sg-registered-exts []: nothing -> record {
    sg-custom-langs
    | items {|lang, cfg| ($cfg.extensions? | default []) | each {|e| { ext: $e, lang: $lang } } }
    | flatten
    | reduce --fold {} {|it, acc| $acc | upsert $it.ext $it.lang }
}

export def registered? [lang: string]: nothing -> bool {
    $lang in (sg-custom-langs | columns)
}

# --- this project ---------------------------------------------------------

# Top source-file extensions in the repo: [{ext, count}]. Uses git's index when
# available (fast, respects .gitignore); falls back to a glob outside a repo.
export def project-exts []: nothing -> table {
    let git = try { ^git ls-files | complete }
    let files = if ($git.exit_code? | default 1) == 0 {
        $git.stdout | lines
    } else {
        try { glob --depth 5 "**/*" --exclude [**/node_modules/** **/target/** **/.git/**] } | default []
    }
    $files
    | each {|f| $f | path parse | get extension }
    | where $it != ""
    | group-by
    | items {|ext, hits| { ext: $ext, count: ($hits | length) } }
    | sort-by -r count
    | where ext not-in [md txt lock toml png jpg svg gitignore]
    | first 6
}

# The ast-grep column for one extension in doctor.nu's second table.
#
# Built and registered are independent failures with the same symptom — ast-grep
# says the language is unsupported either way — so they get separate states
# rather than one "broken". The fix points at /ast-grep-it because that is the
# entry point a reader can act on: for an ad-hoc language the underlying script
# also needs --repo, which sgconfig.yml does not record.
export def grammar-status [ext: string]: nothing -> string {
    let builtin = $AST_GREP_LANGS | get -o $ext
    if $builtin != null { return $builtin }

    let known = $CUSTOM_GRAMMARS | get -o $ext
    let lang = if $known != null { $known.lang } else { sg-registered-exts | get -o $ext }
    if $lang == null { return "no grammar" }

    let fix = $"/ast-grep-it ($lang)"
    match [(built? $lang) (registered? $lang)] {
        [false false] => $"buildable → ($fix)"
        [false true] => $"registered, unbuilt → ($fix)"
        [true false] => $"unregistered → ($fix)"
        _ => "custom (built)"
    }
}

# This project's extensions with their ast-grep verdict — doctor's second table,
# and the input to every "what should I build here?" decision.
export def project-langs []: nothing -> table {
    project-exts | insert ast-grep {|row| grammar-status $row.ext }
}

# The one language this project would gain most from building, or null when
# there is nothing to do. Ranked by file count, so the repo's main language wins
# over an incidental script or two. Restricted to registry entries: an arrow in
# the status is only actionable without --repo if we know where the grammar
# lives, and sgconfig.yml does not record that.
export def suggest-target []: nothing -> any {
    let actionable = project-langs
        | where {|row| ($CUSTOM_GRAMMARS | get -o $row.ext) != null }
        | where ast-grep =~ '→'
    if ($actionable | is-empty) { null } else { $actionable | first }
}
