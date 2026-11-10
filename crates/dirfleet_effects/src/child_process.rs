//! Child processes that die with their parent.
//!
//! A Windows job object (or a process-tree kill elsewhere) takes the whole
//! tree down on cancel, and reader threads never block on a pipe a
//! grandchild kept open.
//!
//! Media previews (`media::process`) and plugins both start tools
//! through this.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long reader threads may take to finish after the child exits.
pub const READER_GRACE: Duration = Duration::from_secs(1);
/// Windows: run without opening a console window.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The child's whole process tree, so a launcher's descendants die with it.
pub struct ProcessTree {
    #[cfg(windows)]
    job: Option<crate::windows_media::KillOnCloseJob>,
}

impl ProcessTree {
    #[cfg_attr(not(windows), allow(clippy::missing_const_for_fn))]
    pub fn adopt(child: &std::process::Child) -> Self {
        #[cfg(windows)]
        {
            Self {
                job: crate::windows_media::KillOnCloseJob::for_child(child),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Self {}
        }
    }

    #[cfg_attr(not(windows), allow(clippy::unused_self))]
    pub fn kill(&self, child: &mut std::process::Child) {
        #[cfg(windows)]
        {
            if let Some(job) = &self.job {
                job.terminate();
            } else {
                kill_tree_with_taskkill(child);
            }
        }
        #[cfg(not(windows))]
        kill_descendants(child);
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Fallback when the child couldn't be put in a job object.
#[cfg(windows)]
fn kill_tree_with_taskkill(child: &std::process::Child) {
    use std::os::windows::process::CommandExt as _;
    let executable = std::env::var_os("SystemRoot").map_or_else(
        || std::path::PathBuf::from("taskkill.exe"),
        |root| std::path::PathBuf::from(root).join("System32/taskkill.exe"),
    );
    if let Ok(mut killer) = Command::new(executable)
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let deadline = Instant::now() + READER_GRACE;
        while matches!(killer.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = killer.kill();
        let _ = killer.wait();
    }
}

#[cfg(not(windows))]
fn kill_descendants(child: &std::process::Child) {
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

/// Waits for a reader thread, but not forever: a grandchild that inherited
/// the pipe can keep it open after the child is gone.
pub fn join_reader<T: Send + 'static>(
    reader: thread::JoinHandle<T>,
    grace_until: Instant,
) -> Option<T> {
    while !reader.is_finished() {
        if Instant::now() >= grace_until {
            log::warn!("child process output reader did not finish; leaving it behind");
            return None;
        }
        thread::sleep(Duration::from_millis(5));
    }
    reader.join().ok()
}

/// Why a process could not run to the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The program doesn't exist.
    NotFound(String),
    /// It exists but couldn't start (an antivirus block, permissions).
    Spawn(String),
    Cancelled,
    Timeout,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(program) => write!(f, "program not found: {program}"),
            Self::Spawn(error) => write!(f, "could not start the program: {error}"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Timeout => write!(f, "timed out"),
        }
    }
}

/// A finished process.
#[derive(Debug, Clone, Default)]
pub struct Output {
    /// The exit code; `None` when it was ended by a signal.
    pub code: Option<i32>,
    /// Standard output, up to the limit.
    pub stdout: String,
    /// The end of standard error, up to the limit.
    pub stderr: String,
}

/// How to run a process.
pub struct Run<'a> {
    pub cancel: &'a AtomicBool,
    pub deadline: Option<Instant>,
    pub stdin: Option<Vec<u8>>,
    /// Bytes of stdout and of stderr kept.
    pub output_limit: usize,
    /// Called with each stdout line, on the calling thread, as it arrives.
    pub on_line: Option<&'a mut dyn FnMut(&str)>,
}

/// Runs `command` to the end, streaming stdout lines to `on_line`. The
/// process tree is killed on cancel or at the deadline.
///
/// # Errors
/// When the command can't start, is cancelled, runs past its deadline or
/// writes more than its limit.
#[allow(clippy::too_many_lines)]
pub fn run(command: &mut Command, options: Run<'_>) -> Result<Output, RunError> {
    let Run {
        cancel,
        deadline,
        stdin,
        output_limit,
        mut on_line,
    } = options;
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RunError::NotFound(command.get_program().to_string_lossy().into_owned())
        } else {
            RunError::Spawn(error.to_string())
        }
    })?;
    let tree = ProcessTree::adopt(&child);
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        tree.kill(&mut child);
        return Err(RunError::Spawn("no output pipes".into()));
    };
    let (lines_tx, lines) = mpsc::channel::<String>();
    let output_reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        while let Ok(count) = reader.read_until(b'\n', &mut line) {
            if count == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&line)
                .trim_end_matches(['\r', '\n'])
                .to_owned();
            line.clear();
            if lines_tx.send(text).is_err() {
                break;
            }
        }
    });
    let error_reader = thread::spawn(move || {
        let mut retained = Vec::new();
        let mut stderr = stderr;
        let mut buffer = [0; 4096];
        while let Ok(count) = stderr.read(&mut buffer) {
            if count == 0 {
                break;
            }
            retained.extend_from_slice(&buffer[..count]);
            // Keep the most recent output.
            if retained.len() > 2 * output_limit {
                retained.drain(..retained.len() - output_limit);
            }
        }
        if retained.len() > output_limit {
            retained.drain(..retained.len() - output_limit);
        }
        String::from_utf8_lossy(&retained).into_owned()
    });
    let mut stdout = String::new();
    let mut take_line = |line: String, stdout: &mut String| {
        if let Some(on_line) = on_line.as_mut() {
            on_line(&line);
        }
        if stdout.len() + line.len() < output_limit {
            stdout.push_str(&line);
            stdout.push('\n');
        }
    };
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            tree.kill(&mut child);
            break Err(RunError::Cancelled);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            tree.kill(&mut child);
            break Err(RunError::Timeout);
        }
        while let Ok(line) = lines.try_recv() {
            take_line(line, &mut stdout);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if let Ok(line) = lines.recv_timeout(Duration::from_millis(15)) {
                    take_line(line, &mut stdout);
                }
            }
            Err(error) => {
                tree.kill(&mut child);
                break Err(RunError::Spawn(error.to_string()));
            }
        }
    };
    // The child is done; anything it started is not needed any more.
    #[cfg_attr(not(windows), allow(clippy::drop_non_drop))]
    drop(tree);
    let grace_until = Instant::now() + READER_GRACE;
    let _ = join_reader(output_reader, grace_until);
    while let Ok(line) = lines.try_recv() {
        take_line(line, &mut stdout);
    }
    let stderr = join_reader(error_reader, grace_until).unwrap_or_default();
    let status = status?;
    Ok(Output {
        code: status.code(),
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", script]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            command
        }
    }

    #[test]
    fn streams_lines_and_exit_code() {
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let mut on_line = |line: &str| seen.push(line.to_owned());
        let output = run(
            &mut shell("echo one&& echo two&& exit 3"),
            Run {
                cancel: &cancel,
                deadline: None,
                stdin: None,
                output_limit: 1024,
                on_line: Some(&mut on_line),
            },
        )
        .expect("runs");
        assert_eq!(output.code, Some(3));
        assert_eq!(output.stdout, "one\ntwo\n");
        assert_eq!(seen, ["one", "two"]);
    }

    #[test]
    fn missing_program_and_timeout() {
        let cancel = AtomicBool::new(false);
        let options = || Run {
            cancel: &cancel,
            deadline: Some(Instant::now() + Duration::from_millis(300)),
            stdin: None,
            output_limit: 1024,
            on_line: None,
        };
        assert!(matches!(
            run(&mut Command::new("dirfleet-no-such-program"), options()),
            Err(RunError::NotFound(_))
        ));
        let slow = if cfg!(windows) {
            "ping -n 10 127.0.0.1 > NUL"
        } else {
            "sleep 10"
        };
        let started = Instant::now();
        assert_eq!(
            run(&mut shell(slow), options()).err(),
            Some(RunError::Timeout)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
