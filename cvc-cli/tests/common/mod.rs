//! Shared fixtures for tests that drive the `cvc` binary: an isolated
//! repository with a local bare remote, and a pseudo-terminal session that
//! makes the binary's consent challenges answerable from a test.
#![allow(dead_code)]

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const WAIT_LIMIT: Duration = Duration::from_secs(60);

pub struct Fixture {
    pub temp: TempDir,
    pub home: PathBuf,
    pub repo: PathBuf,
    /// A bare repository registered as `origin`, so publication can be
    /// exercised end to end without a network.
    pub remote: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let remote = temp.path().join("remote.git");
        fs::create_dir(&home).unwrap();
        fs::create_dir(home.join("templates")).unwrap();
        fs::create_dir(&repo).unwrap();
        git2::Repository::init_bare(&remote).unwrap();
        let fixture = Self {
            temp,
            home,
            repo,
            remote,
        };
        fixture.git(&["init"]);
        fixture.git(&["config", "user.email", "test@example.invalid"]);
        fixture.git(&["config", "user.name", "CVC test"]);
        let remote = fixture.remote.to_str().unwrap().to_owned();
        fixture.git(&["remote", "add", "origin", &remote]);
        fs::write(fixture.repo.join("tracked"), "initial\n").unwrap();
        fixture.git(&["add", "tracked"]);
        fixture.git(&["commit", "-m", "initial"]);
        // Advertise one ref: git2's `Remote::list` trips a debug-build
        // pointer check on a remote with no refs at all.
        fixture.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
        fixture
    }

    /// A command isolated from the developer's own Git and CVC configuration.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", self.home.join("templates"))
            .stdin(Stdio::null());
        command
    }

    pub fn git(&self, args: &[&str]) {
        let output = self.command("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub fn cvc_command(&self, args: &[&str]) -> Command {
        let mut command = self.command(env!("CARGO_BIN_EXE_cvc"));
        command.args(args);
        command
    }

    pub fn cvc(&self, args: &[&str]) -> Output {
        self.cvc_command(args).output().unwrap()
    }

    pub fn cvc_ok(&self, args: &[&str]) -> String {
        let output = self.cvc(args);
        assert!(
            output.status.success(),
            "cvc {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    pub fn open_repo(&self) -> git2::Repository {
        git2::Repository::open(&self.repo).unwrap()
    }

    /// Grants destination sharing consent for `origin` directly through the
    /// library, the way the interactive challenge would have recorded it.
    pub fn consent_to_sharing(&self) -> cvc_core::privacy::RemoteDestination {
        let repo = self.open_repo();
        let destination = cvc_core::privacy::remote_destination(&repo, "origin").unwrap();
        cvc_core::privacy::acknowledge_sharing_destination(&repo, &destination).unwrap();
        destination
    }

    /// The id of the single `cvc run` conversation captured so far.
    pub fn only_run_conversation_id(&self) -> String {
        let listing = self.cvc_ok(&["conversations"]);
        let ids: Vec<_> = listing
            .split_whitespace()
            .filter(|token| token.starts_with("run-"))
            .collect();
        assert_eq!(ids.len(), 1, "{listing}");
        ids[0].to_owned()
    }
}

/// A child process whose stdin, stdout, and stderr are the slave side of a
/// pseudo-terminal that is also its controlling terminal, exactly as when a
/// human runs it from a shell.
pub struct PtySession {
    child: Child,
    master: File,
    output: Arc<Mutex<Vec<u8>>>,
}

impl PtySession {
    /// Spawns `command` on a fresh pty. `queued_input` is written to the
    /// terminal *before* the process starts, which is what type-ahead or a
    /// paste looks like from the process's side.
    pub fn spawn(mut command: Command, queued_input: &[u8]) -> Self {
        let (mut master, slave) = openpty();
        master.write_all(queued_input).unwrap();
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // SAFETY: the hook only calls async-signal-safe syscalls that make the
        // pty the child's controlling terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        // Dropping the command releases the parent's slave descriptors, so
        // the master sees hangup once the child exits.
        drop(command);
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().unwrap();
        let sink = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink.lock().unwrap().extend_from_slice(&chunk[..n]),
                }
            }
        });
        Self {
            child,
            master,
            output,
        }
    }

    /// Types `bytes` at the terminal.
    pub fn write(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    /// Everything the process has shown so far, including echoed input.
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            let output = self.output();
            if done(&output) {
                return output;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                // Give the reader a moment to drain what was written last.
                std::thread::sleep(Duration::from_millis(100));
                let output = self.output();
                if done(&output) {
                    return output;
                }
                panic!("process exited ({status}) before {what}:\n{output}");
            }
            assert!(
                Instant::now() < deadline,
                "timed out before {what}:\n{}",
                self.output()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until the `n`th typed-acknowledgement prompt has been displayed
    /// and returns the challenge it demands.
    pub fn wait_for_challenge(&mut self, n: usize) -> String {
        let output = self.wait_until(&format!("challenge #{n}"), |output| {
            challenges(output).len() >= n
        });
        challenges(&output)[n - 1].clone()
    }

    /// Waits until `needle` has been displayed.
    pub fn wait_for_text(&mut self, needle: &str) -> String {
        self.wait_until(&format!("{needle:?}"), |output| output.contains(needle))
    }

    /// Waits for exit and returns the status with the full terminal output.
    pub fn wait_exit(mut self) -> (ExitStatus, String) {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                // Drop our master handle only after the reader thread has had
                // the chance to see the hangup and drain the last output.
                std::thread::sleep(Duration::from_millis(100));
                return (status, self.output());
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("process did not exit:\n{}", self.output());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Every challenge string prompted so far, in display order.
fn challenges(output: &str) -> Vec<String> {
    const LEAD: &str = "Type exactly '";
    const TRAIL: &str = "' to continue: ";
    output
        .match_indices(LEAD)
        .filter_map(|(at, _)| {
            let rest = &output[at + LEAD.len()..];
            rest.find(TRAIL).map(|end| rest[..end].to_owned())
        })
        .collect()
}

fn openpty() -> (File, File) {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: both out-pointers are valid; the optional arguments may be null.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", io::Error::last_os_error());
    // SAFETY: openpty returned two fresh descriptors that we now own.
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}
