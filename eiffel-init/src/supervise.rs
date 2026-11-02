//! Spawning the supervised programs, reaping everything, and shutting down.

use crate::sys;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

const DEFAULT_PATH: &str = "/sbin:/usr/sbin:/bin:/usr/bin";
/// How long children get to flush after SIGTERM before the enclave reboots.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Start every program, then reap until one of them exits.
pub fn run(programs: &[String], env: &[(String, String)]) -> ! {
    let mut children: HashMap<Pid, &str> = HashMap::new();
    for path in programs {
        children.insert(spawn(path, env), path.as_str());
    }

    // One waitpid(-1) owns all reaping: orphans get reparented here too, and a
    // Child::wait() elsewhere would steal statuses from this loop.
    loop {
        match waitpid(None, None) {
            Ok(status) => {
                let Some(pid) = status.pid() else { continue };
                match children.get(&pid) {
                    Some(path) => {
                        log!("exited {path} pid {pid} {}", describe(&status));
                        shutdown();
                    }
                    None => log!("reaped orphan pid {pid} {}", describe(&status)),
                }
            }
            // Nothing left to wait for: every child is already gone.
            Err(Errno::ECHILD) => {
                log!("no children left");
                shutdown();
            }
            Err(e) => fatal!("waitpid: {e}"),
        }
    }
}

/// stdio is inherited: fds 0-2 already point at the console, so there is nothing
/// to pipe and no copy threads to fail.
fn spawn(path: &str, env: &[(String, String)]) -> Pid {
    let mut command = Command::new(path);
    command.env_clear().envs(env.iter().cloned());
    if !env.iter().any(|(key, _)| key == "PATH") {
        command.env("PATH", DEFAULT_PATH);
    }
    sys::unblock_signals_and_setsid(&mut command);

    match command.spawn() {
        // Only the pid is kept; the Child is dropped without ever being waited on.
        Ok(child) => {
            let pid = Pid::from_raw(child.id() as i32);
            log!("started {path} pid {pid}");
            pid
        }
        Err(e) => fatal!("spawn {path}: {e}"),
    }
}

fn describe(status: &WaitStatus) -> String {
    match status {
        WaitStatus::Exited(_, code) => format!("status {code}"),
        WaitStatus::Signaled(_, signal, _) => format!("signal {signal}"),
        other => format!("{other:?}"),
    }
}

/// SIGTERM everything, give it a moment, then reboot. The enclave has no other
/// way down: PID 1 returning would panic the kernel.
fn shutdown() -> ! {
    log!("shutting down: SIGTERM, then reboot in {SHUTDOWN_GRACE:?}");
    if let Err(e) = kill(Pid::from_raw(-1), Signal::SIGTERM) {
        log!("kill(-1, SIGTERM): {e}");
    }

    // Signals are blocked, so nothing would interrupt a sleep; poll instead.
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    while Instant::now() < deadline {
        match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => std::thread::sleep(Duration::from_millis(20)),
            Ok(_) => {}
            Err(Errno::ECHILD) => break,
            Err(e) => {
                log!("waitpid during shutdown: {e}");
                break;
            }
        }
    }
    crate::reboot_now()
}
