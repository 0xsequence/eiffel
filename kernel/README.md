# Enclave kernel inputs

Inputs for the `kernel` stage in the top-level Dockerfile, which builds the
`bzImage` that boots inside the enclave. The kernel version and tarball
checksum live in the Dockerfile; this directory holds everything else.

- `microvm-kernel-config-x86_64`: the kernel `.config`. Vendored from
  [aws-nitro-enclaves-sdk-bootstrap](https://github.com/aws/aws-nitro-enclaves-sdk-bootstrap)
  (`kernel/microvm-kernel-config-x86_64`, commit noted in the file header).
  It is AWS's enclave-tuned config: virtio-mmio devices, vsock, the KVM PTP
  clock, and almost nothing else. The Dockerfile appends `CONFIG_NSM=y` so the
  attestation driver is built in instead of shipped as `nsm.ko`.
- `0001-vsock-virtio-Remove-queued_replies-pushback-logic.patch`: AWS's
  out-of-tree fix for a vsock deadlock between parent and enclave, rebased onto
  the pinned kernel. Not upstream as of 6.18.
- `cmdline`: the kernel command line, embedded in the EIF. Identical to AWS's.

## Bumping the kernel

1. Change `KERNEL_VERSION` in the Dockerfile and the checksum from
   <https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc>.
2. Build. If the patch fails to apply, rebase it: every hunk removes a use of
   `queued_replies` in `net/vmw_vsock/virtio_transport.c`, so grep for that.
   If the symbol is gone entirely, the fix landed upstream; drop the patch.
3. The config needs no change. `make olddefconfig` keeps every choice in the
   file and picks defaults for options added since it was written. Warnings
   like `symbol value 'm' invalid for ...` mean an option changed type and
   was resolved automatically.
4. Build the `kernel` stage twice with `--no-cache` and compare the `bzImage`
   hashes. They must match.

## Refreshing the config

Only when AWS publishes a config written against a newer kernel, or when you
want the defaults `olddefconfig` picked reviewed rather than inherited:
copy AWS's new file over this one and update the header comment with the
commit it came from. Every blob change moves PCR0 and PCR1.

## Pinned toolchain

The Alpine stages install their packages from the repo at build time, so only
the three packages that decide the bytes of the compiled blobs are pinned:
`musl` (linked into init and chronyd), `gcc` and `binutils`. They are set once
as `MUSL_VERSION`, `GCC_VERSION` and `BINUTILS_VERSION` at the top of the
Dockerfile. The CA bundle is pinned in the `certs` stage because the file
itself is measured.

When Alpine drops one of those versions the build fails instead of drifting.
To bump, read the current versions from the pinned base image:

```bash
docker run --rm --platform linux/amd64 alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0 \
  sh -c 'apk add --no-cache build-base >/dev/null && apk list -I 2>/dev/null | grep -E "^(musl|gcc|binutils)-[0-9]"'
```

Any change to them moves PCR0 and PCR1; rebuild twice and compare before
publishing.
