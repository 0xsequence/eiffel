//! `eiffel-init` — PID 1 inside the enclave.
//!
//! Replaces AWS's `init.c` and eiffel's Go `pid1` with one binary: it brings up the
//! console, answers the heartbeat nitro-cli waits for, switches into the customer
//! rootfs, mounts the usual filesystem set, sets the clock from the host's and then
//! supervises the programs listed in `/cmd`. When the first of them exits, the
//! enclave reboots.
//!
//! The console is the only debugging channel an enclave has, so every failure path
//! logs one line explaining itself before acting.

#![deny(unsafe_code)]

use nix::errno::Errno;
use nix::fcntl::{OFlag, open};
use nix::mount::{MsFlags, mount};
use nix::sys::reboot::{RebootMode, reboot};
use nix::sys::signal::SigSet;
use nix::sys::socket::{AddressFamily, SockFlag, SockType, VsockAddr, connect, socket};
use nix::sys::stat::Mode;
use nix::unistd::{chdir, chroot, dup2_stderr, dup2_stdin, dup2_stdout, read, write};
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Write one line to the console. Unbuffered, and failures are ignored: there is
/// nowhere else to report that reporting failed.
pub fn log_line(args: std::fmt::Arguments) {
    let _ = writeln!(std::io::stderr(), " eiffel-init:  {args}");
}

macro_rules! log {
    ($($arg:tt)*) => { $crate::log_line(format_args!($($arg)*)) };
}

/// Log a reason and reboot the enclave. Used for every unrecoverable failure.
macro_rules! fatal {
    ($($arg:tt)*) => {{
        $crate::log_line(format_args!($($arg)*));
        $crate::reboot_now()
    }};
}

mod config;
mod mounts;
mod supervise;
mod sys;
mod time_sync;

/// The parent instance, which nitro-cli runs on.
const PARENT_CID: u32 = 3;
const HEARTBEAT_PORT: u32 = 9000;
const HEARTBEAT: u8 = 0xB7;
/// The vsock device can finish probing a few milliseconds after init starts.
const HEARTBEAT_RETRY: Duration = Duration::from_secs(5);

fn main() {
    // 1. Block every signal. PID 1 must not die from one, and a blocked SIGCHLD
    //    cannot interrupt a syscall; waitpid works regardless of the mask.
    if let Err(e) = SigSet::all().thread_block() {
        fatal!("sigprocmask(SIG_BLOCK, all): {e}");
    }

    // 2. /dev and the console, before anything that could need explaining.
    mount_devtmpfs();
    open_console();

    // 3. Nothing to do for the NSM: the driver is built into the kernel and
    //    devtmpfs creates /dev/nsm for us.

    // 4. Tell nitro-cli the enclave is alive. It blocks on this, so it comes
    //    before any slow work.
    heartbeat();

    // 5. What to run, and with which environment.
    let cmd = config::read("/cmd").unwrap_or_else(|e| fatal!("{e}"));
    let env = config::read("/env").unwrap_or_else(|e| fatal!("{e}"));
    let programs = config::parse_cmd(&cmd).unwrap_or_else(|e| fatal!("{e}"));
    let env = config::parse_env(&env).unwrap_or_else(|e| fatal!("{e}"));

    // 6. and 7. Become the customer rootfs and furnish it.
    switch_root();
    mount_devtmpfs();
    mounts::apply();

    // 7b. Set the clock from the host's before anything can observe it, so no
    //     process ever sees time jump backwards.
    time_sync::start();

    // 8., 9. and 10.
    supervise::run(&programs, &env)
}

/// `reboot(2)` only returns on failure, and there is nothing left to try then.
pub fn reboot_now() -> ! {
    let _ = reboot(RebootMode::RB_AUTOBOOT);
    std::process::exit(1)
}

/// Mount devtmpfs on `/dev`. Done once before the console is open and again after
/// the switch into `/rootfs`, which left the first one behind with the old root.
fn mount_devtmpfs() {
    let flags = MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC;
    match mount(Some("dev"), "/dev", Some("devtmpfs"), flags, None::<&str>) {
        Ok(()) => {}
        // CONFIG_DEVTMPFS_MOUNT=y, so the kernel has usually done it already.
        Err(Errno::EBUSY) => {}
        Err(e) => fatal!("mount devtmpfs on /dev: {e}"),
    }
}

/// Point fds 0, 1 and 2 at `/dev/console`, which is where everything is logged from
/// here on — ours and every child's.
fn open_console() {
    let console = match open(
        "/dev/console",
        OFlag::O_RDWR | OFlag::O_NOCTTY,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(e) => fatal!("open /dev/console: {e}"),
    };
    let dups = [
        ("stdin", dup2_stdin(&console)),
        ("stdout", dup2_stdout(&console)),
        ("stderr", dup2_stderr(&console)),
    ];
    for (name, result) in dups {
        if let Err(e) = result {
            fatal!("dup2 /dev/console onto {name}: {e}");
        }
    }
}

/// Send one `0xB7` to the parent and require the same byte back. nitro-cli waits
/// for this and reports the enclave as failed if it never arrives.
fn heartbeat() {
    let addr = VsockAddr::new(PARENT_CID, HEARTBEAT_PORT);
    let deadline = Instant::now() + HEARTBEAT_RETRY;
    let fd = loop {
        match connect_parent(&addr) {
            Ok(fd) => break fd,
            Err(Errno::ENODEV | Errno::ECONNREFUSED) if Instant::now() < deadline => {
                sleep(Duration::from_millis(50));
            }
            Err(e) => fatal!("heartbeat: connect to cid {PARENT_CID} port {HEARTBEAT_PORT}: {e}"),
        }
    };

    match write(&fd, &[HEARTBEAT]) {
        Ok(1) => {}
        Ok(n) => fatal!("heartbeat: wrote {n} bytes, want 1"),
        Err(e) => fatal!("heartbeat: write: {e}"),
    }
    let mut buf = [0u8; 1];
    match read(&fd, &mut buf) {
        Ok(1) => {}
        Ok(n) => fatal!("heartbeat: read {n} bytes, want 1"),
        Err(e) => fatal!("heartbeat: read: {e}"),
    }
    if buf[0] != HEARTBEAT {
        fatal!(
            "heartbeat: parent echoed {:#04x}, want {HEARTBEAT:#04x}",
            buf[0]
        );
    }
    log!("heartbeat accepted by cid {PARENT_CID} port {HEARTBEAT_PORT}");
}

/// A fresh socket per attempt: a stream socket whose connect failed cannot be reused.
fn connect_parent(addr: &VsockAddr) -> Result<OwnedFd, Errno> {
    let fd = socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )?;
    connect(fd.as_raw_fd(), addr)?;
    Ok(fd)
}

/// Make `/rootfs` the root, exactly as `init.c` does it.
fn switch_root() {
    // The bootstrap layout ships these too; either source is fine.
    for dir in [
        "/rootfs/dev",
        "/rootfs/proc",
        "/rootfs/sys",
        "/rootfs/run",
        "/rootfs/tmp",
        "/rootfs/var",
    ] {
        mounts::mkdir(dir);
    }

    // Turn /rootfs into a mount point so it can be moved, then overmount / with it.
    if let Err(e) = mount(
        Some("/rootfs"),
        "/rootfs",
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    ) {
        fatal!("mount --bind /rootfs /rootfs: {e}");
    }
    if let Err(e) = chdir("/rootfs") {
        fatal!("chdir /rootfs: {e}");
    }
    if let Err(e) = mount(Some("."), "/", None::<&str>, MsFlags::MS_MOVE, None::<&str>) {
        fatal!("mount --move . /: {e}");
    }
    if let Err(e) = chroot(".") {
        fatal!("chroot .: {e}");
    }
    if let Err(e) = chdir("/") {
        fatal!("chdir /: {e}");
    }
}
