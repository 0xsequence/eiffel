# eiffel

Turns a static binary plus three config files into an AWS Nitro Enclave image (`.eif`).

Everything measured into the enclave's PCRs — the kernel, the bootstrap init, chronyd —
is built from source in this repo. The host-side tool that packs them, `eiffel-build`,
is a Rust binary in this workspace; it replaces the previous `eiffel.sh` + LinuxKit +
`eif_build` pipeline.

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
