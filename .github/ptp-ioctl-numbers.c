/*
 * The PTP ioctl request numbers and struct sizes eiffel-init has to agree with,
 * straight from the kernel it will run on.
 *
 * CI compiles this inside the Dockerfile's kernel stage, against the unpacked
 * tree's own include/uapi/linux/ptp_clock.h (which is why it warns about using
 * kernel headers from user space — that is the point), and diffs its output
 * against .github/ptp-ioctl-numbers.txt,
 * which eiffel-init's own unit test checks its nix-derived numbers against. A
 * header that changes under us therefore fails the build rather than leaving an
 * enclave unable to read the host clock.
 */
#include <stddef.h>
#include <stdio.h>
#include <linux/ptp_clock.h>

int main(void) {
    printf("PTP_SYS_OFFSET_PRECISE2 0x%lx\n", (unsigned long)PTP_SYS_OFFSET_PRECISE2);
    printf("PTP_SYS_OFFSET2 0x%lx\n", (unsigned long)PTP_SYS_OFFSET2);
    printf("sizeof(struct ptp_sys_offset_precise) %zu\n", sizeof(struct ptp_sys_offset_precise));
    printf("sizeof(struct ptp_sys_offset) %zu\n", sizeof(struct ptp_sys_offset));
    return 0;
}
