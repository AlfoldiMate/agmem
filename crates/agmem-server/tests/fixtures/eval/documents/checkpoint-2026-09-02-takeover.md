# Checkpoint 2026-09-02 (late morning) — could not reach agmem

This session's agmem MCP server was a 0.1.7 process; it died when the 0.1.7
daemon retired during the brew upgrade, and MCP does not reconnect
mid-session. Store these with `remember` from a fresh session, then delete
this note.

## branch:main (fact, decay_class fast, tag branch:main, supersedes 01M1GQ80NHCER0BQ4VHYK6CRAB)

Branch main, 2026-09-02 ~11:00Z: v0.1.8 is released (PR #123 merged; Release
run 33614459615 needed a `gh run rerun --failed` after the x86_64 Linux
attest step hit a Sigstore "GCP AsymmetricSign CANCELLED" flake; attempt 2
published the Homebrew formula) and installed on this machine
(/opt/homebrew/bin/agmem 0.1.8). The 0.1.8 daemon that served the dogfood
store exited on idle at 10:19:47Z; the next attach starts a fresh one. Issue
#124 (release takeover races the retiring daemon's store lock) is filed with
the reproduction, and PR #125 on branch fix/takeover-lock-order fixes it;
first CI run failed only on rustfmt, the fix is pushed (33c7293). Next: merge
#125 when CI is green; v0.1.9 is the first release with it, and the
0.1.8 → 0.1.9 upgrade still goes through the client's retry-once path
because the 0.1.8 daemon opens the gap. Still open otherwise: only #86
(re-probe 2026-09-16 and 2026-10-01); #119/#120 parked.

## #124 mechanism (fact, entities agmem, issue-124)

agmem's daemon held two locks released at different moments: agmem.lock (an
flock guard local to serve::run, released when run returns) and
agmem.db/LOCK (surrealkv's own flock, released when the engine's shutdown
task finishes closing the store, a few ms later). The client's
wait_for_store_lock treated agmem.lock free as "process gone", spawned a
daemon in the gap, and that daemon died on the store's lock with its error
on a closed stderr, costing the session the 120 s READY_DEADLINE. Reproduced
with the v0.1.7 release binary (a 0.1.7 daemon with an attached session,
then a 0.1.8 `agmem context`): lost 2 of 3 runs. PR #125 makes the daemon
drain its sessions, drop its last Db handle, and wait for agmem.db/LOCK to
be free (10 s cap, ~140 ms in practice) before returning; logs the run
error to daemon.log; and has the client try_wait its spawned Child and
retry the spawn once after a retirement.

## Runtime-drop does not release the store (lesson, tag role:architect, entities agmem, surrealkv)

Holding agmem.lock outside the tokio runtime (sync main, lock taken before
Builder::build and dropped after the Runtime) does not order the locks:
surrealkv 0.21's `impl Drop for Tree` spawns `core.close()` onto
`Handle::try_current()`, and when the Tree drops during runtime teardown
that spawn lands on a runtime that is shutting down, so close never runs
and agmem.db/LOCK lives until process exit. Measured 9/10 failures on
tests/takeover.rs with that approach. The store only lets go when the last
`Surreal` handle is dropped while the runtime is alive: the SDK's router
loop ends, calls `Datastore::shutdown` → surrealkv `close`. So an
orderly exit must drop the handle and then wait, inside the runtime.

## Upgrading agmem mid-session kills that session's memory tools (lesson, tag role:ops)

`brew upgrade agmem` followed by any attach from the new binary retires the
old daemon, which closes the socket under every session still attached —
including the Claude Code session doing the upgrade, whose MCP server is
the old binary. Its tools vanish for the rest of the session (Claude Code
does not reconnect). Run /checkpoint before upgrading, and restart the
session after.

## Debug log level propagates to the spawned daemon (lesson, entities agmem, surrealkv)

`AGMEM_LOG=debug agmem context` passes `--log debug` to the daemon it
spawns, and at debug surrealkv logs one line per replayed WAL batch on
open: 169,559 lines / 25 MB in one second on the dogfood store, and that
daemon keeps debug for its whole life. Never set debug on a client that may
spawn a daemon; start the daemon by hand with the level instead. Side
observation worth an issue if startup slows: the dogfood store replays
169,559 WAL batches from one segment at every open (WAL never rotated).

## A retired daemon is a zombie until its spawner exits (lesson, entities agmem)

The daemon is spawned as a child (process_group(0)) and, before PR #125,
never waited on; after it exits it stays a zombie for as long as the
session that started it lives, so `kill -0 <pid>`, `ps` state ZN and any
pid-liveness check say "alive" for a process that has released everything.
PR #125 reaps it in a thread. A pid from agmem.lock is only a hint.

## Reproducing with an old release (reference)

`gh release download v0.1.7 -R the userMate/agmem -p '*aarch64-apple-darwin*'`
gives a tarball whose `agmem-server-aarch64-apple-darwin/agmem` runs as the
old daemon; `(sleep 900) | old-agmem --data T …` keeps a session attached.
