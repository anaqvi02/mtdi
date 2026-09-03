// worst-case hook cost: cold icache, evicted caches, first call, contention
// run: cargo run --release --bin bench_cold
// caveats: mach_absolute_time ticks ~42ns on m-series, single shots coarse
// means batch-timed via instant; dvfs may boost mid-run
// "cold" = fresh process + thrash, not a thermal reset
// the log-reader thread spins in the background, stealing ~1 core during contention

use std::arch::global_asm;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use mtdi_lib::hook::manager::{install_hook, HookType};
use mtdi_lib::hook::trampoline::thunk::RegisterContext;

extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    fn mach_task_self() -> u32;
    fn mach_vm_protect(task: u32, addr: u64, size: u64, set_maximum: i32, prot: i32) -> i32;
}
const VM_PROT_READ: i32 = 0x01;
const VM_PROT_EXECUTE: i32 = 0x04;

#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}
static mut TB: Timebase = Timebase { numer: 0, denom: 0 };

fn now_ns() -> f64 {
    unsafe {
        if TB.numer == 0 {
            mach_timebase_info(&raw mut TB);
        }
        mach_absolute_time() as f64 * TB.numer as f64 / TB.denom as f64
    }
}

global_asm!(r#"
    .global _t_plain
    .align 4
_t_plain:
    nop
    nop
    nop
    nop
    nop
    ret

    .global _t_ctx
    .align 4
_t_ctx:
    nop
    nop
    nop
    nop
    nop
    ret

    .global _t_fast
    .align 4
_t_fast:
    nop
    nop
    nop
    nop
    nop
    ret
"#);

extern "C" {
    fn t_plain();
    fn t_ctx();
    fn t_fast();
}

pub fn handler_ctx(_ctx: &mut RegisterContext) {}

static TRAMP_FAST: AtomicUsize = AtomicUsize::new(0);

/// detour target: loads trampoline addr and jumps to it
///
/// # Safety
/// TRAMP_FAST must be initialized before first call
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fast_handler() {
    // forward through the trampoline, like the real fastpath
    let tramp = TRAMP_FAST.load(Ordering::Relaxed);
    let f: unsafe extern "C" fn() = core::mem::transmute(tramp);
    f();
}

// thrashers

const THRASH_CODE_BYTES: usize = 32 * 1024 * 1024; // > L2i on M1/M2
const THRASH_CODE_FNS: usize = THRASH_CODE_BYTES / 4;
const THRASH_DATA_BYTES: usize = 64 * 1024 * 1024; // > L2d everywhere
static mut THRASH_CODE: *mut u8 = core::ptr::null_mut();
static mut THRASH_DATA: *mut u8 = core::ptr::null_mut();

fn init_thrashers() {
    unsafe {
        let c = libc::mmap(
            core::ptr::null_mut(),
            THRASH_CODE_BYTES,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        assert!(c != libc::MAP_FAILED, "code mmap failed");
        let bytes = core::slice::from_raw_parts_mut(c as *mut u8, THRASH_CODE_BYTES);
        for chunk in bytes.chunks_mut(4) {
            chunk.copy_from_slice(&0xD65F03C0u32.to_le_bytes()); // ret
        }
        // rw then r-x: macOS blocks anon rwx
        let kr = mach_vm_protect(
            mach_task_self(),
            c as u64,
            THRASH_CODE_BYTES as u64,
            0,
            VM_PROT_READ | VM_PROT_EXECUTE,
        );
        assert!(kr == 0, "mach_vm_protect failed: {}", kr);
        THRASH_CODE = c as *mut u8;

        let d = libc::mmap(
            core::ptr::null_mut(),
            THRASH_DATA_BYTES,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        assert!(d != libc::MAP_FAILED);
        THRASH_DATA = d as *mut u8;
    }
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// random 4-byte fns across 32mb: thrashes btb, icache, tlb
#[inline(never)]
fn thrash_code(rng: &mut XorShift, n: usize) {
    unsafe {
        for _ in 0..n {
            let idx = rng.below(THRASH_CODE_FNS);
            let f: extern "C" fn() = core::mem::transmute(THRASH_CODE.add(idx * 4));
            f();
        }
    }
}

// 64mb touch: evicts l1/l2 and tlb entries for the hook's own pages
#[inline(never)]
fn thrash_data() {
    unsafe {
        let s = core::slice::from_raw_parts_mut(THRASH_DATA, THRASH_DATA_BYTES);
        let mut acc = 0u8;
        for chunk in s.chunks_mut(4096) {
            acc ^= chunk[0];
            chunk[0] = acc;
        }
        core::hint::black_box(acc);
    }
}

// measurements

fn stat(t: &[f64], label: &str) {
    let mut s = t.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    let p50 = s[(s.len() as f64 * 0.50) as usize];
    let p99 = s[(s.len() as f64 * 0.99) as usize];
    let max = s[s.len() - 1];
    println!(
        "  {:<28} min {:>8.1}  p50 {:>8.1}  mean {:>8.1}  p99 {:>9.1}  max {:>9.1} ns",
        label, s[0], p50, mean, p99, max
    );
}

fn cold_trials(target: unsafe extern "C" fn(), trials: usize, rng: &mut XorShift) -> Vec<f64> {
    let mut out = Vec::with_capacity(trials);
    for _ in 0..trials {
        thrash_code(rng, 4096);
        thrash_data();
        let t0 = now_ns();
        unsafe { target() };
        out.push((now_ns() - t0).max(0.0));
    }
    out
}

fn contention(target: unsafe extern "C" fn(), label: &str, nthreads: usize, iters: usize) {
    let start = Instant::now();
    let maxes: Vec<f64> = std::thread::scope(|s| {
        let mut hs = Vec::new();
        for _ in 0..nthreads {
            hs.push(s.spawn(move || {
                let mut mx = 0.0f64;
                for _ in 0..iters {
                    let t0 = now_ns();
                    unsafe { target() };
                    let dt = now_ns() - t0;
                    if dt > mx {
                        mx = dt;
                    }
                }
                mx
            }));
        }
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let per_call = start.elapsed().as_nanos() as f64 / (nthreads * iters) as f64;
    let max = maxes.iter().cloned().fold(0.0f64, f64::max);
    println!(
        "  {:<28} mean {:>8.1}  worst-thread-max {:>9.1} ns  ({} threads x {} iters)",
        label, per_call, max, nthreads, iters
    );
}

fn main() {
    unsafe { mach_timebase_info(&raw mut TB) };
    init_thrashers();
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);

    println!("=== MTDI worst-case hook-cost battery ===");
    println!("{}MB code thrasher, {}MB data thrasher, fresh process", THRASH_CODE_BYTES >> 20, THRASH_DATA_BYTES >> 20);
    println!();

    let _tctx = install_hook("t_ctx", t_ctx as usize, HookType::FullContext(handler_ctx)).unwrap();
    let tfast = install_hook("t_fast", t_fast as usize, HookType::FastPath(fast_handler as usize)).unwrap();
    TRAMP_FAST.store(tfast, Ordering::Relaxed);

    println!("[1] FullContext");
    // first call: everything cold, predictor empty
    let t0 = now_ns();
    unsafe { t_ctx() };
    let first = now_ns() - t0;
    println!("  {:<28} {:>8.1} ns  (single sample, first call after install)", "first-call-after-install", first);

    let n = 1_000_000;
    let s = Instant::now();
    for _ in 0..n {
        unsafe { t_ctx() };
    }
    let warm = s.elapsed().as_nanos() as f64 / n as f64;
    println!("  {:<28} {:>8.1} ns  ({} iters, warm loop)", "warm loop", warm, n);

    let cold = cold_trials(t_ctx, 2000, &mut rng);
    stat(&cold, "cold (code+data thrash)");
    println!();

    println!("[2] FastPath");
    let s = Instant::now();
    for _ in 0..n {
        unsafe { t_fast() };
    }
    let warm_f = s.elapsed().as_nanos() as f64 / n as f64;
    println!("  {:<28} {:>8.1} ns  ({} iters, warm loop)", "warm loop", warm_f, n);

    let cold_f = cold_trials(t_fast, 2000, &mut rng);
    stat(&cold_f, "cold (code+data thrash)");
    println!();

    println!("[3] 8-thread contention (dispatcher global mutex)");
    let iters = 200_000;
    contention(t_plain, "baseline (unhooked)", 8, iters);
    contention(t_ctx, "FullContext hooked", 8, iters);
    contention(t_fast, "FastPath hooked", 8, iters);
}
