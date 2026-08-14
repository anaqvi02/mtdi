use super::allocator::allocate_trampoline;
use super::relocator::relocate_instruction;
use crate::hook::{unprotect_page, protect_page};

/// Builds a trampoline that re-executes the stolen prologue instructions and
/// branches back into the original function after the detour.
///
/// # Safety
/// `stolen_bytes` must be the exact 16 bytes previously read from
/// `original_addr` before the detour was installed, and `original_addr` must
/// still be mapped executable.
pub unsafe fn build_trampoline(original_addr: usize, stolen_bytes: &[u8; 16]) -> usize {
    let mut relocated_instructions = Vec::with_capacity(64);

    for i in 0..4 {
        let instruction = u32::from_le_bytes(stolen_bytes[(i * 4)..(i * 4 + 4)].try_into().unwrap());
        let current_pc = original_addr + i * 4;
        relocate_instruction(instruction, current_pc, &mut relocated_instructions);
    }

    let branch_back_size = 16;
    let trampoline_size = relocated_instructions.len() + branch_back_size;
    
    let tramp_addr = allocate_trampoline(trampoline_size);
    let tramp_ptr = tramp_addr as *mut u8;

    let return_addr = original_addr + 16;
    let mut branch_payload = [0u8; 16];
    branch_payload[0..4].copy_from_slice(&0x58000050u32.to_le_bytes());
    branch_payload[4..8].copy_from_slice(&0xD61F0200u32.to_le_bytes());
    branch_payload[8..16].copy_from_slice(&(return_addr as u64).to_le_bytes());

    unprotect_page(tramp_addr);

    std::ptr::copy_nonoverlapping(relocated_instructions.as_ptr(), tramp_ptr, relocated_instructions.len());
    std::ptr::copy_nonoverlapping(
        branch_payload.as_ptr(),
        tramp_ptr.add(relocated_instructions.len()),
        16
    );

    extern "C" { fn sys_icache_invalidate(start: *mut libc::c_void, len: usize); }
    sys_icache_invalidate(tramp_ptr as *mut _, trampoline_size);

    protect_page(tramp_addr);

    tramp_addr
}
