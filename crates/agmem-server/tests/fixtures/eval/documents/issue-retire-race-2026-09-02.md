---
title: A release takeover races the retiring daemon's store lock, and the loser dies without a word
---

## What happened

Upgrading the Homebrew binary 0.1.7 → 0.1.8 on 2026-09-02, the first `agmem context` one-shot retired the 0.1.7 daemon (the PR #87 path) and then hung for 120 s. The daemon.log for the whole exchange:

```
10:06:46.155705Z  WARN  refused a session from another release refusal=the running daemon is agmem 0.1.7 and this session is agmem 0.1.8; …
10:06:46.157436Z  INFO  another release attached; retiring so its binary can serve attached=1
10:06:46.215190Z  INFO  agmem: agmem starting space=agmem db=surrealkv:///…/agmem.db
```

Nothing after that. `agmem.lock` held the pid of the new daemon (31276), which was dead; no socket; the old daemon was gone. Three minutes later a second one-shot found a stale lock, started a 0.1.8 daemon normally, and was answered in one second.

## Reproduced

With the v0.1.7 release binary on a copy of the store: a 0.1.7 session attached to a 0.1.7 daemon, then a 0.1.8 `agmem context`.

Run 1, lost the race:

```
10:15:05.781835Z  WARN refused a session from another release …
10:15:05.781914Z  INFO another release attached; retiring so its binary can serve attached=1
10:15:05.789207Z  INFO agmem: agmem starting …
          (nothing more)
client: 10:15:05.782467Z started the shared store takeover=FromRetired
client: Error: the shared store did not come up within 120s. Its log is …
agmem.db/LOCK afterwards: empty
```

Run 2, won it: `took over from a retired daemon` 0.3 s after the retire line; the one-shot answered.

In both runs the retiring daemon was a zombie within one second of the retire line, so its process exits promptly. The client, already polling at 50 ms, spawned the replacement **0.5 ms** after the retire.

## Mechanism

Two locks, released at different moments:

1. `agmem.lock` is the `DataDirLock` guard local to `serve::run`. It is released when `run` returns.
2. `agmem.db/LOCK` is surrealkv's own exclusive flock (`lockfile.rs`, `try_lock_exclusive`). It is released when the `Datastore` is dropped, which happens during the tokio runtime teardown after `main` returns, or at process exit.

`client::wait_for_store_lock` documents lock 1 as "what says its process is actually gone". It is not: there is a window of a few milliseconds where lock 1 is free and lock 2 is still held. A client that was already polling for the retire lands in that window, spawns a daemon, and that daemon's `connect_with` fails on lock 2.

Two things then make the failure invisible:

- `daemon::serve::run`'s error goes to `main`'s `Err` return, which prints to stderr. The spawned daemon's stderr is `Stdio::null()`. Nothing reaches `daemon.log`. Confirmed directly: starting a second `--daemon-serve` by hand against a held store prints the lock error on the terminal and leaves only `agmem starting` in the log.
- `client::wait_until_ready` polls the socket for `READY_DEADLINE` (120 s) and never looks at the child it spawned, so a daemon that died in 10 ms costs the session two minutes.

The empty `LOCK` file after the lost race is surrealkv truncating the file before the failed `try_lock_exclusive`, so the old pid is wiped and `holder_hint`-style diagnostics have nothing to point at.

Side observation: the daemon is reaped by nobody (spawned with `process_group(0)`, never `wait`ed), so after retire it stays a zombie until the session that started it exits. `kill -0 <pid>` and any pid-liveness check say "alive" for a process that has already released everything.

## Fix shape

Three independent pieces, in order of payoff:

1. **The advisory lock must outlive the store.** Hold the `DataDirLock` outside the async runtime for `--daemon-serve` (acquire before `block_on`, drop after the runtime is dropped), so lock 1 is released strictly after lock 2. Then `wait_for_store_lock` means what it says and the race is gone.
2. **Log the startup error.** The `--daemon-serve` branch in `main` should `tracing::error!` the failure before returning, so a daemon that dies at startup leaves its reason in `daemon.log`.
3. **Fail fast on a dead child.** Keep the `Child` from `spawn` and `try_wait` it inside `wait_until_ready`; a daemon that exited is a bail with the log path, not a 120 s wait. Optionally retry the spawn once while the store lock settles.

Reaping the zombie (`try_wait` the child on detach, or double-fork) is a fourth, cosmetic, piece.

## Ships in

First release after v0.1.8. The 0.1.8 client side (piece 3) helps every future takeover; piece 1 helps only takeovers *from* the release that carries it, so the 0.1.8 → 0.1.9 upgrade can still lose the race.
