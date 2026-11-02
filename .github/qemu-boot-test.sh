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
# The guest boots with its RTC in 2000 so its wall clock starts ~26 years behind
# the host's while ptp_kvm still reports the host's true CLOCK_REALTIME. That is
# what makes the time assertions below mean something: eiffel-init has to notice
# the gap and close it before /app/hello prints a date.
#
# /dev/ptp0 is the KVM_HC_CLOCK_PAIRING hypercall, so it only exists with KVM.
# Without /dev/kvm the time assertions are skipped and the run instead proves
# that a missing PTP clock does not stop the enclave booting.
#
# Requires: bash, python3, vhost-device-vsock and a qemu-system-x86_64 that has the
# nitro-enclave machine (QEMU >= 9.2 built with libcbor and gnutls). Debian and Ubuntu
# build QEMU without libcbor at every version, so CI runs this inside a Fedora
# container that has all four, with --network host so the AF_VSOCK listener below
# reaches the guest. The vsock_loopback module is loaded on the host, outside the
# container; /dev/kvm is passed into the container where the runner has one.
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

KVM=()
if [ -w /dev/kvm ]; then KVM=(-enable-kvm -cpu host); fi

# The guest's date has to land between these two, give or take a second.
started=$(date +%s)
timeout 300 qemu-system-x86_64 \
  -M nitro-enclave,vsock=c,id=test \
  -chardev "socket,id=c,path=$RUN/vhost4.socket" \
  -kernel "$EIF" -nographic -m 1G -rtc base=2000-01-01 "${KVM[@]}" \
  > "$RUN/console.log" 2>&1 || true
finished=$(date +%s)

echo "--- guest console"; cat "$RUN/console.log"
echo "--- vsock harness"; cat "$RUN/harness.log"

grep -q "heartbeat answered" "$RUN/harness.log"
grep -q "started /app/hello" "$RUN/console.log"
grep -q "hello: unix time" "$RUN/console.log"
grep -q "hello: /dev/nsm present" "$RUN/console.log"
grep -q "shutting down" "$RUN/console.log"
grep -qE "exited /app/hello pid [0-9]+ status 0" "$RUN/console.log"

if [ ${#KVM[@]} -eq 0 ]; then
  # No hypercall, no PTP clock. init says so once and boots anyway.
  grep -q "time: /dev/ptp0 not present" "$RUN/console.log"
  echo "note: no /dev/kvm, skipping the time-sync assertions"
  exit 0
fi

# Either path through time_sync has to produce this: nested virtualisation can
# clear the pvclock TSC-stable bit, which makes PTP_SYS_OFFSET_PRECISE2 answer
# EOPNOTSUPP and sends the sample through the PTP_SYS_OFFSET2 fallback. The
# console says which one ran, so a failure here is diagnosable.
if grep -q "time: /dev/ptp0 has no cross-timestamp" "$RUN/console.log"; then
  echo "note: PTP_SYS_OFFSET2 fallback path"
else
  echo "note: PTP_SYS_OFFSET_PRECISE2 path"
fi
grep -q "time: stepped by" "$RUN/console.log"

# -rtc base=2000-01-01 put the guest 26 years behind; if the step worked, the date
# /app/hello printed is the host's. The window is the run itself plus a second on
# each side, because the host clock moved while QEMU was booting.
guest=$(sed -n 's/^.*hello: unix time \([0-9][0-9]*\).*$/\1/p' "$RUN/console.log" | head -1)
echo "guest unix time $guest, host $started..$finished"
if [ "$guest" -lt "$((started - 1))" ] || [ "$guest" -gt "$((finished + 1))" ]; then
  echo "guest clock is not the host's" >&2
  exit 1
fi
