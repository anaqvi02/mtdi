// bench.rs — MTDI hook-overhead microbenchmark.
//
// Measures the two hook styles shipped by the engine against a naked-call
// baseline. The result is the cost of the hook mechanism itself — the detour,
// the dispatch, and the trampoline hop — NOT the hooked function's own code
// (identical in both runs, cancels out) and NOT any handler work (the handlers
// here are a no-op and a pure forwarder).
//   [1] FullContext — full register save/restore (64 regs) + dispatcher +
//       trampoline, no-op handler. Worst-case fixed cost of the engine.
//   [2] FastPath — handler is called directly with no context save; the
//       handler forwards through the trampoline exactly like the real
//       libc::open hook in src/lib.rs (atomic load + indirect call).
//
// Run:  cargo run --release --bin bench
// Cold/contention numbers: cargo run --release --bin bench_cold

use std::arch::global_asm;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use mtdi_lib::hook::manager::{install_hook, HookType};
use mtdi_lib::hook::trampoline::thunk::RegisterContext;

global_asm!(r#"
    .global _target_baseline
    .align 4
_target_baseline:
    nop
    nop
    nop
    nop
    nop
    ret

    .global _target_uprobe
    .align 4
_target_uprobe:
    nop
    nop
    nop
    nop
    nop
    ret

    .global _target_fast
    .align 4
_target_fast:
    nop
    nop
    nop
    nop
    nop
    ret
"#);

extern "C" {
    fn target_baseline();
    fn target_uprobe();
    fn target_fast();
}

pub fn handler_uprobe(_ctx: &mut RegisterContext) {}

static TRAMP_FAST: AtomicUsize = AtomicUsize::new(0);

/// FastPath forwarding handler: loads the trampoline address and jumps to it.
///
/// # Safety
/// `TRAMP_FAST` must be initialized (by `install_hook`) before the first call;
/// the engine invokes this via the detour with arbitrary register state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn handler_fast() {
    // Forward through the trampoline — same shape as the real FastPath
    // handler (the libc::open hook in src/lib.rs).
    let tramp = TRAMP_FAST.load(Ordering::Relaxed);
    let f: unsafe extern "C" fn() = core::mem::transmute(tramp);
    f();
}

fn main() {
    println!("========================================");
    println!("            MTDI Benchmark              ");
    println!("========================================");

    install_hook("target_uprobe", target_uprobe as usize, HookType::FullContext(handler_uprobe)).unwrap();
    let tfast = install_hook("target_fast", target_fast as usize, HookType::FastPath(handler_fast as usize)).unwrap();
    TRAMP_FAST.store(tfast, Ordering::Relaxed);

    let iterations = 1_000_000;
    println!("Running {} iterations per test...\n", iterations);

    let start = Instant::now();
    for _ in 0..iterations {
        unsafe { target_baseline(); }
    }
    let baseline_ns = start.elapsed().as_nanos() as f64 / iterations as f64;

    let start = Instant::now();
    for _ in 0..iterations {
        unsafe { target_uprobe(); }
    }
    let uprobe_ns = start.elapsed().as_nanos() as f64 / iterations as f64;
    let uprobe_overhead = uprobe_ns - baseline_ns;

    let start = Instant::now();
    for _ in 0..iterations {
        unsafe { target_fast(); }
    }
    let fast_ns = start.elapsed().as_nanos() as f64 / iterations as f64;
    let fast_overhead = fast_ns - baseline_ns;

    println!("--------------------------------------------------");
    println!(" Hook Type                   | Overhead (per call)");
    println!("--------------------------------------------------");
    println!(" [1] FullContext Uprobe      | {:>7.2} ns", uprobe_overhead);
    println!(" [2] FastPath                | {:>7.2} ns", fast_overhead);
    println!("==================================================");
}
