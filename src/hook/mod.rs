use mach2::kern_return::kern_return_t;
use mach2::traps::mach_task_self;
use mach2::vm::mach_vm_protect;
use mach2::vm_prot::{VM_PROT_COPY, VM_PROT_EXECUTE, VM_PROT_READ, VM_PROT_WRITE};

extern "C" {
    pub(crate) fn sys_icache_invalidate(start: *mut libc::c_void, len: usize);
}

/// one fork per unique page, not per symbol (stubs share pages;
/// fork copies page tables, ~1ms each)
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

/// probes write-ability in a forked child so a fault (e.g. PPL) kills
/// only the child; verdicts cached per page
pub(crate) fn page_accepts_writes(addr: usize) -> bool {
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

/// mach_vm_protect result: non-success = page refuses writes (e.g. ppl),
/// caller must not write
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

/// overwrites 16 bytes at target with an absolute jump to hook
/// (ldr x16,#8; br x16; .dword hook), unprotecting and re-protecting the page
///
/// # Safety
/// target must be 16+ bytes of mapped executable memory; no other thread
/// may be executing it concurrently
pub unsafe fn overwrite_with_jump(target_addr: usize, hook_addr: usize) -> Result<(), String> {
    let target_ptr = target_addr as *mut u8;

    let mut payload = [0u8; 16];
    payload[0..4].copy_from_slice(&0x58000050u32.to_le_bytes()); // ldr x16, #8
    payload[4..8].copy_from_slice(&0xD61F0200u32.to_le_bytes()); // br x16
    payload[8..16].copy_from_slice(&(hook_addr as u64).to_le_bytes());

    // probe first on the pristine page; the child must do the writes,
    // or cow materialization faults even on writable pages
    if !page_accepts_writes(target_addr) {
        return Err(format!(
            "page at {:#x} is PPL-protected (write probe faulted)",
            target_addr
        ));
    }

    let kr = unprotect_page(target_addr);
    if kr != mach2::kern_return::KERN_SUCCESS {
        // page refused writes (ppl). do not write; the fault would kill
        // the whole process
        return Err(format!("cannot unprotect page at {:#x} (kr={})", target_addr, kr));
    }

    std::ptr::copy_nonoverlapping(payload.as_ptr(), target_ptr, 16);
    sys_icache_invalidate(target_ptr as *mut libc::c_void, 16);

    protect_page(target_addr);

    Ok(())
}

pub mod trampoline;
pub mod manager;
