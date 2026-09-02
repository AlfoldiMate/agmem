#!/usr/bin/env nu
# Build a tree-sitter grammar ast-grep does not ship, and register it for this
# project. `/ast-grep-it` is the front door; this is the deterministic half.
#
#     nu .claude/scripts/build-grammar.nu            # whatever this project needs
#     nu .claude/scripts/build-grammar.nu nu
#     nu .claude/scripts/build-grammar.nu zig --repo https://github.com/... --expando _
#     nu .claude/scripts/build-grammar.nu --list
#
# Languages in the registry need only their name. Anything else needs --repo,
# and then the flags describe it: --subdir for monorepo grammars, --exts when
# the extensions are not just the language name, --expando when `$` already
# means something in that language.
#
# Everything it writes is local and disposable: the compiled library goes to
# ~/.cache/ctx-flow/grammars, shared by every project on this machine, and the
# only project file it touches is sgconfig.yml, which it will not clobber.
const GRAMMARS = path self "grammars.nu"
use $GRAMMARS *

def have [cmd: string]: nothing -> bool {
    (which $cmd | length) > 0
}

def die [msg: string] {
    print $"(ansi red)($msg)(ansi reset)"
    exit 1
}

# Add the language to sgconfig.yml without destroying what is already there.
#
# `to yaml` cannot round-trip comments, so a hand-written config is printed for
# the user to paste rather than silently reformatted. Only a config with nothing
# to lose gets rewritten in place.
def register [g: record] {
    let cfg_path = sgconfig-path
    let entry = {
        libraryPath: (dylib-path $g.lang)
        extensions: $g.exts
    } | merge (if ($g.expando | is-empty) { {} } else { { expandoChar: $g.expando } })

    if (registered? $g.lang) {
        print $"sgconfig.yml already registers ($g.lang) — left alone"
        return
    }

    if ($cfg_path | path type) != "file" {
        { ruleDirs: [], customLanguages: { ($g.lang): $entry } } | to yaml | save -f $cfg_path
        print $"wrote ($cfg_path)"
        print "  gitignore it — libraryPath is an absolute, machine-local path"
        return
    }

    if (open --raw $cfg_path) =~ '(?m)^\s*#' {
        print $"($cfg_path) has comments — add this under customLanguages by hand:"
        print ({ ($g.lang): $entry } | to yaml | lines | each {|l| $"  ($l)" } | str join "\n")
        return
    }

    let cfg = open $cfg_path
    let langs = ($cfg | get -o customLanguages | default {}) | upsert $g.lang $entry
    $cfg | upsert customLanguages $langs | to yaml | save -f $cfg_path
    print $"updated ($cfg_path)"
}

# Can ast-grep parse a metavariable pattern in this language?
#
# Pattern parsing happens before any file is read, so an empty file of the right
# extension is enough to ask. Exit 8 is "cannot parse query"; exit 1 is "parsed
# fine, matched nothing", which is the answer we want. This is what catches a
# missing expandoChar — the single failure that makes a correctly built grammar
# look unsupported.
def probe-pattern [lang: string, ext: string, pattern: string]: nothing -> bool {
    let dir = mktemp -d
    let file = $dir | path join $"probe.($ext)"
    "" | save -f $file
    let r = do { cd $dir; ^ast-grep run --config (sgconfig-path) -p $pattern -l $lang $file | complete }
    rm -rf $dir
    $r.exit_code != 8
}

# What to build when called with no language: the extension this project has
# most of that ast-grep cannot handle yet. Returns null — after saying why — when
# there is nothing actionable, which is the normal state once a project is set
# up. Printing the reason here rather than returning it keeps main linear.
def auto-target []: nothing -> any {
    let s = suggest-target
    if $s != null {
        print $"($s.ext): ($s.count) files in this project — ($s.ast-grep)"
        return ($CUSTOM_GRAMMARS | get $s.ext | get lang)
    }

    let langs = project-langs
    let unknown = $langs | where ast-grep == "no grammar"
    if ($unknown | is-empty) {
        print $"(ansi green)nothing to build(ansi reset) — ast-grep already handles this project:"
        print ($langs | rename extension files ast-grep | table -i false --width 160)
    } else {
        print "no registry entry for this project's remaining languages:"
        print ($unknown | select ext count | rename extension files | table -i false)
        print $"  build one anyway:  nu .claude/scripts/build-grammar.nu ($unknown | first | get ext) --repo <url>"
    }
    null
}

def main [
    lang?: string           # registry key or language name; omit to auto-detect
    --repo: string = ""     # grammar repo; required for languages not in the registry
    --subdir: string = ""   # grammar directory inside the repo, for monorepos
    --exts: list<string> = []   # file extensions; defaults to [<lang>]
    --expando: string = ""  # character replacing $ in patterns
    --list                  # print the grammars this script knows by name
    --force                 # re-clone and rebuild even if the library is there
] {
    if $list {
        print ($CUSTOM_GRAMMARS | items {|ext, g| { ext: $ext, lang: $g.lang, repo: $g.repo } }
            | table -i false --width 160)
        return
    }

    # An explicit language always wins; bare invocation asks the project what it
    # needs. Closure form, so auto-target only runs — and only shells out to git
    # — when no language was given.
    let lang = $lang | default {|| auto-target }
    if $lang == null { return }

    # Registry entry is the default; every flag given overrides it, so a known
    # language can still be built from a fork or pinned to a different subdir.
    # `default --empty`, not plain `default`: the flags arrive as "" rather than
    # null when unset, and plain `default` only replaces null — which silently
    # blanks the registry's repo for every known language.
    let known = $CUSTOM_GRAMMARS | get -o $lang | default {}
    let g = {
        lang: $lang
        repo: ($repo | default --empty ($known.repo? | default ""))
        subdir: ($subdir | default --empty ($known.subdir? | default ""))
        exts: (if ($exts | is-not-empty) { $exts } else { $known.exts? | default [$lang] })
        expando: ($expando | default --empty ($known.expando? | default ""))
    }

    if ($g.repo | is-empty) {
        die $"no repo for '($lang)' — pass --repo <url>, or pick a known one: ($CUSTOM_GRAMMARS | columns | str join ', ')"
    }

    # All three are needed and none is implied by having ast-grep, so check
    # before cloning rather than failing halfway through a build.
    let missing = [git tree-sitter cc] | where {|c| not (have $c) }
    if ($missing | is-not-empty) {
        print $"(ansi red)missing: ($missing | str join ', ')(ansi reset)"
        print "  tree-sitter: npm i -g tree-sitter-cli"
        print "  cc: xcode-select --install   # macOS; else install gcc/clang"
        exit 1
    }

    let dir = grammar-dir
    let src = $dir | path join $"tree-sitter-($g.lang)"
    let lib = dylib-path $g.lang
    mkdir $dir

    if $force and ($src | path type) == "dir" { rm -rf $src }

    if ($src | path type) != "dir" {
        print $"cloning ($g.repo)"
        let r = ^git clone --depth 1 $g.repo $src | complete
        if $r.exit_code != 0 { print $r.stderr; die "clone failed" }
    }

    if $force or not (built? $g.lang) {
        # `tree-sitter build` takes the grammar directory as an argument, which
        # keeps the subdir case a path join rather than a cd.
        let grammar_src = if ($g.subdir | is-empty) { $src } else { $src | path join $g.subdir }
        if ($grammar_src | path join "grammar.js" | path type) != "file" {
            die $"no grammar.js in ($grammar_src) — wrong --subdir?"
        }
        print $"building ($g.lang) → ($lib)"
        let r = ^tree-sitter build -o $lib $grammar_src | complete
        if $r.exit_code != 0 { print $r.stderr; die "build failed" }
    } else {
        print $"($lib) already built — use --force to rebuild"
    }

    register $g

    # A grammar that builds and registers can still reject every metavariable
    # pattern, so say which of the two actually happened rather than "done".
    # This proves `$A` resolves, not that every pattern shape will: a pattern can
    # still fail because the grammar has no node for that shape, which is a
    # pattern problem rather than a setup problem.
    let ext = $g.exts | first
    print ""
    if (probe-pattern $g.lang $ext '$A') {
        print $"(ansi green)ok(ansi reset) — ast-grep parses ($g.lang), and `$A` resolves as a metavariable"
    } else {
        print $"(ansi yellow)built, but `$A` does not parse(ansi reset) — this language gives `$` its own meaning."
        print $"  re-run with an expando char:  nu .claude/scripts/build-grammar.nu ($g.lang) --expando _ --force"
    }
    print $"try it:  ast-grep run --kind <node-kind> -l ($g.lang) <path>"

    if ($CUSTOM_GRAMMARS | get -o $lang) == null {
        print ""
        print $"LEARNED: ($g.lang) grammar builds from ($g.repo) — exts ($g.exts | str join ','), expando '($g.expando)'"
    }
}
