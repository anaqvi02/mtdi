use std::arch::global_asm;
use std::time::Instant;

use mactrace_lib::hook::manager::{install_hook, HookType};
use mactrace_lib::hook::trampoline::thunk::RegisterContext;

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
"#);

extern "C" {
    fn target_baseline();
    fn target_uprobe();
}

pub fn handler_uprobe(_ctx: &mut RegisterContext) {}

fn main() {
    println!("========================================");
    println!("            MTDI Benchmark              ");
    println!("========================================");

    install_hook("target_uprobe", target_uprobe as usize, HookType::FullContext(handler_uprobe)).unwrap();

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

    println!("--------------------------------------------------");
    println!(" Hook Type                   | Overhead (per call)");
    println!("--------------------------------------------------");
    println!(" [1] FullContext Uprobe      | {:>7.2} ns", uprobe_overhead);
    println!("==================================================");
}
