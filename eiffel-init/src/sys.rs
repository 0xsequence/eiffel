//! The only module allowed to use `unsafe`.
//!
//! `pre_exec` runs between `fork` and `exec` in the child, where only
//! async-signal-safe functions may be called — which rules out most of `nix` and
//! leaves the raw libc calls below.

#![allow(unsafe_code)]

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Undo PID 1's signal mask in the child and give it its own session, as `init.c`
/// does before `execvpe`. A child that inherited the mask would ignore SIGTERM at
/// shutdown.
pub fn unblock_signals_and_setsid(command: &mut Command) {
    // SAFETY: the closure calls only sigfillset, sigprocmask and setsid, all of
    // which are async-signal-safe, and allocates nothing.
    unsafe {
        command.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            if libc::sigfillset(&mut set) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}
