use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const POLL: Duration = Duration::from_millis(50);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(8);

/// Runs a command with captured output, killing it if it exceeds `timeout`.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Result<Output> {
    let label = describe(program, args);
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start {label}"))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let Some(status) = wait_timeout(&mut child, timeout)? else {
        let _ = child.kill();
        let _ = wait_timeout(&mut child, Duration::from_secs(5));
        bail!("{label} did not finish within {}s", timeout.as_secs());
    };
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Like [`run`], but a non-zero exit status is an error carrying the command's stderr.
pub fn run_ok(program: &str, args: &[&str], timeout: Duration) -> Result<Output> {
    let output = run(program, args, timeout)?;
    if !output.status.success() {
        bail!(
            "{} failed: {}",
            describe(program, args),
            output_text(&output)
        );
    }
    Ok(output)
}

pub fn output_text(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        stderr
    }
}

pub fn wait_timeout(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("failed to poll child process")? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL);
    }
}

pub fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Option<JoinHandle<Vec<u8>>> {
    pipe.map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    })
}

pub fn collect(handle: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

/// Stops a child without hanging. SIGTERM first, because `sudo` forwards it to the
/// root-owned `dd`, whereas SIGKILL would orphan `dd`.
pub fn terminate(child: &mut Child) {
    let pid = child.id().to_string();
    let _ = run("/bin/kill", &["-TERM", &pid], Duration::from_secs(5));
    if matches!(wait_timeout(child, Duration::from_secs(10)), Ok(Some(_))) {
        return;
    }
    let _ = child.kill();
    let _ = wait_timeout(child, Duration::from_secs(5));
}

pub fn retry<T>(label: &str, attempts: u32, op: impl FnMut(u32) -> Result<T>) -> Result<T> {
    retry_with(label, attempts, Duration::from_secs(1), op)
}

pub fn retry_with<T>(
    label: &str,
    attempts: u32,
    initial_delay: Duration,
    mut op: impl FnMut(u32) -> Result<T>,
) -> Result<T> {
    let mut delay = initial_delay;
    let mut attempt = 1;
    loop {
        match op(attempt) {
            Ok(value) => return Ok(value),
            Err(_) if attempt < attempts => {
                thread::sleep(delay);
                delay = (delay * 2).min(MAX_RETRY_DELAY);
                attempt += 1;
            }
            Err(error) => {
                return Err(error.context(format!("{label} failed after {attempts} attempts")));
            }
        }
    }
}

fn describe(program: &str, args: &[&str]) -> String {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    if args.is_empty() {
        name.to_string()
    } else {
        format!("{name} {}", args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_captures_output() {
        let output = run("/bin/echo", &["hello"], Duration::from_secs(5)).expect("echo");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    fn run_times_out_instead_of_hanging() {
        let started = Instant::now();
        let error = run("/bin/sleep", &["30"], Duration::from_millis(300)).expect_err("timeout");
        assert!(error.to_string().contains("did not finish"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn run_ok_reports_failure() {
        let error = run_ok("/bin/ls", &["/definitely/not/here"], Duration::from_secs(5))
            .expect_err("ls should fail");
        assert!(error.to_string().contains("ls /definitely/not/here failed"));
    }

    #[test]
    fn retry_succeeds_after_transient_failures() {
        let mut calls = 0;
        let value = retry_with("op", 3, Duration::ZERO, |_| {
            calls += 1;
            if calls < 3 {
                bail!("transient");
            }
            Ok(42)
        })
        .expect("third attempt succeeds");
        assert_eq!(value, 42);
        assert_eq!(calls, 3);
    }

    #[test]
    fn retry_gives_up_after_limit() {
        let mut calls = 0;
        let error = retry_with::<()>("op", 2, Duration::ZERO, |_| {
            calls += 1;
            bail!("still broken")
        })
        .expect_err("should give up");
        assert_eq!(calls, 2);
        assert!(format!("{error:#}").contains("failed after 2 attempts"));
    }
}
