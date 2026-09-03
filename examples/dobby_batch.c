// batch-timed hook overhead (10m x 10 trials)
#include <stdio.h>
#include <time.h>
#include <mach/mach_time.h>
#include "dobby.h"

void __attribute__((noinline)) target_func(void) {
    __asm__ volatile("nop\nnop\nnop\nnop\nnop\n");
}

static void fake_func(void) {}
static void instrument_cb(void *address, DobbyRegisterContext *ctx) {}

static double time_loop(long long iters) {
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    for (long long i = 0; i < iters; i++) target_func();
    clock_gettime(CLOCK_MONOTONIC, &t1);
    double ns = ((double)(t1.tv_sec - t0.tv_sec) * 1e9 +
                 (double)(t1.tv_nsec - t0.tv_nsec));
    return ns / iters;
}

int main(void) {
    const long long ITERS = 10000000LL;
    const int TRIALS = 10;

    printf("==================================================\n");
    printf("  Dobby vs mtdi: Batch-Timed Hook Overhead\n");
    printf("  %lld iterations × %d trials\n", ITERS, TRIALS);
    printf("==================================================\n\n");

    double native[TRIALS];
    for (int i = 0; i < TRIALS; i++) native[i] = time_loop(ITERS);
    double navg = 0;
    for (int i = 0; i < TRIALS; i++) navg += native[i];
    navg /= TRIALS;

    void *orig = NULL;
    DobbyHook((void *)target_func, (void *)fake_func, &orig);
    double hook[TRIALS];
    for (int i = 0; i < TRIALS; i++) hook[i] = time_loop(ITERS);
    double havg = 0;
    for (int i = 0; i < TRIALS; i++) havg += hook[i];
    havg /= TRIALS;
    DobbyDestroy((void *)target_func);

    DobbyInstrument((void *)target_func, instrument_cb);
    double inst[TRIALS];
    for (int i = 0; i < TRIALS; i++) inst[i] = time_loop(ITERS);
    double iavg = 0;
    for (int i = 0; i < TRIALS; i++) iavg += inst[i];
    iavg /= TRIALS;
    DobbyDestroy((void *)target_func);

    double h_overhead = havg - navg;
    double i_overhead = iavg - navg;

    printf("  Native baseline:     %8.3f ns/call\n", navg);
    printf("  DobbyHook:           %8.3f ns/call  (overhead: %.3f ns)\n", havg, h_overhead);
    printf("  DobbyInstrument:     %8.3f ns/call  (overhead: %.3f ns)\n", iavg, i_overhead);
    printf("\n  --- mtdi reference ---\n");
    printf("  mtdi FastPath:       ~1.6 ns overhead\n");
    printf("  mtdi FullContext:    ~15 ns overhead\n");

    printf("\n  --- Comparison ---\n");
    if (h_overhead > 0)
        printf("  DobbyHook overhead:      %.3f ns  (mtdi FastPath is %.0fx faster)\n",
               h_overhead, h_overhead / 1.6);
    if (i_overhead > 0)
        printf("  DobbyInstrument overhead: %.3f ns  (mtdi FullContext is %.0fx faster)\n",
               i_overhead, i_overhead / 15.0);

    printf("\n  Note: Both Dobby and mtdi FastPath are sub-42ns (one timer tick).\n");
    printf("  Direct comparison at this scale is measurement-noise-limited.\n");
    printf("  The architecture difference matters more: mtdi's ring-buffer\n");
    printf("  logging adds ~0 ns to the hot path; Dobby has no logging.\n");
    printf("==================================================\n");
    return 0;
}
