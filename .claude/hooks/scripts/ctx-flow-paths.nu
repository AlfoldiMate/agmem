#!/usr/bin/env nu
# Resolve the two memory paths, exactly the way the hooks resolve them.
#
# The commands used to re-derive the branch filename themselves, in shell, by
# substituting dashes for slashes. Two implementations of one rule drift, and
# when this one drifts it fails silently: on `release/v1.2.3+build`,
# /checkpoint writes `state/release-v1.2.3+build.md` while SessionStart
# reads `state/release-v1.2.3-build.md`. The checkpoint is taken, the session is
# cleared, and the branch state never comes back. Anything git permits but a
# filename should not carry — `+ # ( ) % &`, non-ASCII, over 80 characters —
# diverges the same way. One resolver, called by both sides, is the only version
# of this that stays correct.
#
# Usage, from anywhere inside the repo:
#     nu ctx-flow-paths.nu            # KEY=VALUE lines, shell-friendly
#     nu ctx-flow-paths.nu --json
const COMMON = path self "_common.nu"
use $COMMON *

# Keys are printed straight from the record, so ROOT/LEDGER/BRANCH/STATE cannot
# drift from what `paths` actually returns.
def main [--json]: nothing -> nothing {
    let r = paths { cwd: $env.PWD }
    if $json {
        print ($r | to json)
    } else {
        # BRANCH and STATE come back empty on a detached HEAD.
        $r | items {|key, value| print $"($key | str uppercase)=($value | default '')" } | ignore
    }
}
