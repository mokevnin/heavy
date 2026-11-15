//! The `heavy` binary against real processes: queueing, kill -9, nesting, exit codes.
//!
//! Every command runs through `sh -c`, so the tests need a POSIX `sh` in `PATH`: on Windows, the
//! one Git for Windows installs.

use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// A directory of slots of its own, and a `heavy` command that uses one slot of it.
struct Slots {
    dir: TempDir,
}

impl Slots {
    fn new() -> Slots {
        Slots {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn heavy(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_heavy"));
        command
            .env("HEAVY_DIR", self.dir.path())
            .env("HEAVY_POOL", "test")
            .env("HEAVY_SLOTS", "1")
            .env_remove("HEAVY_HELD")
            .env_remove("HEAVY_SLOT")
            .env_remove("HEAVY_NO_SKIP_CI")
            .env_remove("HEAVY_TIMEOUT")
            .env_remove("HEAVY_ON_TIMEOUT")
            .env_remove("HEAVY_LOG")
            .env_remove("CI");
        command
    }

    /// Starts `heavy -- <command>` and returns once it holds the slot.
    fn hold(&self, command: &[&str]) -> Child {
        let child = self
            .heavy()
            .arg("--")
            .args(command)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.status().contains("held by") {
            assert!(Instant::now() < deadline, "the holder never took the slot");
            sleep(Duration::from_millis(20));
        }
        child
    }

    fn status(&self) -> String {
        let output = self.heavy().arg("--status").output().unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    fn run(command: &mut Command) -> (Output, Duration) {
        let started = Instant::now();
        let output = command.output().unwrap();
        (output, started.elapsed())
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn it_returns_the_exit_code_of_the_command() {
    let slots = Slots::new();

    let output = slots
        .heavy()
        .args(["--", "sh", "-c", "exit 3"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn a_command_that_exits_75_is_told_from_a_timeout_by_stderr_only() {
    let slots = Slots::new();

    let output = slots
        .heavy()
        .args(["--", "sh", "-c", "exit 75"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(75));
    assert!(!stderr(&output).contains("gave up waiting"));
}

#[test]
fn a_command_that_does_not_exist_exits_127() {
    let slots = Slots::new();

    let output = slots
        .heavy()
        .args(["--", "no-such-command-in-heavy-tests"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(127));
    assert!(stderr(&output).contains("no-such-command-in-heavy-tests"));
}

#[cfg(unix)]
#[test]
fn a_command_that_dies_of_a_signal_exits_128_plus_the_signal() {
    let slots = Slots::new();

    let output = slots
        .heavy()
        .args(["--", "sh", "-c", "kill -TERM $$"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(143));
}

#[test]
fn no_command_is_an_error() {
    let slots = Slots::new();

    let output = slots.heavy().output().unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no command given"));
}

#[test]
fn the_command_sees_its_pool_and_its_slot() {
    let slots = Slots::new();

    let output = slots
        .heavy()
        .args(["--", "sh", "-c", "echo $HEAVY_HELD $HEAVY_SLOT"])
        .output()
        .unwrap();

    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "test 0");
}

#[test]
fn a_second_heavy_waits_until_the_first_is_done() {
    let slots = Slots::new();
    let mut holder = slots.hold(&["sh", "-c", "sleep 2"]);

    let (output, took) = Slots::run(slots.heavy().args(["--", "sh", "-c", "exit 0"]));

    assert!(output.status.success());
    assert!(stderr(&output).contains("all 1 slots of pool 'test' are busy"));
    assert!(took >= Duration::from_secs(1), "waited only {took:?}");
    holder.wait().unwrap();
}

#[test]
fn a_waiting_heavy_says_who_holds_the_slots_and_how_long_it_waited() {
    let slots = Slots::new();
    let mut holder = slots.hold(&["sh", "-c", "sleep 2"]);

    let (output, _) = Slots::run(slots.heavy().args(["--", "sh", "-c", "exit 0"]));

    let stderr = stderr(&output);
    assert!(output.status.success());
    assert!(stderr.contains("slot 0: held by pid"), "{stderr}");
    assert!(stderr.contains("sleep 2"), "{stderr}");
    assert!(stderr.contains("got slot 0 after"), "{stderr}");
    holder.wait().unwrap();
}

#[test]
fn a_heavy_that_does_not_wait_says_nothing() {
    let slots = Slots::new();

    let output = slots
        .heavy()
        .args(["--", "sh", "-c", "exit 0"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(stderr(&output), "");
}

#[test]
fn on_timeout_run_runs_the_command_without_a_slot() {
    let slots = Slots::new();
    let mut holder = slots.hold(&["sh", "-c", "sleep 4"]);

    let (output, took) = Slots::run(slots.heavy().args([
        "--timeout",
        "1",
        "--on-timeout",
        "run",
        "--",
        "sh",
        "-c",
        "echo \"[$HEAVY_SLOT]\"; exit 3",
    ]));

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "[]");
    assert!(stderr.contains("running without one"), "{stderr}");
    assert!(!stderr.contains("gave up waiting"), "{stderr}");
    assert!(took < Duration::from_secs(3), "took {took:?}");
    holder.kill().unwrap();
    holder.wait().unwrap();
}

#[test]
fn the_log_has_a_line_for_every_queued_run() {
    let slots = Slots::new();
    let log = slots.dir.path().join("runs.jsonl");
    let mut holder = slots.hold(&["sh", "-c", "sleep 3"]);

    let (timed_out, _) = Slots::run(slots.heavy().env("HEAVY_LOG", &log).args([
        "--timeout",
        "1",
        "--",
        "echo",
        "a \"quoted\" word",
    ]));
    holder.wait().unwrap();
    let (ran, _) = Slots::run(
        slots
            .heavy()
            .env("HEAVY_LOG", &log)
            .args(["--", "sh", "-c", "exit 2"]),
    );

    assert_eq!(timed_out.status.code(), Some(75));
    assert_eq!(ran.status.code(), Some(2));
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].contains(r#""command":"echo a \"quoted\" word""#),
        "{text}"
    );
    assert!(lines[0].contains(r#""slot":null"#), "{text}");
    assert!(lines[0].contains(r#""ran_ms":null"#), "{text}");
    assert!(lines[0].contains(r#""timed_out":true"#), "{text}");
    assert!(lines[0].contains(r#""exit":75"#), "{text}");
    assert!(lines[1].contains(r#""slot":0"#), "{text}");
    assert!(lines[1].contains(r#""timed_out":false"#), "{text}");
    assert!(lines[1].contains(r#""exit":2"#), "{text}");
}

#[test]
fn a_heavy_killed_outright_frees_its_slot_at_once() {
    let slots = Slots::new();
    let mut holder = slots.hold(&["sh", "-c", "sleep 4"]);
    holder.kill().unwrap();
    holder.wait().unwrap();

    let (output, took) = Slots::run(slots.heavy().args(["--", "sh", "-c", "exit 0"]));

    assert!(output.status.success());
    assert!(!stderr(&output).contains("busy"), "{}", stderr(&output));
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

#[test]
fn a_command_started_inside_a_held_slot_runs_at_once() {
    let slots = Slots::new();
    let mut holder = slots.hold(&["sh", "-c", "sleep 3"]);

    let (output, took) = Slots::run(
        slots
            .heavy()
            .env("HEAVY_HELD", "test")
            .args(["--", "sh", "-c", "exit 0"]),
    );

    assert!(output.status.success());
    assert!(took < Duration::from_secs(2), "took {took:?}");
    holder.kill().unwrap();
    holder.wait().unwrap();
}

#[test]
fn another_pool_has_slots_of_its_own() {
    let slots = Slots::new();
    let mut holder = slots.hold(&["sh", "-c", "sleep 3"]);

    let (output, took) = Slots::run(
        slots
            .heavy()
            .env("HEAVY_POOL", "other")
            .env("HEAVY_HELD", "test")
            .args(["--", "sh", "-c", "exit 0"]),
    );

    assert!(output.status.success());
    assert!(!stderr(&output).contains("busy"));
    assert!(took < Duration::from_secs(2), "took {took:?}");
    holder.kill().unwrap();
    holder.wait().unwrap();
}

#[test]
fn ci_skips_the_queue_unless_asked_not_to() {
    let slots = Slots::new();
    let mut holder = slots.hold(&["sh", "-c", "sleep 3"]);

    let (skipped, took) = Slots::run(
        slots
            .heavy()
            .env("CI", "1")
            .args(["--", "sh", "-c", "exit 0"]),
    );
    let (queued, _) = Slots::run(slots.heavy().env("CI", "1").args([
        "--no-skip-ci",
        "--timeout",
        "1",
        "--",
        "sh",
        "-c",
        "exit 0",
    ]));

    assert!(skipped.status.success());
    assert!(took < Duration::from_secs(2), "took {took:?}");
    assert_eq!(queued.status.code(), Some(75));
    assert!(stderr(&queued).contains("gave up waiting"));
    holder.kill().unwrap();
    holder.wait().unwrap();
}

#[test]
fn status_says_who_holds_a_slot() {
    let slots = Slots::new();
    assert!(slots.status().contains("slot 0: free"));
    let mut holder = slots.hold(&["sh", "-c", "sleep 3"]);

    let status = slots.status();

    assert!(status.contains("slot 0: held by pid"), "{status}");
    assert!(status.contains("sleep 3"), "{status}");
    holder.kill().unwrap();
    holder.wait().unwrap();
}

#[test]
fn options_come_before_or_after_the_status_flag() {
    let slots = Slots::new();

    let before = slots
        .heavy()
        .args(["--slots", "2", "--pool", "other", "--status"])
        .output()
        .unwrap();
    let after = slots
        .heavy()
        .args(["--status", "--slots", "2", "--pool", "other"])
        .output()
        .unwrap();

    for output in [before, after] {
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(text.contains("pool 'other'"), "{text}");
        assert!(text.contains("slot 1: free"), "{text}");
    }
}

#[test]
fn a_command_named_status_after_the_separator_is_run() {
    let slots = Slots::new();

    let output = slots
        .heavy()
        .args(["--", "sh", "-c", "echo status"])
        .output()
        .unwrap();

    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "status");
}

#[cfg(windows)]
#[test]
fn a_windows_exit_code_is_not_cut_to_eight_bits() {
    let slots = Slots::new();

    let output = slots
        .heavy()
        .args(["--", "cmd", "/c", "exit 256"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(256));
}

#[cfg(windows)]
#[test]
fn a_bare_name_runs_the_batch_file_it_stands_for() {
    let slots = Slots::new();
    let bin = tempfile::tempdir().unwrap();
    std::fs::write(bin.path().join("heavy-test-shim.cmd"), "@echo shim %1\r\n").unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.path().to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();

    let output = slots
        .heavy()
        .env("PATH", path)
        .args(["--", "heavy-test-shim", "works"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "shim works");
}
