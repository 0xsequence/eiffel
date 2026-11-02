#!/usr/bin/env bash
# Boot an EIF under QEMU's nitro-enclave machine and check the guest reached the
# vsock heartbeat, which is the point where nitro-cli considers an enclave running.
#
# The guest runs at CID 4 and connects to CID 3 port 9000 expecting one 0xB7 byte
# back. vhost-device-vsock turns that into a unix socket on the host, which the
# python harness below answers. Nothing here is Nitro-specific beyond the machine
# type, so it runs on a plain runner (TCG when /dev/kvm is absent).
#
# Requires: bash, python3, vhost-device-vsock and a qemu-system-x86_64 that has the
# nitro-enclave machine (QEMU >= 9.2 built with libcbor and gnutls). Debian and Ubuntu
# build QEMU without libcbor at every version, so CI runs this inside a Fedora
# container that has all four; /dev/kvm is passed in where the runner has one.
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

vhost-device-vsock \
  --vm "guest-cid=4,uds-path=$RUN/vm4.vsock,socket=$RUN/vhost4.socket" \
  --vm "guest-cid=3,uds-path=$RUN/vm3.vsock,socket=$RUN/vhost3.socket" &

for _ in $(seq 50); do [ -S "$RUN/vhost4.socket" ] && break; sleep 0.2; done

# Register as the listener for CID 3 port 9000 and echo one heartbeat byte per
# connection, which is all nitro-cli does and all the enclave's init waits for.
python3 - "$RUN/vm3.vsock" > "$RUN/harness.log" 2>&1 <<'PY' &
import socket, sys

uds = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
uds.connect(sys.argv[1])
uds.sendall(b"LISTEN 9000\n")
while True:
    data = uds.recv(4096)
    if not data:
        break
    if b"\xb7" in data:
        uds.sendall(b"\xb7")
        print("heartbeat answered", flush=True)
PY

KVM=()
[ -w /dev/kvm ] && KVM=(-enable-kvm -cpu host)

timeout 300 qemu-system-x86_64 \
  -M nitro-enclave,vsock=c,id=test \
  -chardev "socket,id=c,path=$RUN/vhost4.socket" \
  -kernel "$EIF" -nographic -m 1G "${KVM[@]}" \
  > "$RUN/console.log" 2>&1 || true

echo "--- guest console"; cat "$RUN/console.log"
echo "--- vsock harness"; cat "$RUN/harness.log"

# The example app serves HTTP over vsock and prints nothing, so the heartbeat is
# the strongest thing to assert here. PR 2 replaces it with a test app that logs.
grep -q "heartbeat answered" "$RUN/harness.log"
