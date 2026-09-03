// single-call cost via mach_absolute_time
#include <stdio.h>
#include <mach/mach_time.h>

void __attribute__((noinline)) target_func(void) {
    __asm__ volatile("nop\nnop\nnop\nnop\nnop\n");
}

int main(void) {
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);

    // warm up branch predictor
    for (int i = 0; i < 10000; i++) target_func();

    // many samples -> distribution
    const int SAMPLES = 100000;
    uint64_t times[100000];

    for (int i = 0; i < SAMPLES; i++) {
        uint64_t t0 = mach_absolute_time();
        target_func();
        uint64_t t1 = mach_absolute_time();
        times[i] = (t1 - t0) * tb.numer / tb.denom;
    }

    for (int i = 0; i < SAMPLES - 1; i++)
        for (int j = i + 1; j < SAMPLES; j++)
            if (times[j] < times[i]) {
                uint64_t tmp = times[i]; times[i] = times[j]; times[j] = tmp;
            }

    // drop zero-time samples (timer granularity)
    int nonzero = 0;
    uint64_t sum = 0;
    for (int i = 0; i < SAMPLES; i++) {
        if (times[i] > 0) { nonzero++; sum += times[i]; }
    }

    printf("mach_absolute_time resolution: %.2f ns/tick\n", (double)tb.numer / tb.denom);
    printf("Samples: %d total, %d with nonzero time\n", SAMPLES, nonzero);
    printf("Single target_func() call:\n");
    printf("  min:  %3llu ns\n", times[0]);
    printf("  p1:   %3llu ns\n", times[(int)(SAMPLES * 0.01)]);
    printf("  p5:   %3llu ns\n", times[(int)(SAMPLES * 0.05)]);
    printf("  p50:  %3llu ns\n", times[SAMPLES / 2]);
    printf("  p95:  %3llu ns\n", times[(int)(SAMPLES * 0.95)]);
    printf("  p99:  %3llu ns\n", times[(int)(SAMPLES * 0.99)]);
    printf("  max:  %3llu ns\n", times[SAMPLES - 1]);
    if (nonzero > 0)
        printf("  mean: %.1f ns\n", (double)sum / nonzero);
    printf("  zero-time samples: %d/%d (%.1f%%)\n",
           SAMPLES - nonzero, SAMPLES, (double)(SAMPLES - nonzero) / SAMPLES * 100);

    return 0;
}
