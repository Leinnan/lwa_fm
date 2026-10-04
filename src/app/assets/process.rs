//! Bounded, cancellable subprocesses shared by posters, probes, and overview samples.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const STDERR_LIMIT: usize = 64 * 1024;
static VIDEO_SLOT: Mutex<usize> = Mutex::new(0);
#[cfg(test)]
pub(super) static BENCHMARK_PROCESS_LIMIT: AtomicUsize = AtomicUsize::new(1);
#[cfg(test)]
fn process_limit() -> usize {
    BENCHMARK_PROCESS_LIMIT.load(Ordering::Acquire)
}
#[cfg(not(test))]
const fn process_limit() -> usize {
    1
}
static VIDEO_AVAILABLE: Condvar = Condvar::new();
pub(super) static PROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(super) static PROCESS_MICROS: AtomicU64 = AtomicU64::new(0);
pub(super) static PERMIT_WAIT_MICROS: AtomicU64 = AtomicU64::new(0);
pub(super) static OUTPUT_BYTES: AtomicU64 = AtomicU64::new(0);
struct ProcessTimer(Instant);
impl Drop for ProcessTimer {
    fn drop(&mut self) {
        PROCESS_MICROS.fetch_add(self.0.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ErrorKind {
    Cancelled,
    Timeout,
    Unavailable,
    Unsupported,
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
        while *busy >= process_limit() {
            check(cancel, deadline)?;
            busy = VIDEO_AVAILABLE
                .wait_timeout(busy, Duration::from_millis(10))
                .expect("video slot wait")
                .0;
        }
        check(cancel, deadline)?;
        *busy += 1;
        drop(busy);
        Ok(Self)
    }
}
impl Drop for VideoPermit {
    fn drop(&mut self) {
        *VIDEO_SLOT.lock().expect("video slot mutex") -= 1;
        VIDEO_AVAILABLE.notify_one();
    }
}

fn kill_and_reap(child: &mut std::process::Child) {
    // A PATH executable can be a launcher. Stop descendants before closing pipe readers.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let executable = std::env::var_os("SystemRoot").map_or_else(
            || std::path::PathBuf::from("taskkill.exe"),
            |root| std::path::PathBuf::from(root).join("System32/taskkill.exe"),
        );
        if let Ok(mut killer) = Command::new(executable)
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let deadline = Instant::now() + Duration::from_secs(1);
            while matches!(killer.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let _ = killer.kill();
            let _ = killer.wait();
        }
    }
    #[cfg(not(windows))]
    {
        let mut system = sysinfo::System::new();
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        let mut descendants = vec![sysinfo::Pid::from_u32(child.id())];
        let mut index = 0;
        while index < descendants.len() {
            let parent = descendants[index];
            descendants.extend(
                system
                    .processes()
                    .iter()
                    .filter(|(_, process)| process.parent() == Some(parent))
                    .map(|(pid, _)| *pid),
            );
            index += 1;
        }
        for pid in descendants.into_iter().rev() {
            if let Some(process) = system.process(pid) {
                process.kill();
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(super) fn run(
    command: &mut Command,
    cancel: &AtomicBool,
    deadline: Instant,
    output_limit: usize,
) -> Result<Vec<u8>, MediaError> {
    run_capture(command, cancel, deadline, output_limit).map(|output| output.bytes)
}

pub(super) struct Output {
    pub bytes: Vec<u8>,
    pub stderr: String,
}

pub(super) fn run_capture(
    command: &mut Command,
    cancel: &AtomicBool,
    deadline: Instant,
    output_limit: usize,
) -> Result<Output, MediaError> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::media_process");
    let waiting = Instant::now();
    let _permit = VideoPermit::acquire(cancel, deadline)?;
    PERMIT_WAIT_MICROS.fetch_add(waiting.elapsed().as_micros() as u64, Ordering::Relaxed);
    let _timer = ProcessTimer(Instant::now());
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
            kill_and_reap(&mut child);
            break Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                kill_and_reap(&mut child);
                break Err(MediaError::new(ErrorKind::InvalidMedia, error.to_string()));
            }
        }
    };
    let bytes = output_reader
        .join()
        .map_err(|_| MediaError::new(ErrorKind::InvalidMedia, "Media output reader panicked"))?
        .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
    OUTPUT_BYTES.fetch_add(bytes.len() as u64, Ordering::Relaxed);
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
    Ok(Output { bytes, stderr })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_command(action: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("asset fixture operation"));
        command
            .args([
                "--exact",
                "app::assets::process::tests::child_fixture",
                "--nocapture",
            ])
            .env("LWA_PROCESS_TEST_ACTION", action);
        command
    }
    #[test]
    fn child_fixture() {
        match std::env::var("LWA_PROCESS_TEST_ACTION").as_deref() {
            Ok("sleep") => thread::sleep(Duration::from_secs(5)),
            Ok("tree") => {
                let mut command = fixture_command("sleep");
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt as _;
                    command.creation_flags(0x0800_0000);
                }
                let mut child = command.spawn().expect("asset fixture operation");
                child.wait().expect("asset fixture operation");
            }
            Ok("output") => {
                let _ = std::io::stdout().write_all(&vec![b'x'; 1024 * 1024]);
            }
            _ => {}
        }
    }
    #[test]
    fn portable_cancellation_deadline_and_descendants_are_bounded() {
        for action in ["sleep", "tree"] {
            let started = Instant::now();
            let result = run(
                &mut fixture_command(action),
                &AtomicBool::new(false),
                started + Duration::from_millis(250),
                64 * 1024,
            );
            assert_eq!(
                result.expect_err("expected media failure").kind,
                ErrorKind::Timeout
            );
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{action}: {:?}",
                started.elapsed()
            );
        }
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let signal = std::sync::Arc::clone(&cancel);
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            signal.store(true, Ordering::Release);
        });
        assert_eq!(
            run(
                &mut fixture_command("sleep"),
                &cancel,
                Instant::now() + Duration::from_secs(2),
                64 * 1024
            )
            .expect_err("expected media failure")
            .kind,
            ErrorKind::Cancelled
        );
        trigger.join().expect("asset fixture operation");
    }
    #[test]
    fn portable_output_limit_is_enforced() {
        assert_eq!(
            run(
                &mut fixture_command("output"),
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(2),
                1024
            )
            .expect_err("expected media failure")
            .kind,
            ErrorKind::Limit
        );
    }
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
