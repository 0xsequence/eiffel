###############################################################################
FROM rust AS eif_build

RUN mkdir /workspace
RUN git clone --depth 1 -b main https://github.com/aws/aws-nitro-enclaves-image-format.git /workspace

WORKDIR /workspace

RUN cargo build --example eif_build
# Executable at /workspace/target/debug/examples/eif_build


###############################################################################
FROM alpine AS chrony

RUN apk add git linux-headers build-base bison asciidoctor

RUN mkdir /workspace
RUN git clone --depth 1 -b 4.5 https://gitlab.com/chrony/chrony.git /workspace

WORKDIR /workspace

RUN mkdir /out
ENV SOURCE_DATE_EPOCH=1707421839
RUN ./configure --prefix=/out
RUN make && make install


###############################################################################
FROM golang:1.21-alpine3.19 AS pid1

RUN mkdir /workspace
WORKDIR /workspace

ADD pid1/go.mod pid1/go.sum ./
RUN go mod download
ADD pid1/ ./
RUN go build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o /out/pid1 .


###############################################################################
FROM ubuntu:22.04

COPY --chmod=755 --from=eif_build /workspace/target/debug/examples/eif_build /usr/bin/eif_build
ADD --chmod=755 https://github.com/linuxkit/linuxkit/releases/download/v1.0.1/linuxkit-linux-amd64 /usr/bin/linuxkit

# TODO: compile all these blobs at build-time
RUN mkdir /blobs
ADD https://github.com/aws/aws-nitro-enclaves-cli/raw/ec002ccc722051d01c9a00d68e485977b3a9ad08/blobs/x86_64/init /blobs/init
ADD https://github.com/aws/aws-nitro-enclaves-cli/raw/ec002ccc722051d01c9a00d68e485977b3a9ad08/blobs/x86_64/nsm.ko /blobs/nsm.ko
ADD https://github.com/aws/aws-nitro-enclaves-cli/raw/ec002ccc722051d01c9a00d68e485977b3a9ad08/blobs/x86_64/bzImage /blobs/bzImage
ADD https://github.com/aws/aws-nitro-enclaves-cli/raw/ec002ccc722051d01c9a00d68e485977b3a9ad08/blobs/x86_64/bzImage.config /blobs/bzImage.config
ADD https://github.com/aws/aws-nitro-enclaves-cli/raw/ec002ccc722051d01c9a00d68e485977b3a9ad08/blobs/x86_64/cmdline /blobs/cmdline

COPY --from=pid1 /out/pid1 /blobs/pid1
COPY --from=chrony /out/sbin/chronyd /blobs/chronyd
COPY --from=chrony /lib/ld-musl-x86_64.so.1 /blobs/ld-musl-x86_64.so.1
COPY --from=chrony /etc/ssl/certs/ca-certificates.crt /blobs/ca-certificates.crt

ADD --chmod=755 eiffel.sh /app/

WORKDIR /eiffel
ADD ./config ./

ENTRYPOINT ["/app/eiffel.sh"]

CMD ["app"]
