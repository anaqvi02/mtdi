use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::overwrite_with_jump;
use super::trampoline::builder::build_trampoline;
use super::trampoline::thunk::{hook_thunk, RegisterContext};
use super::trampoline::allocator::allocate_trampoline;
use crate::hook::{unprotect_page, protect_page};

pub static HOOKS: OnceLock<Mutex<HashMap<usize, HookInfo>>> = OnceLock::new();

pub struct HookInfo {
    pub name: String,
    pub original_addr: usize,
    pub trampoline_addr: usize,
    pub handler: Option<fn(&mut RegisterContext)>,
}

pub enum HookType {
    FastPath(usize),
    FullContext(fn(&mut RegisterContext)),
}

pub fn get_hooks() -> &'static Mutex<HashMap<usize, HookInfo>> {
    HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn install_hook(name: &str, target_addr: usize, hook_type: HookType) -> Result<usize, String> {
    let mut hooks = get_hooks().lock().unwrap();
    
    if hooks.values().any(|h| h.original_addr == target_addr) {
        return Err(format!("Hook already installed at {:#x}", target_addr));
    }

    let mut stolen_bytes = [0u8; 16];
    unsafe { std::ptr::copy_nonoverlapping(target_addr as *const u8, stolen_bytes.as_mut_ptr(), 16); }
    let trampoline_addr = unsafe { build_trampoline(target_addr, &stolen_bytes) };

    let (jump_target, handler_func) = match hook_type {
        HookType::FastPath(handler_addr) => (handler_addr, None),
        HookType::FullContext(func) => {
            let stub_addr = allocate_trampoline(32);
            let stub_ptr = stub_addr as *mut u8;
            let hook_id = trampoline_addr as u64; 
            let thunk_addr = hook_thunk as usize as u64;

            let mut stub = [0u8; 32];
            stub[0..4].copy_from_slice(&0x58000070u32.to_le_bytes()); // LDR X16, #12
            stub[4..8].copy_from_slice(&0x58000091u32.to_le_bytes()); // LDR X17, #16
            stub[8..12].copy_from_slice(&0xD61F0220u32.to_le_bytes()); // BR X17
            stub[12..20].copy_from_slice(&hook_id.to_le_bytes());
            stub[20..28].copy_from_slice(&thunk_addr.to_le_bytes());

            unsafe {
                unprotect_page(stub_addr);
                std::ptr::copy_nonoverlapping(stub.as_ptr(), stub_ptr, 32);
                extern "C" { fn sys_icache_invalidate(start: *mut libc::c_void, len: usize); }
                sys_icache_invalidate(stub_ptr as *mut _, 32);
                protect_page(stub_addr);
            }
            (stub_addr, Some(func))
        }
    };

    match unsafe { overwrite_with_jump(target_addr, jump_target) } {
        Ok(_) => {}
        Err(e) => return Err(e),
    }

    hooks.insert(trampoline_addr, HookInfo {
        name: name.to_string(),
        original_addr: target_addr,
        trampoline_addr,
        handler: handler_func,
    });

    Ok(trampoline_addr)
}
