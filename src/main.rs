//! `heavy`: run a command while holding one of a few machine-wide slots.

use std::cell::Cell;
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use heavy::{Holder, Pool};

/// Run a command while holding one of a few machine-wide slots, so many checkouts and agents
/// starting heavy commands together take turns instead of starving each other.
///
/// A command started inside a held slot (it sees `HEAVY_HELD`) runs at once, so nesting never
/// deadlocks, and so does any command when CI is set, unless --no-skip-ci is given.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(flatten)]
    pool: PoolArgs,

    /// Run commands at once when the CI environment variable is set, without a slot.
    #[arg(long, env = "HEAVY_NO_SKIP_CI")]
    no_skip_ci: bool,

    /// Stop waiting after this many seconds without a slot (default: wait as long as it takes).
    #[arg(long, value_name = "SECONDS", env = "HEAVY_TIMEOUT")]
    timeout: Option<u64>,

    /// What to do when --timeout runs out: fail with exit status 75, or run the command without
    /// a slot.
    #[arg(long, value_enum, env = "HEAVY_ON_TIMEOUT", default_value = "fail")]
    on_timeout: OnTimeout,

    /// Append a JSON line about every queued run to this file: how long it waited and ran.
    #[arg(long, value_name = "FILE", env = "HEAVY_LOG")]
    log: Option<PathBuf>,

    /// Show who holds the slots of the pool, instead of running a command.
    #[arg(long)]
    status: bool,

    /// The command to run, after `--`.
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<OsString>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OnTimeout {
    /// Exit with status 75 without running the command.
    Fail,
    /// Run the command without a slot.
    Run,
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
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("heavy: {error}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: &Cli) -> std::io::Result<u8> {
    if cli.status {
        return status(&cli.pool);
    }
    let Some((program, args)) = cli.command.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no command given: heavy [OPTIONS] -- COMMAND [ARGS]... (or heavy --status)",
        ));
    };

    let held = env::var("HEAVY_HELD").is_ok_and(|held| held == cli.pool.pool);
    let ci = !cli.no_skip_ci && env::var_os("CI").is_some_and(|ci| !ci.is_empty());
    if held || ci {
        return spawn(program, args, &cli.pool.pool, None);
    }

    let pool = cli.pool.open()?;
    let words: Vec<String> = std::iter::once(program)
        .chain(args)
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    let command = words.join(" ");
    let mut record = Record {
        log: cli.log.as_deref(),
        start: unix_now(),
        pool: &cli.pool.pool,
        command: &command,
        slot: None,
        waited: Duration::ZERO,
        ran: None,
        timed_out: false,
    };

    let started = Instant::now();
    let waited = Cell::new(false);
    let on_wait = || {
        waited.set(true);
        print_busy(&pool, &cli.pool.pool);
    };
    let slot = match cli.timeout {
        Some(seconds) => pool.acquire_timeout(Duration::from_secs(seconds), on_wait)?,
        None => Some(pool.acquire(on_wait)?),
    };
    record.waited = started.elapsed();

    let Some(mut slot) = slot else {
        record.timed_out = true;
        if cli.on_timeout == OnTimeout::Fail {
            eprintln!(
                "heavy: gave up waiting for a slot after {}",
                seconds(record.waited)
            );
            record.write(75);
            return Ok(75);
        }
        eprintln!(
            "heavy: no slot of pool '{}' after {}, running without one",
            cli.pool.pool,
            seconds(record.waited)
        );
        let ran = Instant::now();
        let code = spawn(program, args, &cli.pool.pool, None)?;
        record.ran = Some(ran.elapsed());
        record.write(code);
        return Ok(code);
    };
    if waited.get() {
        eprintln!(
            "heavy: got slot {} after {}",
            slot.slot(),
            seconds(record.waited)
        );
    }
    slot.describe(&command)?;
    record.slot = Some(slot.slot());
    // The slot is held until `slot` is dropped, after the command ends; the command does not
    // inherit the file, so a heavy that is killed outright frees the slot at once.
    let ran = Instant::now();
    let code = spawn(program, args, &cli.pool.pool, record.slot)?;
    record.ran = Some(ran.elapsed());
    drop(slot);
    record.write(code);
    Ok(code)
}

/// Says that every slot is busy and who holds them.
fn print_busy(pool: &Pool, name: &str) {
    eprintln!(
        "heavy: all {} slots of pool '{name}' are busy, waiting for one",
        pool.slots()
    );
    let now = unix_now();
    for slot in pool.status().unwrap_or_default() {
        if let Some(holder) = slot.holder {
            eprintln!("heavy:   slot {}: {}", slot.slot, held_by(&holder, now));
        }
    }
}

fn spawn(
    program: &OsString,
    args: &[OsString],
    pool: &str,
    slot: Option<usize>,
) -> std::io::Result<u8> {
    let mut command = Command::new(program);
    command.args(args).env("HEAVY_HELD", pool);
    if let Some(slot) = slot {
        command.env("HEAVY_SLOT", slot.to_string());
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("heavy: {}: {error}", program.to_string_lossy());
            return Ok(127);
        }
    };
    forward_signals(&child);
    let status = child.wait()?;
    CHILD.store(0, Ordering::SeqCst);
    Ok(exit_code(status))
}

/// One queued run, for the log: how long it waited for a slot and how long it ran.
struct Record<'a> {
    log: Option<&'a Path>,
    start: u64,
    pool: &'a str,
    command: &'a str,
    slot: Option<usize>,
    waited: Duration,
    ran: Option<Duration>,
    timed_out: bool,
}

impl Record<'_> {
    /// Appends the record to the log as one JSON line. A log that cannot be written is reported
    /// and does not change the exit status.
    fn write(&self, exit: u8) {
        let Some(path) = self.log else { return };
        let millis = |duration: Duration| duration.as_millis().to_string();
        let mut line = String::from("{");
        let _ = write!(line, "\"start\":{}", self.start);
        let _ = write!(line, ",\"pool\":{}", json_string(self.pool));
        let _ = write!(line, ",\"command\":{}", json_string(self.command));
        let cwd =
            env::current_dir().map_or_else(|_| String::new(), |cwd| cwd.display().to_string());
        let _ = write!(line, ",\"cwd\":{}", json_string(&cwd));
        let _ = write!(
            line,
            ",\"slot\":{}",
            self.slot
                .map_or_else(|| "null".to_owned(), |slot| slot.to_string())
        );
        let _ = write!(line, ",\"waited_ms\":{}", millis(self.waited));
        let _ = write!(
            line,
            ",\"ran_ms\":{}",
            self.ran.map_or_else(|| "null".to_owned(), millis)
        );
        let _ = write!(line, ",\"timed_out\":{}", self.timed_out);
        let _ = write!(line, ",\"exit\":{exit}");
        line.push_str("}\n");
        // One write of one line to a file opened for appending, so lines from many runs do not mix.
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        if let Err(error) = written {
            eprintln!("heavy: cannot write the log {}: {error}", path.display());
        }
    }
}

fn json_string(text: &str) -> String {
    let mut json = String::from("\"");
    for char in text.chars() {
        match char {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            char if u32::from(char) < 0x20 => {
                let _ = write!(json, "\\u{:04x}", u32::from(char));
            }
            char => json.push(char),
        }
    }
    json.push('"');
    json
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn seconds(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

fn held_by(holder: &Holder, now: u64) -> String {
    if holder.pid == 0 {
        // Taken a moment ago: the holder has not written its record yet.
        return "held, just taken".to_owned();
    }
    format!(
        "held by pid {} for {}s: {}",
        holder.pid,
        now.saturating_sub(holder.since),
        holder.command
    )
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
fn exit_code(status: std::process::ExitStatus) -> u8 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => u8::try_from(code & 0xff).unwrap_or(1),
        (None, Some(signal)) => u8::try_from(128 + signal).unwrap_or(1),
        (None, None) => 1,
    }
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> u8 {
    u8::try_from(status.code().unwrap_or(1) & 0xff).unwrap_or(1)
}

fn status(args: &PoolArgs) -> std::io::Result<u8> {
    let pool = args.open()?;
    let now = unix_now();
    println!("pool '{}' in {}", args.pool, pool.dir().display());
    for slot in pool.status()? {
        match slot.holder {
            None => println!("slot {}: free", slot.slot),
            Some(holder) => println!("slot {}: {}", slot.slot, held_by(&holder, now)),
        }
    }
    Ok(0)
}
