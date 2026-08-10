use std::arch::global_asm;
use crate::hook::manager::get_hooks;

#[repr(C)]
#[derive(Debug)]
pub struct RegisterContext {
    pub x: [u64; 29],
    pub fp: u64,
    pub lr: u64,
    pub sp: u64,
    pub cpsr: u64,
    pub hook_id: u64,
    pub q: [u128; 32],
}

global_asm!(r#"
    .global _hook_thunk
    .align 4
_hook_thunk:
    // We arrive here from a tiny stub that loaded the Hook ID into x16.
    
    // Allocate space for Q registers first (512 bytes)
    sub sp, sp, #512
    stp q0, q1, [sp, #0]
    stp q2, q3, [sp, #32]
    stp q4, q5, [sp, #64]
    stp q6, q7, [sp, #96]
    stp q8, q9, [sp, #128]
    stp q10, q11, [sp, #160]
    stp q12, q13, [sp, #192]
    stp q14, q15, [sp, #224]
    stp q16, q17, [sp, #256]
    stp q18, q19, [sp, #288]
    stp q20, q21, [sp, #320]
    stp q22, q23, [sp, #352]
    stp q24, q25, [sp, #384]
    stp q26, q27, [sp, #416]
    stp q28, q29, [sp, #448]
    stp q30, q31, [sp, #480]

    // Allocate space for X registers + SP + CPSR + HookID (272 bytes)
    sub sp, sp, #272
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    str x28, [sp, #224]

    // Save FP and LR
    stp x29, x30, [sp, #232]

    // Save original SP
    add x0, sp, #784
    str x0, [sp, #248]

    // Save CPSR
    mrs x0, nzcv
    str x0, [sp, #256]

    // Save hook_id (which is in x16)
    str x16, [sp, #264]

    // Call Rust Dispatcher
    mov x0, sp
    bl _hook_dispatcher
    
    // Restore CPSR
    ldr x0, [sp, #256]
    msr nzcv, x0

    // Restore FP and LR
    ldp x29, x30, [sp, #232]

    // Restore X0-X28
    ldp x0, x1, [sp, #0]
    ldp x2, x3, [sp, #16]
    ldp x4, x5, [sp, #32]
    ldp x6, x7, [sp, #48]
    ldp x8, x9, [sp, #64]
    ldp x10, x11, [sp, #80]
    ldp x12, x13, [sp, #96]
    ldp x14, x15, [sp, #112]
    ldp x16, x17, [sp, #128]
    ldp x18, x19, [sp, #144]
    ldp x20, x21, [sp, #160]
    ldp x22, x23, [sp, #176]
    ldp x24, x25, [sp, #192]
    ldp x26, x27, [sp, #208]
    ldr x28, [sp, #224]

    // Deallocate X registers block
    add sp, sp, #272

    // Restore Q registers
    ldp q0, q1, [sp, #0]
    ldp q2, q3, [sp, #32]
    ldp q4, q5, [sp, #64]
    ldp q6, q7, [sp, #96]
    ldp q8, q9, [sp, #128]
    ldp q10, q11, [sp, #160]
    ldp q12, q13, [sp, #192]
    ldp q14, q15, [sp, #224]
    ldp q16, q17, [sp, #256]
    ldp q18, q19, [sp, #288]
    ldp q20, q21, [sp, #320]
    ldp q22, q23, [sp, #352]
    ldp q24, q25, [sp, #384]
    ldp q26, q27, [sp, #416]
    ldp q28, q29, [sp, #448]
    ldp q30, q31, [sp, #480]

    // Deallocate Q registers block
    add sp, sp, #512
    
    // Jump to the trampoline address which was stored in x16 by the dispatcher
    br x16
"#);

extern "C" {
    pub fn hook_thunk();
}

#[no_mangle]
pub extern "C" fn hook_dispatcher(ctx: &mut RegisterContext) {
    let hooks = get_hooks().lock().unwrap();
    if let Some(hook_info) = hooks.get(&(ctx.hook_id as usize)) {
        if let Some(handler) = hook_info.handler {
            handler(ctx);
        }
        // Tell the thunk where to jump when it returns
        ctx.x[16] = hook_info.trampoline_addr as u64;
    }
}
