// writes timing to /tmp/frida_bench_result.txt
#include <stdio.h>
#include <time.h>
#include <unistd.h>

void __attribute__((noinline)) target_func(void) {
    __asm__ volatile("nop\nnop\nnop\nnop\nnop\n");
}

int main(void) {
    const long long ITERATIONS = 1000000LL;

    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (long long i = 0; i < ITERATIONS; i++) {
        target_func();
    }
    clock_gettime(CLOCK_MONOTONIC, &end);

    double elapsed_ns = (double)(end.tv_sec - start.tv_sec) * 1e9
                      + (double)(end.tv_nsec - start.tv_nsec);
    double per_call_ns = elapsed_ns / ITERATIONS;

    FILE *f = fopen("/tmp/frida_bench_result.txt", "w");
    if (f) {
        fprintf(f, "%.2f\n", per_call_ns);
        fclose(f);
    }

    // stdout for native runs
    printf("%.2f ns/call (%lld iterations)\n", per_call_ns, ITERATIONS);
    return 0;
}
