use mach2::vm::{mach_vm_allocate};
use mach2::vm_types::mach_vm_address_t;
use mach2::mach_types::task_name_t;
use mach2::traps::mach_task_self;

use std::sync::atomic::{AtomicUsize, Ordering};

static TRAMPOLINE_BASE: AtomicUsize = AtomicUsize::new(0);
static TRAMPOLINE_OFFSET: AtomicUsize = AtomicUsize::new(0);
const PAGE_SIZE: usize = 16384; // 16KB on Apple Silicon

pub fn allocate_trampoline(size: usize) -> usize {
    let mut base = TRAMPOLINE_BASE.load(Ordering::Acquire);
    
    if base == 0 {
        let mut addr: mach_vm_address_t = 0;
        unsafe {
            mach_vm_allocate(mach_task_self() as task_name_t, &mut addr, PAGE_SIZE as u64, 1);
        }
        
        match TRAMPOLINE_BASE.compare_exchange(0, addr as usize, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => {
                base = addr as usize;
            },
            Err(existing_base) => {
                base = existing_base;
            }
        }
    }
    
    let offset = TRAMPOLINE_OFFSET.fetch_add(size, Ordering::Relaxed);
    if offset + size > PAGE_SIZE {
        panic!("Out of trampoline memory!");
    }
    
    base + offset
}
