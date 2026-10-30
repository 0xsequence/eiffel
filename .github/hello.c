/*
 * The guest application for the QEMU boot test.
 *
 * It exists to prove four things about eiffel-init at once: that it execs what
 * `cmd` names, that the child inherits the console (this output has to reach
 * QEMU's serial log without any piping), that /dev/nsm is there, and that a
 * child exiting 0 is what takes the enclave down.
 *
 * Built static in CI with `gcc -static`; it never ships in an image.
 */
#include <fcntl.h>
#include <stdio.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    printf("hello: unix time %lld\n", (long long)time(NULL));

    int fd = open("/dev/nsm", O_RDWR);
    printf("hello: /dev/nsm %s\n", fd < 0 ? "missing" : "present");
    if (fd >= 0) {
        close(fd);
    }

    fflush(stdout);
    return 0;
}
