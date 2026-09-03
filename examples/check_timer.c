#include <stdio.h>
#include <time.h>
#include <mach/mach_time.h>

int main(void) {
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);
    printf("mach_absolute_time timebase: %u/%u (tick = %.2f ns)\n", tb.numer, tb.denom,
           (double)tb.numer / tb.denom);

    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    printf("clock_gettime resolution test:\n");

// measure smallest measurable difference
    struct timespec a, b;
    long long diffs[100];
    for (int i = 0; i < 100; i++) {
        clock_gettime(CLOCK_MONOTONIC, &a);
        clock_gettime(CLOCK_MONOTONIC, &b);
        diffs[i] = (b.tv_sec - a.tv_sec) * 1000000000LL + (b.tv_nsec - a.tv_nsec);
    }
    long long min_diff = diffs[0], max_diff = diffs[0];
    int zero_count = 0;
    for (int i = 0; i < 100; i++) {
        if (diffs[i] < min_diff) min_diff = diffs[i];
        if (diffs[i] > max_diff) max_diff = diffs[i];
        if (diffs[i] == 0) zero_count++;
    }
    printf("  back-to-back reads: min=%lld  max=%lld  zeros=%d/100\n", min_diff, max_diff, zero_count);

    struct timespec start, end;
    volatile int x = 0;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (long long i = 0; i < 1000000LL; i++) { x = i; }
    clock_gettime(CLOCK_MONOTONIC, &end);
    double loop_ns = ((double)(end.tv_sec - start.tv_sec) * 1e9 + (double)(end.tv_nsec - start.tv_nsec));
    printf("  1M plain loop iterations: %.0f ns total, %.4f ns/iter\n", loop_ns, loop_ns / 1e6);
    printf("  (if this is ~0, clock_gettime resolution is too low to measure us)\n");
    return 0;
}
