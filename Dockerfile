# Toolchain pins shared by the Alpine build stages below. These three decide the bytes of every blob
# that is compiled here (musl is linked into init and chronyd; gcc and binutils produce all of them),
# so they are pinned exactly and the build fails loudly when Alpine drops a version. Everything else
# the stages install (make, bison, perl, ruby for the chrony man pages, ...) only has to work and is
# left to float. Bumping these moves PCR0 and PCR1.
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
FROM rust:1.88.0@sha256:af306cfa71d987911a781c37b59d7d67d934f49684058f96cf72079c3626bfe0 AS eif_build

COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

RUN mkdir /workspace
WORKDIR /workspace

RUN git clone https://github.com/aws/aws-nitro-enclaves-image-format.git . \
    && git checkout d0d224b8b626db5fcc2d7b685bdd229991bbf0a7

# Upstream ships no Cargo.lock, so the crate set floats with crates.io; pin it for
# reproducibility (without this the build breaks on aws-nitro-enclaves-cose's MSRV).
COPY eif_build/Cargo.lock ./Cargo.lock

RUN RUSTFLAGS="--allow non_local_definitions" cargo build --locked --example eif_build
# Executable at /workspace/target/debug/examples/eif_build


###############################################################################
FROM alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0 AS chrony

COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

ARG MUSL_VERSION GCC_VERSION BINUTILS_VERSION
RUN apk add --no-cache git linux-headers build-base bison asciidoctor \
    musl=${MUSL_VERSION} musl-dev=${MUSL_VERSION} gcc=${GCC_VERSION} binutils=${BINUTILS_VERSION}

RUN mkdir /workspace
# chrony tag 4.8
RUN git clone --depth 1 -b 4.8 https://gitlab.com/chrony/chrony.git /workspace \
    && test "$(git -C /workspace rev-parse HEAD)" = 9e8541e3c4c89bfb7a60b404f646fab57c69ce59

WORKDIR /workspace

RUN mkdir /out
ENV SOURCE_DATE_EPOCH=1707421839
# Static so the enclave carries no musl loader: the unpinned apk add above floats musl, and a
# floating /lib/ld-musl-x86_64.so.1 in the bootstrap initrd moved PCR1 between builds.
RUN LDFLAGS=-static ./configure --prefix=/out
# Fail the build if chronyd still needs a dynamic loader (an ELF with PT_INTERP).
RUN make && make install && ! readelf -lW /out/sbin/chronyd | grep -q interpreter


###############################################################################
FROM alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0 AS kernel

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

# AWS carries this out of tree: without it the parent and the enclave can deadlock each other's vsock queues.
COPY kernel/0001-vsock-virtio-Remove-queued_replies-pushback-logic.patch /tmp/
RUN patch -p1 < /tmp/0001-vsock-virtio-Remove-queued_replies-pushback-logic.patch

# AWS's microvm config plus the in-tree Nitro Security Module driver (mainline since 6.8), so no nsm.ko.
COPY kernel/microvm-kernel-config-x86_64 .config
RUN echo CONFIG_NSM=y >> .config && make ARCH=x86_64 olddefconfig && grep -q '^CONFIG_NSM=y' .config

# The kernel embeds these in its version banner; fixed values keep bzImage reproducible.
# Same arbitrary epoch as the chrony stage.
ENV SOURCE_DATE_EPOCH=1707421839
RUN KBUILD_BUILD_TIMESTAMP="$(date -u -d @${SOURCE_DATE_EPOCH})" KBUILD_BUILD_USER=eiffel KBUILD_BUILD_HOST=eiffel KBUILD_BUILD_VERSION=1 \
    make ARCH=x86_64 -j"$(nproc)" bzImage \
    && mkdir /out && cp arch/x86/boot/bzImage /out/bzImage && cp .config /out/bzImage.config


###############################################################################
FROM alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0 AS init

COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

ARG MUSL_VERSION GCC_VERSION BINUTILS_VERSION
RUN apk add --no-cache git build-base linux-headers \
    musl=${MUSL_VERSION} musl-dev=${MUSL_VERSION} gcc=${GCC_VERSION} binutils=${BINUTILS_VERSION}

# Enclave bootstrap init from AWS; the same source their nitro-cli blobs are built from.
RUN git clone https://github.com/aws/aws-nitro-enclaves-sdk-bootstrap.git /workspace \
    && git -C /workspace checkout f718dea60a9d9bb8b8682fd852ad793912f3c5db
WORKDIR /workspace/init
RUN gcc -static -Wall -Wextra -Werror -O2 -flto -o /init init.c && strip --strip-all /init


###############################################################################
FROM golang:1.26.10-alpine3.23@sha256:a07f231c19b03e6106cf442f02a5bcf262147e744d15b7b5e4b6d81d420c70a0 AS pid1

COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

RUN mkdir /workspace
WORKDIR /workspace

ADD pid1/go.mod pid1/go.sum ./
RUN go mod download
ADD pid1/ ./
RUN go build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o /out/pid1 .


###############################################################################
FROM ubuntu:26.04@sha256:c098726dd9b9733f3e885f8de0ef375206a311b0a317eadc40d347b1115fb944

COPY --chmod=755 --from=eif_build /workspace/target/debug/examples/eif_build /usr/bin/eif_build
ADD --chmod=755 --checksum=sha256:18b97b0d83401dd88ade14f207eb26358d9a7fde5d3dc4b1e24bd8faf087be6f \
    https://github.com/linuxkit/linuxkit/releases/download/v1.8.2/linuxkit-linux-amd64 /usr/bin/linuxkit

RUN mkdir /blobs
COPY --from=kernel /out/bzImage /out/bzImage.config /blobs/
COPY kernel/cmdline /blobs/cmdline
COPY --from=init /init /blobs/init

COPY --from=pid1 /out/pid1 /blobs/pid1
COPY --from=chrony /out/sbin/chronyd /blobs/chronyd
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /blobs/ca-certificates.crt

ADD --chmod=755 eiffel.sh /app/

WORKDIR /eiffel
ADD ./config ./

ENTRYPOINT ["/app/eiffel.sh"]

CMD ["app"]
