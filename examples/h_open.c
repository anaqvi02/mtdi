// h_open.c — real-world open() cost harness for mtdi.
// mode 0: warm loop      (200k opens, back to back)
// mode 1: cold scattered (5k opens, 16MB memory touch between each)
// mode 2: first call     (single open, cold process)
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <sys/time.h>
#include <sys/mman.h>

static double now(void) {
    struct timeval tv;
    gettimeofday(&tv, 0);
    return tv.tv_sec + tv.tv_usec * 1e-6;
}

int main(int argc, char **argv) {
    int mode = argc > 1 ? atoi(argv[1]) : 0;
    char *buf = mmap(0, 16 * 1024 * 1024, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (buf == MAP_FAILED) return 2;

    if (mode == 2) {
        double t0 = now();
        int fd = open("/dev/null", O_RDONLY);
        close(fd);
        printf("first-open: %.0f ns\n", (now() - t0) * 1e9);
        return 0;
    }

    if (mode == 0) {
        const int N = 200000;
        double t0 = now();
        for (int i = 0; i < N; i++) {
            int fd = open("/dev/null", O_RDONLY);
            close(fd);
        }
        double dt = now() - t0;
        printf("warm: %.1f ns/op (%d ops, %.3fs)\n", dt * 1e9 / N, N, dt);
        return 0;
    }

    // mode 1: cold scattered
    const int N = 5000;
    double t0 = now();
    for (int i = 0; i < N; i++) {
        for (int j = 0; j < 16 * 1024 * 1024 / 4096; j++) buf[j * 4096] = (char)i;
        int fd = open("/dev/null", O_RDONLY);
        close(fd);
    }
    double dt = now() - t0;
    printf("cold: %.1f ns/op (%d ops, 16MB touch each, %.3fs)\n", dt * 1e9 / N, N, dt);
    return 0;
}
