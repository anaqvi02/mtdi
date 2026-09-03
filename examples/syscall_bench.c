// open() loop: frida real-trace overhead vs native
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <time.h>
#include <mach/mach_time.h>

int main(void) {
    const long long ITERATIONS = 1000000LL;

    char path[] = "/tmp/_mtdi_bench.XXXXXX";
    int fd = mkstemp(path);
    close(fd);

    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);

    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (long long i = 0; i < ITERATIONS; i++) {
        fd = open(path, O_RDONLY);
        close(fd);
    }
    clock_gettime(CLOCK_MONOTONIC, &end);

    double ns = ((double)(end.tv_sec - start.tv_sec) * 1e9 + (double)(end.tv_nsec - start.tv_nsec)) / (ITERATIONS * 2);
    FILE *f = fopen("/tmp/frida_bench_result.txt", "w");
    if (f) { fprintf(f, "%.2f\n", ns); fclose(f); }
    printf("%.2f ns/syscall (open+close pair)\n", ns);

    unlink(path);
    return 0;
}
