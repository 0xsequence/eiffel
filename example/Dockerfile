#
# Base image for building the application
#
FROM golang:1.21.7-alpine3.19@sha256:163801a964d358d6450aeb51b59d5c807d43a7c97fed92cc7ff1be5bd72811ab AS base

WORKDIR /go/src/github.com/0xsequence/eiffel/example

# Only add the files required in the build
ADD go.mod go.sum main.go ./

# Forces building a static binary
ENV CGO_ENABLED=0

# Build deterministically
RUN go build -v -trimpath -buildvcs=false -ldflags='-s -w -buildid=' -o ./bin/example .


#
# Switch to an eiffel context
#
FROM ghcr.io/0xsequence/eiffel:v0.3.0@sha256:d4aa946d7eb587e0554123efc3eaa5830a1428b0325ea239fe489e372f573dfe

RUN mkdir /workspace

# Add eiffel config files
ADD cmd env layout.yml /workspace/

# Add the result executable from the previous stage
COPY --from=base /go/src/github.com/0xsequence/eiffel/example/bin/example /workspace/example

# The entrypoint in this stage is the eiffel executable, anything given to CMD becomes an argument
# This executes `eiffel example` instructing the tool to output an EIF named `example.eif`
CMD ["example"]
