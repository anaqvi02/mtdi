// hook-mechanism cost vs a naked-call baseline:
// fullcontext: 64-reg save/restore + dispatcher + trampoline, no-op handler
// fastpath: handler called directly, forwards through the trampoline
// run: cargo run --release --bin bench; cold/contention: bench_cold

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

/// detour target: loads trampoline addr and jumps to it
///
/// # Safety
/// TRAMP_FAST must be initialized before first call
#[unsafe(no_mangle)]
pub unsafe extern "C" fn handler_fast() {
    // forward through the trampoline, like the real fastpath
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
