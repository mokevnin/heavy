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
- **Waits and says who it waits for.** When every slot is busy it says so once, with who holds
  each slot, and when it gets one, how long it waited:

  ```
  heavy: all 2 slots of pool 'default' are busy, waiting for one
  heavy:   slot 0: held by pid 81234 for 252s: go test ./...
  heavy:   slot 1: held by pid 81302 for 40s: pnpm exec tsc --noEmit
  heavy: got slot 1 after 37.2s
  ```

  A command that finds a free slot prints nothing.
- **Waits as long as you let it.** `--timeout SECONDS` (`HEAVY_TIMEOUT`) stops waiting; then
  `--on-timeout fail` (the default) exits with status 75 and `--on-timeout run`
  (`HEAVY_ON_TIMEOUT`) runs the command anyway, without a slot, so a busy machine slows a check
  down but never fails it.
- **Logs every wait.** `--log FILE` (`HEAVY_LOG`) appends one JSON line per queued run, to see
  how long commands wait and whether the number of slots is right:

  ```json
  {"start":1761510000,"pool":"default","command":"go test ./...","cwd":"/src/app","slot":1,"waited_ms":37210,"ran_ms":81022,"timed_out":false,"exit":0}
  ```

  `slot` and `ran_ms` are `null` when the run timed out without running. Runs that skip the queue
  (nested or on CI) are not logged.
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
| a lock file in a script | N lock files are easy; the rest is the work: freeing a slot on `kill -9`, nesting without a deadlock, passing on signals and exit codes, saying who holds the slots and how long a command waited |
| `flock(1)` | one lock, not N slots; not on macOS without Homebrew's util-linux |
| GNU `parallel --semaphore` (`sem`) | needs Perl, and a nested call deadlocks |
| the GNU make jobserver | a holder killed with `SIGKILL` loses its token, and only make, ninja and cargo speak it |
| a daemon queue (`pueue`, `nq`) | a service to run and restart; `heavy` has no state beyond the lock files |

## License

Apache-2.0.
