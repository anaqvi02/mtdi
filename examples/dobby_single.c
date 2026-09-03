// dobby_single.c — measure SINGLE CALL overhead with mach_absolute_time.
// No averaging tricks, no pipeline absorption.
#include <stdio.h>
#include <string.h>
#include <mach/mach_time.h>
#include <dlfcn.h>
#include "dobby.h"

void __attribute__((noinline)) target_func(void) {
    __asm__ volatile("nop\nnop\nnop\nnop\nnop\n");
}

static void fake_func(void) {}

static void instrument_cb(void *address, DobbyRegisterContext *ctx) {
    // Empty — just measure dispatch cost
}

int main(void) {
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);

    const int SAMPLES = 10000;
    const int WARMUP = 50000;

    printf("mach_absolute_time resolution: %.2f ns/tick\n", (double)tb.numer / tb.denom);
    printf("Measuring single-call overhead over %d samples\n\n", SAMPLES);

    // Warm up branch predictor
    for (int i = 0; i < WARMUP; i++) target_func();

    // --- Native single-call ---
    uint64_t native_times[SAMPLES];
    for (int i = 0; i < SAMPLES; i++) {
        uint64_t t0 = mach_absolute_time();
        target_func();
        uint64_t t1 = mach_absolute_time();
        native_times[i] = t1 - t0;
    }

    // --- DobbyHook single-call ---
    void *orig = NULL;
    DobbyHook((void *)target_func, (void *)fake_func, &orig);

    // Warm up the hook
    for (int i = 0; i < WARMUP; i++) target_func();

    uint64_t hook_times[SAMPLES];
    for (int i = 0; i < SAMPLES; i++) {
        uint64_t t0 = mach_absolute_time();
        target_func();
        uint64_t t1 = mach_absolute_time();
        hook_times[i] = t1 - t0;
    }
    DobbyDestroy((void *)target_func);

    // --- DobbyInstrument single-call ---
    DobbyInstrument((void *)target_func, instrument_cb);

    for (int i = 0; i < WARMUP; i++) target_func();

    uint64_t instrument_times[SAMPLES];
    for (int i = 0; i < SAMPLES; i++) {
        uint64_t t0 = mach_absolute_time();
        target_func();
        uint64_t t1 = mach_absolute_time();
        instrument_times[i] = t1 - t0;
    }
    DobbyDestroy((void *)target_func);

    // Sort and compute stats
    #define SORT(arr, n) do { \
        for (int i = 0; i < n-1; i++) \
            for (int j = i+1; j < n; j++) \
                if (arr[j] < arr[i]) { uint64_t tmp = arr[i]; arr[i] = arr[j]; arr[j] = tmp; } \
    } while(0)

    #define STATS(arr, n, label) do { \
        SORT(arr, n); \
        uint64_t sum = 0; int nz = 0; \
        for (int i = 0; i < n; i++) { if (arr[i] > 0) { sum += arr[i]; nz++; } } \
        printf("  %-24s min %3llu  p50 %3llu  p95 %3llu  max %3llu  mean %.1f ns  (zero-time: %d/%d)\n", \
            label, arr[0], arr[n/2], arr[(int)(n*0.95)], arr[n-1], \
            nz > 0 ? (double)sum / nz * (double)tb.numer / tb.denom : 0.0, n - nz, n); \
    } while(0)

    printf("%-26s  %s\n", "", "nanoseconds (sorted)");
    printf("------------------------------------------------------------\n");
    STATS(native_times, SAMPLES, "Native (no hook):");
    STATS(hook_times, SAMPLES, "DobbyHook (FastPath):");
    STATS(instrument_times, SAMPLES, "DobbyInstrument (Full):");

    printf("\n--- mtdi reference (from bench_cold) ---\n");
    printf("  mtdi FullContext cold p50:  42 ns\n");
    printf("  mtdi FullContext cold p99: 208 ns\n");
    printf("  mtdi FastPath cold p50:    ~0 ns\n");
    printf("  mtdi FastPath cold p99:    42 ns\n");

    return 0;
}
