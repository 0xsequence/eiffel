//! The only module allowed to use `unsafe`: the raw kernel interfaces.
//!
//! `pre_exec` runs between `fork` and `exec` in the child, where only
//! async-signal-safe functions may be called — which rules out most of `nix` and
//! leaves the raw libc calls below. The PTP ioctls and `adjtimex` are here for the
//! same reason: nothing above this module touches a raw pointer or a request number.

#![allow(unsafe_code)]

use nix::errno::Errno;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
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

/// `include/uapi/linux/ptp_clock.h`: `#define PTP_CLK_MAGIC '='`.
const PTP_CLK_MAGIC: u8 = b'=';
const NR_SYS_OFFSET2: u8 = 14;
const NR_SYS_OFFSET_PRECISE2: u8 = 17;

/// `PTP_MAX_SAMPLES` from the same header; it sizes `ptp_sys_offset.ts`, so it is
/// part of that ioctl's request number and may not be trimmed to what we ask for.
pub const PTP_MAX_SAMPLES: usize = 25;

/// `struct ptp_clock_time`. The seconds field carries the sign of the whole value;
/// nanoseconds are always positive.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct PtpClockTime {
    pub sec: i64,
    pub nsec: u32,
    pub reserved: u32,
}

/// `struct ptp_sys_offset_precise`, the answer to `PTP_SYS_OFFSET_PRECISE2`. Every
/// field is part of the ABI even where we never read it, and the size decides the
/// ioctl's request number, so none of them may be dropped.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
#[allow(dead_code)]
pub struct PtpSysOffsetPrecise {
    pub device: PtpClockTime,
    pub sys_realtime: PtpClockTime,
    pub sys_monoraw: PtpClockTime,
    pub rsv: [u32; 4],
}

/// `struct ptp_sys_offset`. The kernel fills `ts` with `2 * n_samples + 1`
/// interleaved system/PHC timestamps, starting and ending with a system one.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct PtpSysOffset {
    pub n_samples: u32,
    pub rsv: [u32; 3],
    pub ts: [PtpClockTime; 2 * PTP_MAX_SAMPLES + 1],
}

impl Default for PtpSysOffset {
    fn default() -> Self {
        Self {
            n_samples: 0,
            rsv: [0; 3],
            ts: [PtpClockTime::default(); 2 * PTP_MAX_SAMPLES + 1],
        }
    }
}

nix::ioctl_readwrite!(
    ptp_sys_offset_precise2,
    PTP_CLK_MAGIC,
    NR_SYS_OFFSET_PRECISE2,
    PtpSysOffsetPrecise
);
// The header declares this one `_IOW` even though the kernel writes the timestamps
// back into the buffer, so it has to be `ioctl_write_ptr!`: `ioctl_readwrite!` would
// derive a request number with the read bit set and the kernel would reject it.
nix::ioctl_write_ptr!(ptp_sys_offset2, PTP_CLK_MAGIC, NR_SYS_OFFSET2, PtpSysOffset);

/// One cross-timestamp of the PTP clock against `CLOCK_REALTIME`. `EOPNOTSUPP` means
/// the driver has no `getcrosststamp` and the caller must fall back.
pub fn sys_offset_precise(fd: BorrowedFd<'_>) -> nix::Result<PtpSysOffsetPrecise> {
    let mut arg = PtpSysOffsetPrecise::default();
    // SAFETY: the request number is derived from the struct being passed, and `arg`
    // is a live, initialised buffer of exactly that size.
    unsafe { ptp_sys_offset_precise2(fd.as_raw_fd(), &mut arg) }?;
    Ok(arg)
}

/// `n_samples` bracketed PHC reads. Capped at `PTP_MAX_SAMPLES`, which is all the
/// buffer holds and all the kernel accepts.
pub fn sys_offset(fd: BorrowedFd<'_>, n_samples: u32) -> nix::Result<PtpSysOffset> {
    let mut arg = PtpSysOffset {
        n_samples: n_samples.min(PTP_MAX_SAMPLES as u32),
        ..Default::default()
    };
    // SAFETY: as above. The pointer is derived from a mutable borrow even though the
    // ioctl's direction bits say write-only, so the kernel's write-back is in bounds
    // and on a pointer that permits it.
    unsafe { ptp_sys_offset2(fd.as_raw_fd(), &mut arg as *mut PtpSysOffset) }?;
    Ok(arg)
}

/// One `adjtimex(2)` that hands the kernel PLL a phase offset in nanoseconds, with
/// `error_us` reported as both the maximum and the estimated error. `freq` is a
/// frequency correction in ppm x 65536, or `None` to leave the current one alone.
pub fn adjtimex_pll(offset_ns: i64, error_us: i64, freq: Option<i64>) -> nix::Result<()> {
    // SAFETY: `timex` is a plain struct of integers; all-zero is a valid value, and
    // the kernel only reads the fields named in `modes`.
    let mut tx: libc::timex = unsafe { std::mem::zeroed() };
    tx.modes = libc::ADJ_OFFSET
        | libc::ADJ_NANO
        | libc::ADJ_STATUS
        | libc::ADJ_MAXERROR
        | libc::ADJ_ESTERROR;
    tx.offset = offset_ns;
    tx.status = libc::STA_PLL;
    tx.maxerror = error_us;
    tx.esterror = error_us;
    if let Some(freq) = freq {
        tx.modes |= libc::ADJ_FREQUENCY;
        tx.freq = freq;
    }
    // SAFETY: `tx` is a valid, fully initialised `timex` owned by this frame.
    Errno::result(unsafe { libc::adjtimex(&mut tx) }).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Derived here the way the production ioctls derive theirs, and checked against
    // the kernel's own numbers below.
    const PTP_SYS_OFFSET_PRECISE2: libc::c_ulong = nix::request_code_readwrite!(
        PTP_CLK_MAGIC,
        NR_SYS_OFFSET_PRECISE2,
        size_of::<PtpSysOffsetPrecise>()
    );
    const PTP_SYS_OFFSET2: libc::c_ulong =
        nix::request_code_write!(PTP_CLK_MAGIC, NR_SYS_OFFSET2, size_of::<PtpSysOffset>());

    /// Printed by `.github/ptp-ioctl-numbers.c`, which CI compiles against
    /// `include/uapi/linux/ptp_clock.h` from the kernel the Dockerfile pins and diffs
    /// against this file. Between the two checks, a header that moves under us fails
    /// the build instead of failing to set an enclave's clock.
    const FROM_HEADER: &str = include_str!("../../.github/ptp-ioctl-numbers.txt");

    fn from_header(name: &str) -> libc::c_ulong {
        let line = FROM_HEADER
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("{name} is not in .github/ptp-ioctl-numbers.txt"));
        let value = line[name.len()..].trim();
        match value.strip_prefix("0x") {
            Some(hex) => libc::c_ulong::from_str_radix(hex, 16).expect("hex value"),
            None => value.parse().expect("decimal value"),
        }
    }

    #[test]
    fn ptp_ioctl_numbers_match_the_kernel_header() {
        assert_eq!(
            from_header("PTP_SYS_OFFSET_PRECISE2"),
            PTP_SYS_OFFSET_PRECISE2
        );
        assert_eq!(from_header("PTP_SYS_OFFSET2"), PTP_SYS_OFFSET2);
    }

    /// The request numbers above encode these, so a layout change is a silent ABI
    /// break until one of these two assertions catches it.
    #[test]
    fn ptp_struct_sizes_match_the_kernel_header() {
        assert_eq!(
            from_header("sizeof(struct ptp_sys_offset_precise)"),
            size_of::<PtpSysOffsetPrecise>() as libc::c_ulong
        );
        assert_eq!(
            from_header("sizeof(struct ptp_sys_offset)"),
            size_of::<PtpSysOffset>() as libc::c_ulong
        );
        assert_eq!(size_of::<PtpClockTime>(), 16);
    }
}
