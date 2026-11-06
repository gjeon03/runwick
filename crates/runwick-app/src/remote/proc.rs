//! Running short helper programs (`ssh`, `ssh-keyscan`, `tailscale`…) with a time limit.
//! Output is read on separate threads so a chatty program never blocks on a full pipe.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Output {
    /// Exit code; `None` when killed by a signal or by the time limit.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub elapsed: Duration,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// Run `cmd` (stdin from `input`, or closed) and wait at most `limit`.
/// `Err` only when the program could not be started (for example it is not installed).
pub fn run(mut cmd: Command, input: Option<&[u8]>, limit: Duration) -> std::io::Result<Output> {
    let started = Instant::now();
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let mut timed_out = false;
    let code = loop {
        match child.try_wait()? {
            Some(status) => break status.code(),
            None if started.elapsed() >= limit => {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(15)),
        }
    };
    let take = |h: Option<std::thread::JoinHandle<String>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(Output { code, stdout: take(out), stderr: take(err), timed_out, elapsed: started.elapsed() })
}

fn drain<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.by_ref().take(1 << 20).read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_stops_slow_programs() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "echo out; echo err >&2; exit 3"]);
        let o = run(c, None, Duration::from_secs(5)).unwrap();
        assert_eq!((o.code, o.stdout.trim(), o.stderr.trim(), o.timed_out), (Some(3), "out", "err", false));

        let mut c = Command::new("/bin/sh");
        c.args(["-c", "cat"]);
        let o = run(c, Some(b"hello"), Duration::from_secs(5)).unwrap();
        assert_eq!(o.stdout, "hello");

        let mut c = Command::new("/bin/sh");
        c.args(["-c", "sleep 5"]);
        let o = run(c, None, Duration::from_millis(100)).unwrap();
        assert!(o.timed_out && o.code.is_none());
        assert!(o.elapsed < Duration::from_secs(2));

        assert!(run(Command::new("/nonexistent/runwick-test"), None, Duration::from_secs(1)).is_err());
    }
}
