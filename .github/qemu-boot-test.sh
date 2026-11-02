#!/usr/bin/env bash
# Boot an EIF under QEMU's nitro-enclave machine and check what eiffel-init did.
#
# The guest VM gets CID 4. vhost-device-vsock's vsock backend (--forward-cid=1)
# forwards every connection the guest makes to CID 1 on the host — the vsock
# loopback — so the parent instance that answers the heartbeat is a plain
# AF_VSOCK listener here. This is the arrangement QEMU's own nitro-enclave
# documentation describes. The other backend (--uds-path) speaks Firecracker's
# hybrid-vsock protocol, where a host listener is a unix socket named
# <uds-path>_<port> and only the host side may send CONNECT; it has no way to
# answer a guest connection addressed to CID 3, which is what init makes.
#
# Requires: bash, python3, vhost-device-vsock and a qemu-system-x86_64 that has the
# nitro-enclave machine (QEMU >= 9.2 built with libcbor and gnutls). Debian and Ubuntu
# build QEMU without libcbor at every version, so CI runs this inside a Fedora
# container that has all four, with --network host so the AF_VSOCK listener below
# reaches the guest. The vsock_loopback module is loaded on the host, outside the
# container; /dev/kvm is passed in where the runner has one.
set -euo pipefail

# A QEMU without the machine type used to make the caller skip this test silently,
# which it did on every run for months. Fail loudly instead.
if ! qemu-system-x86_64 -M help | grep -q '^nitro-enclave'; then
  echo "qemu-system-x86_64 has no nitro-enclave machine type: it was built without libcbor" >&2
  exit 1
fi

EIF=${1:?usage: qemu-boot-test.sh <eif>}
RUN=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$RUN"' EXIT

vhost-device-vsock --vm "guest-cid=4,forward-cid=1,socket=$RUN/vhost4.socket" &

for _ in $(seq 50); do [ -S "$RUN/vhost4.socket" ] && break; sleep 0.2; done

# Stand in for the parent instance: echo one heartbeat byte per connection, which
# is all nitro-cli does and all init waits for.
python3 - > "$RUN/harness.log" 2>&1 <<'PY' &
import socket

# CID 1 is VMADDR_CID_LOCAL.
listener = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
listener.bind((1, 9000))
listener.listen()
while True:
    conn, _ = listener.accept()
    if conn.recv(1) == b"\xb7":
        conn.sendall(b"\xb7")
        print("heartbeat answered", flush=True)
    conn.close()
PY

# Under TCG there is no kvm-clock, so ptp_kvm never registers /dev/ptp0 and the
# chronyd that init starts first dies on its missing refclock. That still proves
# the boot sequence and supervision work, but it races /app/hello to the exit,
# so the clean-exit assertion only runs where KVM gave chronyd a clock.
KVM=()
if [ -w /dev/kvm ]; then KVM=(-enable-kvm -cpu host); fi

timeout 300 qemu-system-x86_64 \
  -M nitro-enclave,vsock=c,id=test \
  -chardev "socket,id=c,path=$RUN/vhost4.socket" \
  -kernel "$EIF" -nographic -m 1G "${KVM[@]}" \
  > "$RUN/console.log" 2>&1 || true

echo "--- guest console"; cat "$RUN/console.log"
echo "--- vsock harness"; cat "$RUN/harness.log"

grep -q "heartbeat answered" "$RUN/harness.log"
grep -q "started /app/hello" "$RUN/console.log"
grep -q "hello: unix time" "$RUN/console.log"
grep -q "hello: /dev/nsm present" "$RUN/console.log"
grep -q "shutting down" "$RUN/console.log"

if [ ${#KVM[@]} -gt 0 ]; then
  grep -qE "exited /app/hello pid [0-9]+ status 0" "$RUN/console.log"
else
  echo "note: no /dev/kvm, skipping the /app/hello exit-status assertion"
fi
