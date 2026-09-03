// minimal target: native run = baseline, frida run = overhead

#include <stdio.h>
#include <time.h>

// 5 nops + ret, same shape as mtdi bench targets
void __attribute__((noinline)) target_func(void) {
    __asm__ volatile(
        "nop\nnop\nnop\nnop\nnop\n"
    );
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

    // per-call ns for the frida script to parse
    printf("%.2f ns/call (%lld iterations)\n", per_call_ns, ITERATIONS);
    return 0;
}
