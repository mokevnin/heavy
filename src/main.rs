//! `heavy`: run a command while holding one of a few machine-wide slots.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use heavy::Pool;

/// Run a command while holding one of a few machine-wide slots, so many checkouts and agents
/// starting heavy commands together take turns instead of starving each other.
///
/// A command started inside a held slot (it sees `HEAVY_HELD`) runs at once, so nesting never
/// deadlocks, and so does any command when CI is set, unless --no-skip-ci is given.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(flatten)]
    pool: PoolArgs,

    /// Run commands at once when the CI environment variable is set, without a slot.
    #[arg(long, env = "HEAVY_NO_SKIP_CI")]
    no_skip_ci: bool,

    /// Give up after this many seconds without a slot (default: wait as long as it takes).
    #[arg(long, value_name = "SECONDS", env = "HEAVY_TIMEOUT")]
    timeout: Option<u64>,

    #[command(subcommand)]
    subcommand: Option<Sub>,

    /// The command to run, after `--`.
    #[arg(trailing_var_arg = true, num_args = 1.., value_name = "COMMAND")]
    command: Vec<OsString>,
}

#[derive(clap::Args)]
struct PoolArgs {
    /// The name of the pool; pools do not share slots.
    #[arg(long, env = "HEAVY_POOL", default_value = "default")]
    pool: String,

    /// How many commands may run at once (default: a quarter of the cores, at least one).
    #[arg(long, env = "HEAVY_SLOTS")]
    slots: Option<usize>,

    /// The directory that holds the slot files of every pool (default: heavy in the temp directory).
    #[arg(long, env = "HEAVY_DIR")]
    dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Sub {
    /// Show who holds the slots of the pool.
    Status,
}

impl PoolArgs {
    fn open(&self) -> std::io::Result<Pool> {
        let slots = self.slots.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, |cores| (cores.get() / 4).max(1))
        });
        let dir = self
            .dir
            .clone()
            .unwrap_or_else(|| env::temp_dir().join("heavy"))
            .join(&self.pool);
        Pool::open(dir, slots)
    }
}

/// The pid of the running command, for the signal handler to pass signals to.
static CHILD: AtomicI32 = AtomicI32::new(0);

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("heavy: {error}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: &Cli) -> std::io::Result<ExitCode> {
    if let Some(Sub::Status) = cli.subcommand {
        return status(&cli.pool);
    }
    let Some((program, args)) = cli.command.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no command given: heavy [OPTIONS] -- COMMAND [ARGS]...",
        ));
    };

    let held = env::var("HEAVY_HELD").is_ok_and(|held| held == cli.pool.pool);
    let ci = !cli.no_skip_ci && env::var_os("CI").is_some_and(|ci| !ci.is_empty());
    if held || ci {
        return spawn(program, args, &cli.pool.pool, None);
    }

    let pool = cli.pool.open()?;
    let on_wait = || {
        eprintln!(
            "heavy: all {} slots of pool '{}' are busy, waiting for one",
            pool.slots(),
            cli.pool.pool
        );
    };
    let slot = match cli.timeout {
        Some(seconds) => pool.acquire_timeout(Duration::from_secs(seconds), on_wait)?,
        None => Some(pool.acquire(on_wait)?),
    };
    let Some(mut slot) = slot else {
        eprintln!("heavy: gave up waiting for a slot");
        return Ok(ExitCode::from(75));
    };
    let words: Vec<String> = std::iter::once(program)
        .chain(args)
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    slot.describe(&words.join(" "))?;
    let slot_index = slot.slot();
    // The slot is held until `slot` is dropped, after the command ends; the command does not
    // inherit the file, so a heavy that is killed outright frees the slot at once.
    spawn(program, args, &cli.pool.pool, Some(slot_index))
}

fn spawn(
    program: &OsString,
    args: &[OsString],
    pool: &str,
    slot: Option<usize>,
) -> std::io::Result<ExitCode> {
    let mut command = Command::new(program);
    command.args(args).env("HEAVY_HELD", pool);
    if let Some(slot) = slot {
        command.env("HEAVY_SLOT", slot.to_string());
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("heavy: {}: {error}", program.to_string_lossy());
            return Ok(ExitCode::from(127));
        }
    };
    forward_signals(&child);
    let status = child.wait()?;
    CHILD.store(0, Ordering::SeqCst);
    Ok(exit_code(status))
}

#[cfg(unix)]
fn forward_signals(child: &std::process::Child) {
    extern "C" fn pass_on(signal: libc::c_int) {
        let pid = CHILD.load(Ordering::SeqCst);
        if pid > 0 {
            // SAFETY: kill(2) is async-signal-safe and takes plain integers.
            unsafe {
                libc::kill(pid, signal);
            }
        }
    }
    CHILD.store(i32::try_from(child.id()).unwrap_or(0), Ordering::SeqCst);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only calls the async-signal-safe kill(2) and reads an atomic.
        unsafe {
            libc::signal(
                signal,
                pass_on as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
}

#[cfg(not(unix))]
fn forward_signals(_child: &std::process::Child) {}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> ExitCode {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => ExitCode::from(u8::try_from(code & 0xff).unwrap_or(1)),
        (None, Some(signal)) => ExitCode::from(u8::try_from(128 + signal).unwrap_or(1)),
        (None, None) => ExitCode::from(1),
    }
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> ExitCode {
    ExitCode::from(u8::try_from(status.code().unwrap_or(1) & 0xff).unwrap_or(1))
}

fn status(args: &PoolArgs) -> std::io::Result<ExitCode> {
    let pool = args.open()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    println!("pool '{}' in {}", args.pool, pool.dir().display());
    for slot in pool.status()? {
        match slot.holder {
            None => println!("slot {}: free", slot.slot),
            Some(holder) => println!(
                "slot {}: held by pid {} for {}s: {}",
                slot.slot,
                holder.pid,
                now.saturating_sub(holder.since),
                holder.command
            ),
        }
    }
    Ok(ExitCode::SUCCESS)
}
