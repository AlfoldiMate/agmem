#!/usr/bin/env nu
# Resolve the shared root, the agmem branch tag, and the legacy memory paths,
# exactly the way the hooks resolve them.
#
# The commands used to re-derive the branch slug themselves, in shell, by
# substituting dashes for slashes. Two implementations of one rule drift, and
# when this one drifts it fails silently: on `release/v1.2.3+build`,
# /checkpoint tags claims `branch:release-v1.2.3+build` while the SessionStart
# nudge announces `branch:release-v1.2.3-build`. The checkpoint is taken, the
# session is cleared, and the branch state is never recalled. Anything git
# permits but a slug should not carry — `+ # ( ) % &`, non-ASCII, over 80
# characters — diverges the same way. One resolver, called by both sides, is
# the only version of this that stays correct. LEDGER/STATE are the legacy
# file locations, kept so /agmem-import can find a pre-agmem checkout's notes.
#
# Usage, from anywhere inside the repo:
#     nu ctx-flow-paths.nu            # KEY=VALUE lines, shell-friendly
#     nu ctx-flow-paths.nu --json
const COMMON = path self "_common.nu"
use $COMMON *

# Keys are printed straight from the record, so ROOT/LEDGER/BRANCH/STATE/TAG
# cannot drift from what `paths` actually returns.
def main [--json]: nothing -> nothing {
    let r = paths { cwd: $env.PWD }
    if $json {
        print ($r | to json)
    } else {
        # BRANCH, STATE and TAG come back empty on a detached HEAD.
        $r | items {|key, value| print $"($key | str uppercase)=($value | default '')" } | ignore
    }
}
