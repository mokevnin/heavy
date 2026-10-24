//! A machine-wide counting semaphore for heavy commands.
//!
//! A [`Pool`] is a directory with one file per slot. Holding a slot is holding an exclusive `flock`
//! (`LockFileEx` on Windows) on its file, so the operating system frees the slot when the holder
//! dies, however it dies, and every process on the machine that opens the same directory shares the
//! same slots, whatever checkout or tool it comes from.
//!
//! ```no_run
//! use heavy::Pool;
//!
//! let pool = Pool::open("/tmp/heavy/default", 4)?;
//! let slot = pool.acquire(|| eprintln!("all slots are busy, waiting"))?;
//! // ... run the heavy command; the slot is released when `slot` is dropped.
//! drop(slot);
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! The queue is not fair: whoever tries first after a slot frees takes it.

use std::collections::hash_map::RandomState;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long to wait between two rounds over the slots, before the jitter.
const POLL: Duration = Duration::from_millis(250);

/// A pool of slots in a directory.
#[derive(Debug, Clone)]
pub struct Pool {
    dir: PathBuf,
    slots: usize,
}

/// A held slot. The slot is released when the guard is dropped, or when the process ends.
#[derive(Debug)]
pub struct Guard {
    file: File,
    slot: usize,
}

/// What [`Pool::status`] says about one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotStatus {
    /// The index of the slot, from 0.
    pub slot: usize,
    /// Who holds the slot, or `None` when it is free.
    pub holder: Option<Holder>,
}

/// The process that holds a slot, as it described itself when it took the slot.
///
/// This is for display only: the lock, not this record, says whether a slot is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The process id of the holder.
    pub pid: u32,
    /// Seconds since the Unix epoch at which the slot was taken.
    pub since: u64,
    /// What the holder said it runs.
    pub command: String,
}

impl Pool {
    /// Opens the pool in `dir` with `slots` slots, creating the directory and the slot files.
    ///
    /// # Errors
    ///
    /// Fails when `slots` is zero or the directory or a slot file cannot be created.
    pub fn open(dir: impl Into<PathBuf>, slots: usize) -> io::Result<Pool> {
        let dir = dir.into();
        if slots == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a pool needs at least one slot",
            ));
        }
        fs::create_dir_all(&dir)?;
        for slot in 0..slots {
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(slot_path(&dir, slot))?;
        }
        Ok(Pool { dir, slots })
    }

    /// The number of slots.
    #[must_use]
    pub fn slots(&self) -> usize {
        self.slots
    }

    /// The directory of the pool.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Takes a free slot if there is one.
    ///
    /// # Errors
    ///
    /// Fails when a slot file cannot be opened or locked for a reason other than being held.
    pub fn try_acquire(&self) -> io::Result<Option<Guard>> {
        for slot in 0..self.slots {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(slot_path(&self.dir, slot))?;
            match file.try_lock() {
                Ok(()) => return Ok(Some(Guard { file, slot })),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
        Ok(None)
    }

    /// Waits for a free slot and takes it. `on_wait` is called once, when the first round finds
    /// every slot held.
    ///
    /// # Errors
    ///
    /// Fails when a slot file cannot be opened or locked for a reason other than being held.
    pub fn acquire(&self, on_wait: impl FnOnce()) -> io::Result<Guard> {
        let mut on_wait = Some(on_wait);
        loop {
            if let Some(guard) = self.try_acquire()? {
                return Ok(guard);
            }
            if let Some(on_wait) = on_wait.take() {
                on_wait();
            }
            thread::sleep(POLL + jitter(POLL));
        }
    }

    /// Like [`Pool::acquire`], but gives up after `timeout` and returns `None`.
    ///
    /// # Errors
    ///
    /// Fails when a slot file cannot be opened or locked for a reason other than being held.
    pub fn acquire_timeout(
        &self,
        timeout: Duration,
        on_wait: impl FnOnce(),
    ) -> io::Result<Option<Guard>> {
        let deadline = Instant::now() + timeout;
        let mut on_wait = Some(on_wait);
        loop {
            if let Some(guard) = self.try_acquire()? {
                return Ok(Some(guard));
            }
            if let Some(on_wait) = on_wait.take() {
                on_wait();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            thread::sleep((POLL + jitter(POLL)).min(left));
        }
    }

    /// Reports every slot: free, or held and by whom.
    ///
    /// # Errors
    ///
    /// Fails when a slot file cannot be opened or locked for a reason other than being held.
    pub fn status(&self) -> io::Result<Vec<SlotStatus>> {
        (0..self.slots)
            .map(|slot| {
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(slot_path(&self.dir, slot))?;
                let holder = match file.try_lock() {
                    Ok(()) => {
                        file.unlock()?;
                        None
                    }
                    Err(TryLockError::WouldBlock) => Some(read_holder(&mut file)?),
                    Err(TryLockError::Error(error)) => return Err(error),
                };
                Ok(SlotStatus { slot, holder })
            })
            .collect()
    }
}

impl Guard {
    /// The index of the slot, from 0.
    #[must_use]
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// Records who holds the slot, for [`Pool::status`]. The record is for display only.
    ///
    /// # Errors
    ///
    /// Fails when the slot file cannot be written.
    pub fn describe(&mut self, command: &str) -> io::Result<()> {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let command = command.replace(['\n', '\r'], " ");
        self.file.set_len(0)?;
        self.file.rewind()?;
        write!(self.file, "{}\n{since}\n{command}\n", std::process::id())?;
        self.file.flush()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Clear the record first: a free slot must not look held to a reader of the file. The
        // lock is released when the file is closed right after, so an error here changes nothing.
        let _ = self.file.set_len(0);
        let _ = self.file.unlock();
    }
}

fn slot_path(dir: &Path, slot: usize) -> PathBuf {
    dir.join(format!("slot-{slot}"))
}

fn read_holder(file: &mut File) -> io::Result<Holder> {
    let mut text = String::new();
    file.rewind()?;
    file.read_to_string(&mut text)?;
    let mut lines = text.lines();
    let pid = lines.next().and_then(|line| line.parse().ok()).unwrap_or(0);
    let since = lines.next().and_then(|line| line.parse().ok()).unwrap_or(0);
    let command = lines.next().unwrap_or_default().to_owned();
    Ok(Holder {
        pid,
        since,
        command,
    })
}

/// A random duration below `max`, so processes that wait together do not poll together.
fn jitter(max: Duration) -> Duration {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    let max = u64::try_from(max.as_millis()).unwrap_or(u64::MAX).max(1);
    Duration::from_millis(hasher.finish() % max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_without_slots_is_refused() {
        let dir = tempfile::tempdir().unwrap();

        let error = Pool::open(dir.path(), 0).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn two_slots_are_two_different_holders_and_a_third_finds_none() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open(dir.path(), 2).unwrap();

        let first = pool.try_acquire().unwrap().unwrap();
        let second = pool.try_acquire().unwrap().unwrap();

        assert_ne!(first.slot(), second.slot());
        assert!(pool.try_acquire().unwrap().is_none());
    }

    #[test]
    fn dropping_a_guard_frees_its_slot() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open(dir.path(), 1).unwrap();
        let guard = pool.try_acquire().unwrap().unwrap();
        assert!(pool.try_acquire().unwrap().is_none());

        drop(guard);

        assert!(pool.try_acquire().unwrap().is_some());
    }

    #[test]
    fn another_pool_on_the_same_directory_shares_the_slots() {
        let dir = tempfile::tempdir().unwrap();
        let here = Pool::open(dir.path(), 1).unwrap();
        let there = Pool::open(dir.path(), 1).unwrap();
        let _held = here.try_acquire().unwrap().unwrap();

        assert!(there.try_acquire().unwrap().is_none());
    }

    #[test]
    fn a_timeout_gives_up_when_every_slot_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open(dir.path(), 1).unwrap();
        let _held = pool.try_acquire().unwrap().unwrap();
        let mut waited = false;

        let got = pool
            .acquire_timeout(Duration::from_millis(300), || waited = true)
            .unwrap();

        assert!(got.is_none());
        assert!(waited);
    }

    #[test]
    fn status_says_who_holds_a_slot_and_that_a_free_slot_is_free() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open(dir.path(), 2).unwrap();
        let mut held = pool.try_acquire().unwrap().unwrap();
        held.describe("go test ./...").unwrap();

        let status = pool.status().unwrap();

        let holder = status[held.slot()].holder.clone().unwrap();
        assert_eq!(holder.pid, std::process::id());
        assert_eq!(holder.command, "go test ./...");
        assert!(status[1 - held.slot()].holder.is_none());
    }

    #[test]
    fn a_released_slot_leaves_no_record_behind() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open(dir.path(), 1).unwrap();
        let mut held = pool.try_acquire().unwrap().unwrap();
        held.describe("make").unwrap();

        drop(held);

        assert_eq!(pool.status().unwrap()[0].holder, None);
    }
}
