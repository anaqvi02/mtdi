use mach2::kern_return::kern_return_t;
use mach2::traps::mach_task_self;
use mach2::vm::mach_vm_protect;
use mach2::vm_prot::{VM_PROT_COPY, VM_PROT_EXECUTE, VM_PROT_READ, VM_PROT_WRITE};

extern "C" {
    fn sys_icache_invalidate(start: *mut libc::c_void, len: usize);
}

/// One fork per unique page, not per symbol: the 25 libc stubs share far
/// fewer pages, and a fork copies the full page table (~1ms each).
static PROBED_PAGES: [core::sync::atomic::AtomicUsize; 32] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; 32];
static PROBED_N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

fn probe_page(page_start: usize, addr: usize) -> bool {
    unsafe {
        match libc::fork() {
            -1 => false, // fork failed: be conservative and skip the hook
            0 => {
                let kr = mach_vm_protect(
                    mach_task_self(),
                    page_start as u64,
                    16384,
                    0,
                    VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY,
                );
                if kr == mach2::kern_return::KERN_SUCCESS {
                    let p = addr as *mut u8;
                    core::ptr::write_volatile(p, core::ptr::read_volatile(p));
                }
                libc::_exit(if kr == mach2::kern_return::KERN_SUCCESS { 0 } else { 2 });
            }
            pid => {
                let mut status = 0;
                libc::waitpid(pid, &mut status, 0);
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
            }
        }
    }
}

/// Safely probes whether the page containing `addr` accepts writes, without
/// risking the process. On Apple Silicon, PPL-protected shared-cache pages
/// (a subset of libsystem_kernel's text) make mach_vm_protect report success
/// but fault on the actual write — which would kill the whole traced process.
/// The probe runs in a forked child that re-protects the page itself, so a
/// fault takes down only the child; the parent classifies via waitpid.
/// Verdicts are cached per page (entry = page | writable bit).
fn page_accepts_writes(addr: usize) -> bool {
    let page_start = addr & !(16384 - 1);
    for e in PROBED_PAGES.iter() {
        let e = e.load(core::sync::atomic::Ordering::Relaxed);
        if e != 0 && (e & !1) == page_start {
            return e & 1 == 1;
        }
    }
    let writable = probe_page(page_start, addr);
    let n = PROBED_N.load(core::sync::atomic::Ordering::Relaxed);
    if n < PROBED_PAGES.len() {
        PROBED_PAGES[n].store(page_start | writable as usize, core::sync::atomic::Ordering::Relaxed);
        PROBED_N.store(n + 1, core::sync::atomic::Ordering::Relaxed);
    }
    writable
}

/// Returns the mach_vm_protect result (KERN_SUCCESS = writable, anything else
/// means the page refused to become writable — e.g. PPL-protected shared-cache
/// pages on Apple Silicon — and the caller must NOT write to it).
pub fn unprotect_page(addr: usize) -> kern_return_t {
    let page_size = 16384;
    let page_start = addr & !(page_size - 1);
    unsafe {
        mach_vm_protect(
            mach_task_self(),
            page_start as u64,
            page_size as u64,
            0,
            VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY,
        )
    }
}

pub fn protect_page(addr: usize) -> kern_return_t {
    let page_size = 16384;
    let page_start = addr & !(page_size - 1);
    unsafe {
        mach_vm_protect(
            mach_task_self(),
            page_start as u64,
            page_size as u64,
            0,
            VM_PROT_READ | VM_PROT_EXECUTE,
        )
    }
}

/// Overwrites the first 16 bytes at `target_addr` with an absolute jump to
/// `hook_addr` (LDR X16, #8; BR X16; .dword hook_addr), after unprotecting
/// and re-protecting the containing page.
///
/// # Safety
/// `target_addr` must point to at least 16 bytes of mapped, executable memory
/// (typically the prologue of a live function). The caller must ensure no other
/// thread is executing those bytes concurrently.
pub unsafe fn overwrite_with_jump(target_addr: usize, hook_addr: usize) -> Result<[u8; 16], String> {
    let target_ptr = target_addr as *mut u8;

    let mut original_bytes = [0u8; 16];
    std::ptr::copy_nonoverlapping(target_ptr, original_bytes.as_mut_ptr(), 16);

    let mut payload = [0u8; 16];
    payload[0..4].copy_from_slice(&0x58000050u32.to_le_bytes()); // ldr x16, #8
    payload[4..8].copy_from_slice(&0xD61F0200u32.to_le_bytes()); // br x16
    payload[8..16].copy_from_slice(&(hook_addr as u64).to_le_bytes());

    // Probe FIRST, on the pristine page: the forked child must be the one to
    // unprotect + write, or the COW materialization faults even on writable
    // pages (PPL quirk).
    if !page_accepts_writes(target_addr) {
        return Err(format!(
            "page at {:#x} is PPL-protected (write probe faulted)",
            target_addr
        ));
    }

    let kr = unprotect_page(target_addr);
    if kr != mach2::kern_return::KERN_SUCCESS {
        // Page refused to become writable (PPL-protected shared-cache page on
        // Apple Silicon). Do NOT write — the fault would kill the process.
        return Err(format!("cannot unprotect page at {:#x} (kr={})", target_addr, kr));
    }

    std::ptr::copy_nonoverlapping(payload.as_ptr(), target_ptr, 16);
    sys_icache_invalidate(target_ptr as *mut libc::c_void, 16);

    protect_page(target_addr);

    Ok(original_bytes)
}

pub mod trampoline;
pub mod manager;
