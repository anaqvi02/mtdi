use mach2::vm_prot::{VM_PROT_READ, VM_PROT_WRITE, VM_PROT_EXECUTE, VM_PROT_COPY};
use mach2::vm::mach_vm_protect;
use mach2::traps::mach_task_self;

extern "C" {
    fn sys_icache_invalidate(start: *mut libc::c_void, len: usize);
}

pub fn unprotect_page(addr: usize) {
    let page_size = 16384;
    let page_start = addr & !(page_size - 1);
    unsafe {
        mach_vm_protect(
            mach_task_self(),
            page_start as u64,
            page_size as u64,
            0,
            VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY,
        );
    }
}

pub fn protect_page(addr: usize) {
    let page_size = 16384;
    let page_start = addr & !(page_size - 1);
    unsafe {
        mach_vm_protect(
            mach_task_self(),
            page_start as u64,
            page_size as u64,
            0,
            VM_PROT_READ | VM_PROT_EXECUTE,
        );
    }
}

pub unsafe fn overwrite_with_jump(target_addr: usize, hook_addr: usize) -> [u8; 16] {
    let target_ptr = target_addr as *mut u8;
    
    let mut original_bytes = [0u8; 16];
    std::ptr::copy_nonoverlapping(target_ptr, original_bytes.as_mut_ptr(), 16);

    let mut payload = [0u8; 16];
    payload[0..4].copy_from_slice(&0x58000050u32.to_le_bytes()); // ldr x16, #8
    payload[4..8].copy_from_slice(&0xD61F0200u32.to_le_bytes()); // br x16
    payload[8..16].copy_from_slice(&(hook_addr as u64).to_le_bytes());

    unprotect_page(target_addr);

    std::ptr::copy_nonoverlapping(payload.as_ptr(), target_ptr, 16);
    sys_icache_invalidate(target_ptr as *mut libc::c_void, 16);
    
    protect_page(target_addr);

    original_bytes
}

pub mod trampoline;
pub mod manager;
