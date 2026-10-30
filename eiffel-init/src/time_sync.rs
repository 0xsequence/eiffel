//! Wall-clock synchronisation against `/dev/ptp0`, replacing chronyd.
//!
//! `/dev/ptp0` is the host's `CLOCK_REALTIME` as `ptp_kvm` exposes it: reading it
//! is the `KVM_HC_CLOCK_PAIRING` hypercall, answered by the Nitro Hypervisor rather
//! than by the parent instance, so the parent cannot skew an enclave's clock. The
//! guest clocksource is kvm-clock, derived from the same TSC as the host's, so the
//! two run at very nearly the same rate and the whole problem is the offset the
//! enclave boots with. One step before any app starts, then a slow kernel-PLL
//! correction afterwards, covers it.
//!
//! Where the pvclock TSC-stable flag is missing — nested virtualisation, mostly —
//! the driver has no `getcrosststamp` and `PTP_SYS_OFFSET_PRECISE2` answers
//! `EOPNOTSUPP`. The `PTP_SYS_OFFSET2` fallback is therefore load-bearing, not a
//! nicety.

use crate::sys;
use nix::errno::Errno;
use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;
use nix::sys::time::TimeSpec;
use nix::time::{ClockId, clock_gettime, clock_settime};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Frequency tracking is the one piece here that guesses rather than measures.
/// Flip this to `false` to keep only the phase correction.
const ENABLE_FREQ: bool = true;

const PTP_DEV: &str = "/dev/ptp0";
/// `ptp_kvm` registers the device a moment after it probes, which can be after init
/// has already reached this point.
const OPEN_RETRY: Duration = Duration::from_secs(2);
/// chrony's `poll 2` on the same refclock.
const POLL: Duration = Duration::from_secs(4);
/// After an ioctl error, stop hammering the device.
const ERROR_BACKOFF: Duration = Duration::from_secs(30);
/// At boot, anything bigger than this is worth a step; below it the PLL will do.
const BOOT_STEP_NS: i128 = 1_000_000;
/// Afterwards, stepping means something went wrong on the host.
const STEP_NS: i128 = 500_000_000;
/// How long the offset has to keep moving the same way before a frequency
/// correction is drawn from it, and how often the state of the clock is logged.
const FREQ_SETTLE: Duration = Duration::from_secs(600);
const LOG_EVERY: Duration = Duration::from_secs(600);
/// A host drifting further than this is broken; applying the number would be worse
/// than ignoring it.
const MAX_PPM: f64 = 500.0;
const FALLBACK_SAMPLES: u32 = 5;

/// One measurement of the host clock against ours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    /// Host clock minus ours. Positive means our clock is behind.
    offset_ns: i128,
    /// How long the measurement took, which bounds its error. Zero for a precise
    /// cross-timestamp, where there is no interval to measure.
    delay_ns: i128,
    /// Whether `PTP_SYS_OFFSET_PRECISE2` answered or the fallback ran.
    precise: bool,
}

/// Step the clock from the host's, then keep it disciplined on a thread of its own
/// so the supervisor stays single-threaded.
///
/// Every failure here is survivable: the enclave boots either way, and an app that
/// needs the time fails visibly rather than silently getting a wrong one.
pub fn start() {
    let Some(fd) = open_ptp() else { return };

    match sample(fd.as_fd()) {
        Ok(first) => {
            if !first.precise {
                log!(
                    "time: {PTP_DEV} has no cross-timestamp; using {FALLBACK_SAMPLES}-sample offsets"
                );
            }
            if first.offset_ns.abs() > BOOT_STEP_NS {
                match step(first.offset_ns) {
                    Ok(()) => log!("time: stepped by {:.3} ms", first.offset_ns as f64 / 1e6),
                    Err(e) => log!("time: clock_settime: {e}"),
                }
            } else {
                log!(
                    "time: offset {:.1} us, no step",
                    first.offset_ns as f64 / 1e3
                );
            }
        }
        Err(e) => log!("time: first sample failed: {e}"),
    }

    if let Err(e) = std::thread::Builder::new()
        .name("time".to_string())
        .spawn(move || discipline(fd))
    {
        log!("time: could not start the sync thread: {e}");
    }
}

/// Sample every `POLL`, correct, and report once every `LOG_EVERY`. Never returns.
fn discipline(fd: OwnedFd) {
    // Start of the current uninterrupted run of samples, and the offset then. Reset
    // whenever anything interrupts it, because a frequency drawn across a step or an
    // error is nonsense.
    let mut anchor: Option<(Instant, i128)> = None;
    let mut applied_ppm = 0.0f64;
    let mut samples: u64 = 0;
    let mut last_log = Instant::now();

    loop {
        sleep(POLL);

        let s = match sample(fd.as_fd()) {
            Ok(s) => s,
            Err(e) => {
                log!(
                    "time: sample: {e}; retrying every {}s",
                    ERROR_BACKOFF.as_secs()
                );
                anchor = None;
                sleep(ERROR_BACKOFF);
                continue;
            }
        };
        samples += 1;

        if s.offset_ns.abs() >= STEP_NS {
            // The kernel PLL cannot slew this far, and reaching it after the boot
            // step means the host's clock moved under us.
            match step(s.offset_ns) {
                Ok(()) => log!(
                    "time: offset {:.3} ms is beyond slewing; stepped (host clock problem?)",
                    s.offset_ns as f64 / 1e6
                ),
                Err(e) => log!("time: clock_settime: {e}"),
            }
            anchor = None;
        } else {
            let freq = next_freq(&mut anchor, &mut applied_ppm, s.offset_ns);
            let offset_ns = s.offset_ns.clamp(-STEP_NS, STEP_NS) as i64;
            let error_us = (s.delay_ns / 1_000).clamp(0, 1_000_000) as i64;
            if let Err(e) = sys::adjtimex_pll(offset_ns, error_us, freq) {
                log!(
                    "time: adjtimex: {e}; retrying every {}s",
                    ERROR_BACKOFF.as_secs()
                );
                anchor = None;
                sleep(ERROR_BACKOFF);
                continue;
            }
        }

        if last_log.elapsed() >= LOG_EVERY {
            last_log = Instant::now();
            log!(
                "time: offset {:.1} us, freq {applied_ppm:+.3} ppm, {samples} samples",
                s.offset_ns as f64 / 1e3
            );
        }
    }
}

/// The frequency correction to send with this sample, if the run is long enough to
/// have produced one.
///
/// ponytail: the kernel PLL is slewing the clock while this measures it, so part of
/// what it reads back as drift is its own correction. It is bounded by `MAX_PPM` and
/// re-estimated every `FREQ_SETTLE`, which is enough for a guest whose reference is
/// on the same TSC; a host that really drifts wants a chrony-style loop that models
/// the slew, or `ENABLE_FREQ = false`.
fn next_freq(
    anchor: &mut Option<(Instant, i128)>,
    applied_ppm: &mut f64,
    offset_ns: i128,
) -> Option<i64> {
    if !ENABLE_FREQ {
        return None;
    }
    let now = Instant::now();
    match *anchor {
        None => {
            *anchor = Some((now, offset_ns));
            None
        }
        Some((started, first_offset)) => {
            let elapsed = now.duration_since(started);
            if elapsed < FREQ_SETTLE {
                return None;
            }
            let drift = drift_ppm(offset_ns - first_offset, elapsed.as_nanos() as i128);
            *applied_ppm = (*applied_ppm + drift).clamp(-MAX_PPM, MAX_PPM);
            *anchor = None;
            Some(freq_units(*applied_ppm))
        }
    }
}

/// The frequency error behind an offset that moved by `delta_ns` over `elapsed_ns`.
///
/// `offset = host - ours`, so a growing offset means our clock is running slow and
/// wants a positive correction — which is the sign `adjtimex` uses too.
fn drift_ppm(delta_ns: i128, elapsed_ns: i128) -> f64 {
    if elapsed_ns <= 0 {
        return 0.0;
    }
    delta_ns as f64 / elapsed_ns as f64 * 1e6
}

/// `adjtimex` takes its frequency in ppm scaled by 2^16.
fn freq_units(ppm: f64) -> i64 {
    (ppm.clamp(-MAX_PPM, MAX_PPM) * 65536.0).round() as i64
}

/// Wait out `ptp_kvm`'s probe, then give up and say so once.
fn open_ptp() -> Option<OwnedFd> {
    let deadline = Instant::now() + OPEN_RETRY;
    loop {
        match open(PTP_DEV, OFlag::O_RDONLY | OFlag::O_CLOEXEC, Mode::empty()) {
            Ok(fd) => return Some(fd),
            Err(Errno::ENOENT) if Instant::now() < deadline => sleep(Duration::from_millis(50)),
            Err(Errno::ENOENT) => {
                log!("time: {PTP_DEV} not present; the clock will not be synchronised");
                return None;
            }
            Err(e) => {
                log!("time: open {PTP_DEV}: {e}");
                return None;
            }
        }
    }
}

/// One offset measurement, precise where the driver can and bracketed where it cannot.
fn sample(fd: BorrowedFd<'_>) -> nix::Result<Sample> {
    match sys::sys_offset_precise(fd) {
        Ok(precise) => Ok(precise_sample(&precise)),
        // No `getcrosststamp`: the pvclock TSC-stable flag is not set.
        Err(Errno::EOPNOTSUPP) => tightest_sample(&sys::sys_offset(fd, FALLBACK_SAMPLES)?),
        Err(e) => Err(e),
    }
}

/// A cross-timestamp: the two clocks were read at the same instant, so the
/// difference is the offset and there is no delay to subtract.
fn precise_sample(precise: &sys::PtpSysOffsetPrecise) -> Sample {
    Sample {
        offset_ns: ns(precise.device) - ns(precise.sys_realtime),
        delay_ns: 0,
        precise: true,
    }
}

/// `ts` is `[sys, phc, sys, phc, ..., sys]`: each PHC read is bracketed by a system
/// read. The tightest bracket is the least uncertain, so that is the one used, with
/// the midpoint of its two system reads standing in for "when the PHC was read".
fn tightest_sample(offset: &sys::PtpSysOffset) -> nix::Result<Sample> {
    let n = (offset.n_samples as usize).min(sys::PTP_MAX_SAMPLES);
    (0..n)
        .map(|i| {
            let before = ns(offset.ts[2 * i]);
            let phc = ns(offset.ts[2 * i + 1]);
            let after = ns(offset.ts[2 * i + 2]);
            Sample {
                offset_ns: phc - (before + after) / 2,
                delay_ns: after - before,
                precise: false,
            }
        })
        .min_by_key(|s| s.delay_ns)
        .ok_or(Errno::EINVAL)
}

/// Set `CLOCK_REALTIME` forward or back by `offset_ns`.
fn step(offset_ns: i128) -> nix::Result<()> {
    let now = clock_gettime(ClockId::CLOCK_REALTIME)?;
    let total = i128::from(now.tv_sec()) * 1_000_000_000 + i128::from(now.tv_nsec()) + offset_ns;
    let sec = i64::try_from(total.div_euclid(1_000_000_000)).map_err(|_| Errno::EINVAL)?;
    let nsec = total.rem_euclid(1_000_000_000) as i64;
    clock_settime(ClockId::CLOCK_REALTIME, TimeSpec::new(sec, nsec))
}

fn ns(t: sys::PtpClockTime) -> i128 {
    i128::from(t.sec) * 1_000_000_000 + i128::from(t.nsec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(sec: i64, nsec: u32) -> sys::PtpClockTime {
        sys::PtpClockTime {
            sec,
            nsec,
            reserved: 0,
        }
    }

    #[test]
    fn precise_offset_is_device_minus_system() {
        let precise = sys::PtpSysOffsetPrecise {
            device: t(1_000, 500_000_000),
            sys_realtime: t(1_000, 499_000_000),
            ..Default::default()
        };
        assert_eq!(
            precise_sample(&precise),
            Sample {
                offset_ns: 1_000_000,
                delay_ns: 0,
                precise: true
            }
        );

        // Our clock ahead of the host's gives a negative offset.
        let behind = sys::PtpSysOffsetPrecise {
            device: t(999, 0),
            sys_realtime: t(1_000, 0),
            ..Default::default()
        };
        assert_eq!(precise_sample(&behind).offset_ns, -1_000_000_000);
    }

    #[test]
    fn fallback_picks_the_tightest_bracket() {
        let mut offset = sys::PtpSysOffset {
            n_samples: 3,
            ..Default::default()
        };
        // Triple 0: 1000ns wide, midpoint 500, phc 2000  -> offset 1500.
        offset.ts[0] = t(0, 0);
        offset.ts[1] = t(0, 2_000);
        // Triple 1: 100ns wide, midpoint 1050, phc 1300  -> offset 250. The winner.
        offset.ts[2] = t(0, 1_000);
        offset.ts[3] = t(0, 1_300);
        // Triple 2: 400ns wide, midpoint 1300, phc 1000  -> offset -300.
        offset.ts[4] = t(0, 1_100);
        offset.ts[5] = t(0, 1_000);
        offset.ts[6] = t(0, 1_500);

        assert_eq!(
            tightest_sample(&offset).unwrap(),
            Sample {
                offset_ns: 250,
                delay_ns: 100,
                precise: false
            }
        );
    }

    #[test]
    fn fallback_spans_the_seconds_field() {
        let mut offset = sys::PtpSysOffset {
            n_samples: 1,
            ..Default::default()
        };
        offset.ts[0] = t(10, 999_999_000);
        offset.ts[1] = t(12, 0);
        offset.ts[2] = t(11, 0);
        // Midpoint of 10.999999 and 11.0 is 10.9999995; the PHC read 12.0.
        assert_eq!(tightest_sample(&offset).unwrap().offset_ns, 1_000_000_500);
    }

    #[test]
    fn a_kernel_that_reports_no_samples_is_an_error() {
        assert_eq!(
            tightest_sample(&sys::PtpSysOffset::default()),
            Err(Errno::EINVAL)
        );
    }

    #[test]
    fn drift_is_offset_over_elapsed_in_ppm() {
        // 1 ms gained over 1000 s is 1 ppm.
        assert_eq!(drift_ppm(1_000_000, 1_000_000_000_000), 1.0);
        assert_eq!(drift_ppm(-1_000_000, 1_000_000_000_000), -1.0);
        // No elapsed time means no information, not a division by zero.
        assert_eq!(drift_ppm(1_000_000, 0), 0.0);
    }

    #[test]
    fn frequency_is_ppm_times_2_to_the_16_and_clamped() {
        assert_eq!(freq_units(1.0), 65_536);
        assert_eq!(freq_units(-1.0), -65_536);
        assert_eq!(freq_units(0.5), 32_768);
        assert_eq!(freq_units(10_000.0), 500 * 65_536);
        assert_eq!(freq_units(-10_000.0), -500 * 65_536);
    }

    #[test]
    fn frequency_waits_for_a_long_enough_run_then_restarts_it() {
        let mut anchor = None;
        let mut ppm = 0.0;

        // First sample only anchors the run.
        assert_eq!(next_freq(&mut anchor, &mut ppm, 0), None);
        assert!(anchor.is_some());
        // A second one inside the window changes nothing.
        assert_eq!(next_freq(&mut anchor, &mut ppm, 1_000), None);

        // Backdate the anchor past the settling time: 1 ms gained over FREQ_SETTLE.
        anchor = Some((Instant::now() - FREQ_SETTLE, 0));
        let freq = next_freq(&mut anchor, &mut ppm, 1_000_000).expect("a frequency");
        let expected = drift_ppm(1_000_000, FREQ_SETTLE.as_nanos() as i128);
        assert!((ppm - expected).abs() < 0.01, "{ppm} ppm, want {expected}");
        assert_eq!(freq, freq_units(ppm));
        // And the run starts over rather than measuring across the correction.
        assert!(anchor.is_none());
    }

    #[test]
    fn frequency_accumulates_and_stays_clamped() {
        let mut ppm = 0.0;
        for _ in 0..10 {
            let mut anchor = Some((Instant::now() - FREQ_SETTLE, 0));
            // 100 ms over FREQ_SETTLE is ~167 ppm each round.
            next_freq(&mut anchor, &mut ppm, 100_000_000);
        }
        assert_eq!(ppm, MAX_PPM);
    }
}
