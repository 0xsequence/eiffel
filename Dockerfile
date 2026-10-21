###############################################################################
FROM alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0 AS certs

# eMudhra CA compromise: the enclave and every stage that fetches over TLS use this bundle,
# so refuse one that still trusts the compromised intermediates.
ARG CA_CERTIFICATES_MIN=20261020-r0
RUN apk add --no-cache --upgrade "ca-certificates-bundle>=${CA_CERTIFICATES_MIN}"


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

RUN apk add git linux-headers build-base bison asciidoctor

RUN mkdir /workspace
# chrony tag 4.8
RUN git clone --depth 1 -b 4.8 https://gitlab.com/chrony/chrony.git /workspace \
    && test "$(git -C /workspace rev-parse HEAD)" = 9e8541e3c4c89bfb7a60b404f646fab57c69ce59

WORKDIR /workspace

RUN mkdir /out
ENV SOURCE_DATE_EPOCH=1707421839
RUN ./configure --prefix=/out
RUN make && make install


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

# TODO: compile all these blobs at build-time
RUN mkdir /blobs
ADD --checksum=sha256:755e650b732777b798cb9ec243ee402bef4826f789cf01a1e453bb724207c005 \
    https://github.com/aws/aws-nitro-enclaves-cli/raw/refs/tags/v1.4.4/blobs/x86_64/init /blobs/init
ADD --checksum=sha256:48904e520db2541ca4378da29d85791749408febc81987ade56cc5c556bd90df \
    https://github.com/aws/aws-nitro-enclaves-cli/raw/refs/tags/v1.4.4/blobs/x86_64/nsm.ko /blobs/nsm.ko
ADD --checksum=sha256:210eda749c1308eb60671a579d24db5e8a3477cb7a247cf313c286b09fe2d857 \
    https://github.com/aws/aws-nitro-enclaves-cli/raw/refs/tags/v1.4.4/blobs/x86_64/bzImage /blobs/bzImage
ADD --checksum=sha256:9378dea490ed6c698c3d23b346ed08e49dae52d74a59cee2673b8a7b1951fc5b \
    https://github.com/aws/aws-nitro-enclaves-cli/raw/refs/tags/v1.4.4/blobs/x86_64/bzImage.config /blobs/bzImage.config
ADD --checksum=sha256:10d7d9dd205d4596d45997d17434f26207525f129d171a51f9859b1af9f4a07a \
    https://github.com/aws/aws-nitro-enclaves-cli/raw/refs/tags/v1.4.4/blobs/x86_64/cmdline /blobs/cmdline

COPY --from=pid1 /out/pid1 /blobs/pid1
COPY --from=chrony /out/sbin/chronyd /blobs/chronyd
COPY --from=chrony /lib/ld-musl-x86_64.so.1 /blobs/ld-musl-x86_64.so.1
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /blobs/ca-certificates.crt

ADD --chmod=755 eiffel.sh /app/

WORKDIR /eiffel
ADD ./config ./

ENTRYPOINT ["/app/eiffel.sh"]

CMD ["app"]
