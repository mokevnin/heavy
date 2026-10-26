# heavy

A machine-wide counting semaphore for heavy commands.

Ten coding agents, a few checkouts and a CI-like `check` task all start `go test`, `tsc` and
`golangci-lint` at once, and the machine stops answering. `heavy` gives every such command one of
a few **slots** shared by everything on the machine: a command that finds all slots busy waits for
one, so the heavy commands take turns instead of starving each other.

```sh
heavy -- go test ./...
heavy -- pnpm exec tsc --noEmit
heavy --status
```

## Install

```sh
brew install mokevnin/tap/heavy        # Homebrew
mise use -g github:mokevnin/heavy      # mise
cargo install heavy                    # from source
curl -LsSf https://github.com/mokevnin/heavy/releases/latest/download/heavy-installer.sh | sh
```

The binaries are built for macOS and Linux, on arm64 and x86_64.

## How it behaves

- **N slots.** `--slots N` or `HEAVY_SLOTS`; the default is a quarter of the cores, at least one.
- **Shared by every process on the machine.** A slot is an exclusive `flock` on a file in
  `$TMPDIR/heavy/<pool>` (`--dir` or `HEAVY_DIR` moves it). Every checkout, every agent and every
  tool that opens the same directory shares the slots.
- **Released however the holder ends.** The operating system drops a `flock` when its holder dies,
  `kill -9` included, so a crashed run never leaves a slot taken. The command does not inherit the
  lock: a `heavy` that is killed outright frees its slot at once while its command, if still
  running, is no longer counted.
- **Waits and says so.** When every slot is busy it prints
  `heavy: all N slots of pool 'default' are busy, waiting for one` once and waits.
  `--timeout SECONDS` gives up with exit status 75 instead.
- **Nesting never deadlocks.** The command runs with `HEAVY_HELD=<pool>` and `HEAVY_SLOT=<n>`; a
  `heavy` of the same pool started inside it runs its command at once. A task that calls another
  heavy task is therefore safe.
- **CI skips the queue.** A CI machine is its own; with `CI` set the command runs at once.
  `--no-skip-ci` turns that off.
- **Pools.** `--pool NAME` (`HEAVY_POOL`) keeps pools apart: they do not share slots.
- **Exit status.** The command's own; `128 + n` when a signal `n` ended it; 127 when it does not
  exist; 75 on a timeout (a command that exits 75 itself looks the same: a timeout is the one
  that prints `gave up waiting` on stderr). `SIGINT`, `SIGTERM` and `SIGHUP` sent to `heavy` reach the command.
- **`heavy --status`** shows who holds each slot (pid, how long, the command). The record is for
  display only; the lock is the source of truth.

The queue is **not fair**: when a slot frees, the first waiter to look takes it (they look every
quarter of a second, with jitter). The slot files must be on a local file system.

## As a library

```rust
use heavy::Pool;

let pool = Pool::open("/tmp/heavy/default", 4)?;
let slot = pool.acquire(|| eprintln!("all slots are busy, waiting"))?;
// run the heavy work; the slot is released when `slot` is dropped
drop(slot);
```

`Pool::try_acquire` takes a free slot or returns `None`, `Pool::acquire_timeout` gives up after a
duration, `Pool::status` reports every slot. The library is synchronous and has no dependencies;
build it without the command line with `default-features = false`.

## Why not ...

| | |
| --- | --- |
| `flock(1)` | one lock, not N slots; not on macOS without Homebrew's util-linux |
| GNU `parallel --semaphore` (`sem`) | needs Perl, and a nested call deadlocks |
| the GNU make jobserver | a holder killed with `SIGKILL` loses its token, and only make, ninja and cargo speak it |
| a daemon queue (`pueue`, `nq`) | a service to run and restart; `heavy` has no state beyond the lock files |

## License

Apache-2.0.
