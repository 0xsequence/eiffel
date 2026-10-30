# Toolchain pins for the kernel stage, the only thing Alpine still compiles here (init and the
# builder are Rust stages and use none of them). They decide the bytes of bzImage, so they are
# pinned exactly and the build fails loudly when Alpine drops a version; everything else the stage
# installs (make, bison, perl, ...) only has to work and is left to float. Bumping these moves PCR0.
ARG MUSL_VERSION=1.2.5-r24
ARG GCC_VERSION=15.2.0-r2
ARG BINUTILS_VERSION=2.45.1-r0

###############################################################################
FROM alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0 AS certs

# eMudhra CA compromise: the enclave and every stage that fetches over TLS use this bundle, so never
# go below 20261020-r0, the first release that distrusts the compromised intermediates. Pinned exactly
# because the bundle lands in the bootstrap initrd and therefore in PCR1.
ARG CA_CERTIFICATES=20261020-r0
RUN apk add --no-cache --upgrade "ca-certificates-bundle=${CA_CERTIFICATES}"


###############################################################################
# Host-side builder: layout YAML -> cpio.gz ramdisks -> EIF. Nothing it produces is
# measured, so the pin here is only about a build that keeps working, not about PCRs.
# The tag matches rust-toolchain.toml so rustup has nothing to download.
FROM rust:1.94.1-bookworm@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55 AS builder

COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY eiffel-build ./eiffel-build
COPY eiffel-init ./eiffel-init

RUN cargo build --release --locked -p eiffel-build
# Executable at /workspace/target/release/eiffel-build


###############################################################################
FROM alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0 AS kernel-src

COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

# coreutils: the kernel's initramfs generator parses KBUILD_BUILD_TIMESTAMP with GNU date; busybox date
# rejects it and the build silently falls back to the current time, breaking reproducibility.
ARG MUSL_VERSION GCC_VERSION BINUTILS_VERSION
RUN apk add --no-cache bash build-base bc bison coreutils flex elfutils-dev openssl-dev perl linux-headers xz \
    musl=${MUSL_VERSION} musl-dev=${MUSL_VERSION} gcc=${GCC_VERSION} binutils=${BINUTILS_VERSION}

# Linux 6.18 LTS. Checksum from https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc
ARG KERNEL_VERSION=6.18.60
ADD --checksum=sha256:3c2f44ef6acc021a7ad444d9a2e6ec1ef488ff09368793d0004a6080f69d621e \
    https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KERNEL_VERSION}.tar.xz /linux.tar.xz
RUN mkdir /linux && tar -xJf /linux.tar.xz -C /linux --strip-components=1 && rm /linux.tar.xz
WORKDIR /linux

# AWS's fix for the parent/enclave vsock deadlock, v3 as posted to netdev (not merged; see the patch header).
COPY kernel/0001-vsock-virtio-Remove-queued_replies-pushback-logic.patch /tmp/
RUN patch -p1 < /tmp/0001-vsock-virtio-Remove-queued_replies-pushback-logic.patch


###############################################################################
# Split off kernel-src above so anything that only needs the unpacked tree (the PTP
# uapi header check in CI) can stop there instead of compiling a kernel. The build
# steps below are unchanged, so bzImage is byte-for-byte what it was.
FROM kernel-src AS kernel

# AWS's microvm config plus the in-tree Nitro Security Module driver (mainline since 6.8), so no nsm.ko.
COPY kernel/microvm-kernel-config-x86_64 .config
RUN echo CONFIG_NSM=y >> .config && make ARCH=x86_64 olddefconfig && grep -q '^CONFIG_NSM=y' .config

# The kernel embeds these in its version banner; fixed values keep bzImage reproducible.
# An arbitrary fixed epoch, not the build date.
ENV SOURCE_DATE_EPOCH=1707421839
RUN KBUILD_BUILD_TIMESTAMP="$(date -u -d @${SOURCE_DATE_EPOCH})" KBUILD_BUILD_USER=eiffel KBUILD_BUILD_HOST=eiffel KBUILD_BUILD_VERSION=1 \
    make ARCH=x86_64 -j"$(nproc)" bzImage \
    && mkdir /out && cp arch/x86/boot/bzImage /out/bzImage && cp .config /out/bzImage.config


###############################################################################
# PID 1 in the enclave. Measured into PCR1, so it is built static (no musl loader
# in the initrd) and with every path remapped, leaving nothing of this machine in
# the bytes. The tag matches rust-toolchain.toml so rustup has nothing to download
# beyond the musl std the toolchain file asks for.
FROM rust:1.94.1-bookworm@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55 AS init

COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY eiffel-build ./eiffel-build
COPY eiffel-init ./eiffel-init

ENV RUSTFLAGS="-C target-feature=+crt-static --remap-path-prefix=/workspace=/build --remap-path-prefix=/usr/local/cargo=/cargo"
# Fail the build if init still needs a dynamic loader (an ELF with PT_INTERP): the
# bootstrap initrd carries no loader for it.
RUN cargo build --release --locked -p eiffel-init --target x86_64-unknown-linux-musl \
    && cp target/x86_64-unknown-linux-musl/release/eiffel-init /init \
    && ! readelf -lW /init | grep -q interpreter


###############################################################################
FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3

# libssl3 is what aws-nitro-enclaves-image-format links against; ca-certificates is only
# there for its (unused) KMS signing path.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --chmod=755 --from=builder /workspace/target/release/eiffel-build /usr/bin/eiffel-build

RUN mkdir /blobs
COPY --from=kernel /out/bzImage /out/bzImage.config /blobs/
COPY kernel/cmdline /blobs/cmdline
COPY --from=init /init /blobs/init

COPY --from=certs /etc/ssl/certs/ca-certificates.crt /blobs/ca-certificates.crt

WORKDIR /eiffel
ADD ./config ./

ENTRYPOINT ["/usr/bin/eiffel-build"]

CMD ["app"]
