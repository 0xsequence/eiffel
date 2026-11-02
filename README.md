# eiffel

Turns a static binary plus three config files into an AWS Nitro Enclave image (`.eif`).

Everything measured into the enclave's PCRs — the kernel and the init — is built from
source in this repo. Both the host-side tool that packs them (`eiffel-build`) and the
enclave's PID 1 (`eiffel-init`) are Rust binaries in this workspace. Apart from your own
rootfs, the bootstrap ramdisk now holds exactly two files: `/init` and a CA bundle.

## Using it

See [`example/`](example) for a complete customer setup. In short, build on top of the
published image and give it `cmd`, `env` and `layout.yml` in `/workspace`:

```dockerfile
FROM ghcr.io/0xsequence/eiffel:<tag>
RUN mkdir /workspace
ADD cmd env layout.yml /workspace/
COPY --from=base /path/to/your/binary /workspace/app
CMD ["myapp"]
```

```sh
docker build --platform linux/amd64 -t myapp-builder .
docker run --platform linux/amd64 -v "$PWD/bin:/out" myapp-builder
```

The `CMD` argument is the output name. `/out` receives:

| File | |
| --- | --- |
| `<name>.eif` | the enclave image |
| `<name>.eif.sha256sum` | its checksum, in `sha256sum -c` format |
| `<name>.pcrs.json` | PCR0, PCR1 and PCR2 — **pin these in your attestation policy** |

`.pcrs.json` is new; previously the PCRs were only printed by `nitro-cli` after the
image was built. The values themselves move whenever anything in the enclave changes,
including on this release: the ramdisks are now packed by `eiffel-build` rather than
LinuxKit, and the bytes differ even though the contents do not.

## Inside the enclave

`/init` is `eiffel-init`, a static binary built from [`eiffel-init/`](eiffel-init). It
replaces both AWS's `init.c` and the Go `pid1` eiffel used to ship. At boot it blocks
every signal, mounts `devtmpfs` and points stdio at `/dev/console`, answers the vsock
heartbeat `nitro-cli` waits for, reads and unlinks `/cmd` and `/env`, switches the root
to `/rootfs`, mounts the usual set (`proc`, `sysfs`, `tmpfs` on `/run` and `/tmp`,
`devpts`, `cgroup2`), sets the clock, then starts every program listed in `cmd`.

After that it only reaps. Children inherit the console directly rather than being piped
through init, so their output interleaves as the console receives it. When the first of
them exits — for any reason, including success — init sends `SIGTERM` to everything,
gives it two seconds to flush, and reboots the enclave. Anything that fails earlier
writes one line to the console saying why and reboots; the console is the only
debugging channel an enclave has.

## Time

`eiffel-init` keeps the clock itself; `chronyd` is gone, along with `/sbin/chronyd`,
`/etc/chrony.conf`, `/etc/passwd` and `/var` in the rootfs.

The time source is unchanged: `/dev/ptp0`, the host's `CLOCK_REALTIME` as the `ptp_kvm`
driver exposes it. Reading it is a hypercall answered by the Nitro Hypervisor, not by
the parent instance, so the parent cannot skew an enclave's clock. (Attestation document
timestamps come from the NSM and never depend on the enclave clock either way.)

What changed is when the correction lands. Init takes one measurement **before it starts
any of your programs** and steps the clock if it is more than a millisecond out, so
nothing ever observes time jumping backwards — chrony stepped on its first measurement,
which could land after an app had already read the clock. From then on a thread samples
every 4 seconds and hands the offset to the kernel PLL, which slews rather than steps.
The console carries one line per boot and one every ten minutes after that:

```
 eiffel-init:  time: stepped by 841234567.891 ms
 eiffel-init:  time: offset 3.4 us, freq +0.012 ppm, 150 samples
```

If `/dev/ptp0` never appears — it needs KVM's clock-pairing hypercall, so it is absent
under emulation — init logs `time: /dev/ptp0 not present` once and boots anyway.

## Migrating

`cmd` lists only your own programs now. Drop the `/sbin/pid1` line when you bump your
pinned tag:

```
/sbin/pid1      <- delete this line
/app/example
```

`/sbin/pid1` is no longer in the rootfs, and a `cmd` that still names it is rejected at
the first boot with `cmd: /sbin/pid1 is gone; list only your programs` on the console,
rather than quietly starting nothing. PCR0, PCR1 and PCR2 all move with this release;
re-pin them from `.pcrs.json`.

Nothing to do for chrony's removal unless you were reading `/var/lib/chrony` or shipping
your own `/etc/passwd` in the bootstrap layout; the rootfs no longer has either.

## `layout.yml`

A layout lists the files that go into the customer ramdisk. It is the LinuxKit `files:`
subset and nothing else — a layout with `init:`, `onboot:` or `services:` is rejected
rather than silently ignored.

```yaml
files:
  - path: cmd                     # relative to the ramdisk root; no leading '/', no '..'
    source: /workspace/cmd        # a file to copy, or
    mode: "0644"                  # octal string; default 0644 for files, 0755 for directories
  - path: env
    contents: "PORT=5000\n"       # inline content, or
  - path: rootfs/app
    directory: true               # a directory
```

Parent directories are created automatically. Entries are written in the order listed,
with uid/gid 0 and mtime 0, so the same layout always produces the same ramdisk bytes.

## Building the image

```sh
docker build --platform linux/amd64 -t eiffel .
```

The kernel stage compiles Linux from source and takes a while. `kernel/README.md`
covers what is pinned and why.

## Development

```sh
cargo test --workspace
cargo clippy --all-targets -- -D warnings
```

`eiffel-build --help` lists the flags; the defaults reproduce the paths inside the
published image, so it is normally run with just an output name.
