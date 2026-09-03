// dobby_bench.c — Dobby vs mtdi inline hook overhead benchmark.
// Same target shape as mtdi's bench: 5 nops + ret.
#include <stdio.h>
#include <mach/mach_time.h>
#include <dlfcn.h>
#include "dobby.h"

// The target function — same as mtdi bench targets
void __attribute__((noinline)) target_func(void) {
    __asm__ volatile("nop\nnop\nnop\nnop\nnop\n");
}

// DobbyInstrument callback — saves register context (like mtdi FullContext)
static void instrument_cb(void *address, DobbyRegisterContext *ctx) {
    // Empty — just measure the dispatch overhead
}

// DobbyHook callback — minimal (like mtdi FastPath)
static void fake_func(void) {
    // Forward to original
}

int main(void) {
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);

    const long long ITERATIONS = 1000000LL;
    const int WARMUP = 10000;

    // Disable near trampoline to use default Dobby behavior
    // dobby_set_near_trampoline(false);

    // --- Native baseline ---
    for (int i = 0; i < WARMUP; i++) target_func();

    uint64_t t0, t1;
    t0 = mach_absolute_time();
    for (long long i = 0; i < ITERATIONS; i++) target_func();
    t1 = mach_absolute_time();
    double native_ns = (double)(t1 - t0) * tb.numer / tb.denom / ITERATIONS;

    // --- DobbyHook (inline hook, like FastPath) ---
    void *orig = NULL;
    DobbyHook((void *)target_func, (void *)fake_func, &orig);

    t0 = mach_absolute_time();
    for (long long i = 0; i < ITERATIONS; i++) target_func();
    t1 = mach_absolute_time();
    double dobby_hook_ns = (double)(t1 - t0) * tb.numer / tb.denom / ITERATIONS;

    DobbyDestroy((void *)target_func);

    // --- DobbyInstrument (register context, like FullContext) ---
    DobbyInstrument((void *)target_func, instrument_cb);

    t0 = mach_absolute_time();
    for (long long i = 0; i < ITERATIONS; i++) target_func();
    t1 = mach_absolute_time();
    double dobby_instrument_ns = (double)(t1 - t0) * tb.numer / tb.denom / ITERATIONS;

    DobbyDestroy((void *)target_func);

    // --- Results ---
    printf("==================================================\n");
    printf("  Dobby vs mtdi: Inline Hook Overhead\n");
    printf("  1M iterations, 5-nop + ret target\n");
    printf("==================================================\n\n");
    printf("  Native baseline:         %6.1f ns/call\n", native_ns);
    printf("  DobbyHook (FastPath):    %6.1f ns/call  (overhead: %.1f ns)\n",
           dobby_hook_ns, dobby_hook_ns - native_ns);
    printf("  DobbyInstrument (Full):  %6.1f ns/call  (overhead: %.1f ns)\n",
           dobby_instrument_ns, dobby_instrument_ns - native_ns);
    printf("\n  --- mtdi reference ---\n");
    printf("  mtdi FastPath:           ~1.6 ns overhead\n");
    printf("  mtdi FullContext:        ~15 ns overhead\n");
    printf("\n");

    double dh = dobby_hook_ns - native_ns;
    double di = dobby_instrument_ns - native_ns;
    if (dh > 0) {
        printf("  DobbyHook vs mtdi FastPath:     mtdi is %.0fx faster\n", dh / 1.6);
    }
    if (di > 0) {
        printf("  DobbyInstrument vs mtdi Full:   mtdi is %.0fx faster\n", di / 15.0);
    }
    printf("==================================================\n");
    return 0;
}
