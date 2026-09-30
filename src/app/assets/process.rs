//! Bounded, cancellable subprocesses shared by posters, probes, and overview samples.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const STDERR_LIMIT: usize = 16 * 1024;
static VIDEO_SLOT: Mutex<bool> = Mutex::new(false);
static VIDEO_AVAILABLE: Condvar = Condvar::new();
pub(super) static PROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ErrorKind {
    Cancelled,
    Timeout,
    Unavailable,
    InvalidMedia,
    Limit,
}

#[derive(Debug, Clone)]
pub(super) struct MediaError {
    pub kind: ErrorKind,
    pub message: String,
}

impl MediaError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

pub(super) fn check(cancel: &AtomicBool, deadline: Instant) -> Result<(), MediaError> {
    if cancel.load(Ordering::Acquire) {
        return Err(MediaError::new(ErrorKind::Cancelled, "Preview cancelled"));
    }
    if Instant::now() >= deadline {
        return Err(MediaError::new(
            ErrorKind::Timeout,
            "Preview deadline exceeded",
        ));
    }
    Ok(())
}

struct VideoPermit;
impl VideoPermit {
    fn acquire(cancel: &AtomicBool, deadline: Instant) -> Result<Self, MediaError> {
        let mut busy = VIDEO_SLOT.lock().expect("video slot mutex");
        while *busy {
            check(cancel, deadline)?;
            busy = VIDEO_AVAILABLE
                .wait_timeout(busy, Duration::from_millis(10))
                .expect("video slot wait")
                .0;
        }
        check(cancel, deadline)?;
        *busy = true;
        drop(busy);
        Ok(Self)
    }
}
impl Drop for VideoPermit {
    fn drop(&mut self) {
        *VIDEO_SLOT.lock().expect("video slot mutex") = false;
        VIDEO_AVAILABLE.notify_one();
    }
}

pub(super) fn run(
    command: &mut Command,
    cancel: &AtomicBool,
    deadline: Instant,
    output_limit: usize,
) -> Result<Vec<u8>, MediaError> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::media_process");
    let _permit = VideoPermit::acquire(cancel, deadline)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|err| {
        MediaError::new(
            ErrorKind::Unavailable,
            format!("Could not start media tool: {err}"),
        )
    })?;
    PROCESS_COUNT.fetch_add(1, Ordering::Relaxed);
    let stdout = child.stdout.take().expect("piped media stdout");
    let stderr = child.stderr.take().expect("piped media stderr");
    let output_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(output_limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let error_reader = thread::spawn(move || {
        let mut retained = Vec::new();
        let mut stderr = stderr;
        let mut buffer = [0; 4096];
        while let Ok(count) = stderr.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let room = STDERR_LIMIT.saturating_sub(retained.len());
            let _ = retained.write_all(&buffer[..count.min(room)]);
        }
        String::from_utf8_lossy(&retained).into_owned()
    });
    let status = loop {
        if let Err(error) = check(cancel, deadline) {
            let _ = child.kill();
            let _ = child.wait();
            break Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(MediaError::new(ErrorKind::InvalidMedia, error.to_string()));
            }
        }
    };
    let bytes = output_reader
        .join()
        .map_err(|_| MediaError::new(ErrorKind::InvalidMedia, "Media output reader panicked"))?
        .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
    let stderr = error_reader.join().unwrap_or_default();
    if bytes.len() > output_limit {
        return Err(MediaError::new(
            ErrorKind::Limit,
            "Media output exceeded its byte limit",
        ));
    }
    let status = status?;
    check(cancel, deadline)?;
    if !status.success() {
        return Err(MediaError::new(
            ErrorKind::InvalidMedia,
            if stderr.trim().is_empty() {
                "Media tool could not decode this source".into()
            } else {
                stderr.trim().to_owned()
            },
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn cancellation_and_deadline_kill_and_reap_child() {
        for cancelled in [true, false] {
            let cancel = AtomicBool::new(cancelled);
            let mut command = Command::new("/bin/sleep");
            command.arg("5");
            let started = Instant::now();
            let result = run(
                &mut command,
                &cancel,
                started + Duration::from_millis(60),
                100,
            );
            assert_eq!(
                result.expect_err("must interrupt").kind,
                if cancelled {
                    ErrorKind::Cancelled
                } else {
                    ErrorKind::Timeout
                }
            );
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
    #[cfg(unix)]
    #[test]
    fn output_is_bounded() {
        let mut command = Command::new("/usr/bin/yes");
        let result = run(
            &mut command,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(1),
            256,
        );
        assert_eq!(result.expect_err("output limit").kind, ErrorKind::Limit);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_interrupts_an_already_running_process() {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let signal = std::sync::Arc::clone(&cancel);
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            signal.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let result = run(
            Command::new("/bin/sleep").arg("5"),
            &cancel,
            started + Duration::from_secs(2),
            100,
        );
        trigger.join().expect("cancel trigger");
        assert_eq!(
            result.expect_err("cancel running child").kind,
            ErrorKind::Cancelled
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn process_budget_is_shared_across_concurrent_callers() {
        let started = Instant::now();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let barrier = std::sync::Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    run(
                        Command::new("/bin/sleep").arg("0.1"),
                        &AtomicBool::new(false),
                        Instant::now() + Duration::from_secs(2),
                        100,
                    )
                    .expect("serialized child");
                })
            })
            .collect();
        barrier.wait();
        for worker in workers {
            worker.join().expect("process worker");
        }
        assert!(started.elapsed() >= Duration::from_millis(190));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
