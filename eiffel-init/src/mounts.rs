//! The filesystem set the customer rootfs gets, as data, in the order it is applied.
//!
//! This mirrors AWS's `init.c` with one deliberate change: a single cgroup2 mount
//! replaces its tmpfs plus one cgroup-v1 mount per controller. Nothing in the
//! enclave uses cgroups, and v2 is what a 6.18 userspace expects.

use nix::errno::Errno;
use nix::mount::{MsFlags, mount};
use nix::sys::stat::Mode;

/// nodev,nosuid,noexec — everything but devpts, which needs device nodes.
const NDSE: MsFlags = MsFlags::MS_NODEV
    .union(MsFlags::MS_NOSUID)
    .union(MsFlags::MS_NOEXEC);
const NSE: MsFlags = MsFlags::MS_NOSUID.union(MsFlags::MS_NOEXEC);
const DIR_MODE: Mode = Mode::from_bits_truncate(0o755);

pub enum Op {
    Mount {
        source: &'static str,
        target: &'static str,
        fstype: &'static str,
        flags: MsFlags,
        data: Option<&'static str>,
    },
    Mkdir {
        path: &'static str,
    },
    Symlink {
        link: &'static str,
        target: &'static str,
    },
}

/// Applied after devtmpfs is back on `/dev`, in this order.
pub const OPS: &[Op] = &[
    Op::Mount {
        source: "proc",
        target: "/proc",
        fstype: "proc",
        flags: NDSE,
        data: None,
    },
    Op::Symlink {
        link: "/dev/fd",
        target: "/proc/self/fd",
    },
    Op::Symlink {
        link: "/dev/stdin",
        target: "/proc/self/fd/0",
    },
    Op::Symlink {
        link: "/dev/stdout",
        target: "/proc/self/fd/1",
    },
    Op::Symlink {
        link: "/dev/stderr",
        target: "/proc/self/fd/2",
    },
    Op::Mount {
        source: "tmpfs",
        target: "/run",
        fstype: "tmpfs",
        flags: NDSE,
        data: Some("mode=0755"),
    },
    Op::Mount {
        source: "tmpfs",
        target: "/tmp",
        fstype: "tmpfs",
        flags: NDSE,
        data: None,
    },
    Op::Mkdir { path: "/dev/shm" },
    Op::Mount {
        source: "shm",
        target: "/dev/shm",
        fstype: "tmpfs",
        flags: NDSE,
        data: None,
    },
    Op::Mkdir { path: "/dev/pts" },
    Op::Mount {
        source: "devpts",
        target: "/dev/pts",
        fstype: "devpts",
        flags: NSE,
        data: None,
    },
    Op::Mount {
        source: "sysfs",
        target: "/sys",
        fstype: "sysfs",
        flags: NDSE,
        data: None,
    },
    Op::Mount {
        source: "cgroup2",
        target: "/sys/fs/cgroup",
        fstype: "cgroup2",
        flags: NDSE,
        data: None,
    },
    // chronyd's driftfile directory. Goes away with chronyd itself.
    Op::Mkdir { path: "/var/lib" },
    Op::Mkdir {
        path: "/var/lib/chrony",
    },
];

/// Run every op in order. A failed mount is fatal; a directory or symlink that is
/// already there is not.
pub fn apply() {
    for op in OPS {
        match op {
            Op::Mount {
                source,
                target,
                fstype,
                flags,
                data,
            } => {
                if let Err(e) = mount(Some(*source), *target, Some(*fstype), *flags, *data) {
                    fatal!("mount {fstype} on {target}: {e}");
                }
            }
            Op::Mkdir { path } => mkdir(path),
            Op::Symlink { link, target } => match std::os::unix::fs::symlink(target, link) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => fatal!("symlink {link} -> {target}: {e}"),
            },
        }
    }
}

/// `mkdir` with mode 0755, tolerating a directory that already exists.
pub fn mkdir(path: &str) {
    match nix::unistd::mkdir(path, DIR_MODE) {
        Ok(()) => {}
        Err(Errno::EEXIST) => {}
        Err(e) => fatal!("mkdir {path}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordered list from the spec. Reordering these bricks the enclave with no
    /// way to debug it, so the order is pinned here rather than only in prose.
    #[test]
    fn ops_are_applied_in_the_documented_order() {
        let actual: Vec<(&str, &str)> = OPS
            .iter()
            .map(|op| match op {
                Op::Mount { fstype, target, .. } => (*fstype, *target),
                Op::Mkdir { path } => ("mkdir", *path),
                Op::Symlink { link, .. } => ("symlink", *link),
            })
            .collect();
        assert_eq!(
            actual,
            [
                ("proc", "/proc"),
                ("symlink", "/dev/fd"),
                ("symlink", "/dev/stdin"),
                ("symlink", "/dev/stdout"),
                ("symlink", "/dev/stderr"),
                ("tmpfs", "/run"),
                ("tmpfs", "/tmp"),
                ("mkdir", "/dev/shm"),
                ("tmpfs", "/dev/shm"),
                ("mkdir", "/dev/pts"),
                ("devpts", "/dev/pts"),
                ("sysfs", "/sys"),
                ("cgroup2", "/sys/fs/cgroup"),
                ("mkdir", "/var/lib"),
                ("mkdir", "/var/lib/chrony"),
            ]
        );
    }

    #[test]
    fn every_mount_is_nosuid_noexec_and_only_devpts_allows_devices() {
        for op in OPS {
            let Op::Mount {
                target,
                flags,
                data,
                ..
            } = op
            else {
                continue;
            };
            assert!(
                flags.contains(MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC),
                "{target} is missing nosuid/noexec"
            );
            assert_eq!(
                flags.contains(MsFlags::MS_NODEV),
                *target != "/dev/pts",
                "{target} has the wrong nodev setting"
            );
            assert_eq!(*data, (*target == "/run").then_some("mode=0755"));
        }
    }

    #[test]
    fn no_cgroup_v1_mounts() {
        assert!(!OPS.iter().any(|op| matches!(
            op,
            Op::Mount { fstype, .. } if *fstype == "cgroup"
        )));
    }
}
