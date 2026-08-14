use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use syn::visit::Visit;
use syn::BinOp;

struct SafetyVerifier {
    pub errors: Vec<String>,
}

impl<'ast> Visit<'ast> for SafetyVerifier {
    fn visit_expr_index(&mut self, node: &'ast syn::ExprIndex) {
        self.errors.push("AST Verifier: Raw array indexing '[]' is forbidden. Use '.get_safe()' to guarantee Zero-Panic bounds clamping.".to_string());
        syn::visit::visit_expr_index(self, node);
    }
    
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method_name = node.method.to_string();
        if method_name == "unwrap" || method_name == "expect" {
            self.errors.push(format!("AST Verifier: Method '.{}()' is forbidden because it can panic. Use '.unwrap_or()' instead.", method_name));
        }
        syn::visit::visit_expr_method_call(self, node);
    }
    
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if let Some(segment) = node.path.segments.last() {
            let name = segment.ident.to_string();
            if ["panic", "assert", "assert_eq", "assert_ne", "todo", "unimplemented", "unreachable"].contains(&name.as_str()) {
                self.errors.push(format!("AST Verifier: Macro '{}!' is forbidden because it triggers an unconditional panic.", name));
            }
        }
        syn::visit::visit_macro(self, node);
    }
    
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(expr_path) = &*node.func {
            if let Some(segment) = expr_path.path.segments.last() {
                let func_name = segment.ident.to_string();
                if ["unwrap", "expect", "panic_any", "abort", "exit", "unreachable_unchecked"].contains(&func_name.as_str()) {
                    self.errors.push(format!("AST Verifier: Function call to '{}' is forbidden because it can panic or kill the process.", func_name));
                }
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        // rustc emits the divide-by-zero check even with -C overflow-checks=off,
        // so raw `/` and `%` can panic at runtime. The Zero-Panic API provides
        // SafeU64::checked_div/checked_rem for division instead.
        if let BinOp::Div(_) | BinOp::Rem(_) = &node.op {
            self.errors.push(
                "AST Verifier: Raw division '/' and modulo '%' are forbidden because dividing by zero panics. Use SafeU64's checked_div()/checked_rem() instead.".to_string(),
            );
        }
        syn::visit::visit_expr_binary(self, node);
    }
}
pub fn compile_script(script_path: &Path, legacy_unwind: bool) -> Result<PathBuf, String> {
    if !script_path.exists() {
        return Err(format!("Script file not found: {}", script_path.display()));
    }

    let user_code = fs::read_to_string(script_path)
        .map_err(|e| format!("Failed to read script file: {}", e))?;

    // 1. Security & AST verification: Full AST pass to ban unsafe/panicking syntax
    if !legacy_unwind {
        let ast = syn::parse_file(&user_code)
            .map_err(|e| format!("[mtdis] AST Parse Error: {}", e))?;
        let mut verifier = SafetyVerifier { errors: Vec::new() };
        verifier.visit_file(&ast);

        if !verifier.errors.is_empty() {
            let mut msg = String::from("[mtdis] Probe Verification Failed:\n");
            for err in verifier.errors {
                msg.push_str(&format!("- {}\n", err));
            }
            return Err(msg);
        }
    }

    // 2. Compute unique hash for this script
    let mut hasher = DefaultHasher::new();
    user_code.hash(&mut hasher);
    let code_hash = hasher.finish();

    let wrapper_src_path = PathBuf::from(format!("/tmp/mtdis_wrap_{:x}.rs", code_hash));
    let out_dylib_path = PathBuf::from(format!("/tmp/mtdis_lib_{:x}.dylib", code_hash));

    // 3. Generate self-contained harness with Zero-Panic API
    let full_source = generate_harness(&user_code, legacy_unwind);
    fs::write(&wrapper_src_path, full_source)
        .map_err(|e| format!("Failed to write wrapper source: {}", e))?;

    // 4. Invoke rustc to compile the dylib
    let mut cmd = Command::new("rustc");
    cmd.arg("--edition=2021")
        .arg("--crate-type")
        .arg("cdylib")
        .arg("-O");

    if !legacy_unwind {
        // -C overflow-checks=off : Hardware executes math natively (wrapping silently like ARM64 C code).
        // -C panic=abort         : Strip all DWARF unwinding branches for 100% straight-line performance.
        cmd.arg("-C").arg("overflow-checks=off")
           .arg("-C").arg("panic=abort");
    }

    cmd.arg(&wrapper_src_path)
        .arg("-o")
        .arg(&out_dylib_path);

    let status = cmd.output()
        .map_err(|e| format!("Failed to execute rustc: {}", e))?;

    if !status.status.success() {
        let err_msg = String::from_utf8_lossy(&status.stderr);
        return Err(format!("[mtdis] Compilation failed:\n{}", err_msg));
    }

    Ok(out_dylib_path)
}

fn generate_harness(user_code: &str, legacy_unwind: bool) -> String {
    let dispatch_logic = if legacy_unwind {
        r###"
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler(&mut safe_ctx);
        }));
        "###
    } else {
        r###"
        handler(&mut safe_ctx);
        "###
    };

    format!(r###"// Auto-generated by mtdis
use std::arch::global_asm;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{{Mutex, OnceLock}};

// Engine Internals

#[repr(C)]
#[derive(Debug)]
pub struct RegisterContext {{
    pub x: [u64; 29],
    pub fp: u64,
    pub lr: u64,
    pub sp: u64,
    pub cpsr: u64,
    pub hook_id: u64,
    pub q: [u128; 32],
}}

global_asm!(r#"
    .global _hook_thunk
    .align 4
_hook_thunk:
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

    stp x29, x30, [sp, #232]
    add x0, sp, #784
    str x0, [sp, #248]
    mrs x0, nzcv
    str x0, [sp, #256]
    str x16, [sp, #264]

    mov x0, sp
    bl _hook_dispatcher

    ldr x0, [sp, #256]
    msr nzcv, x0
    ldp x29, x30, [sp, #232]

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

    add sp, sp, #272

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

    add sp, sp, #512
    br x16
"#);

extern "C" {{
    fn mach_task_self() -> u32;
    fn mach_vm_protect(target_task: u32, address: u64, size: u64, set_maximum: i32, new_protection: i32) -> i32;
    fn mach_vm_allocate(target_task: u32, address: *mut u64, size: u64, flags: i32) -> i32;
    fn mach_vm_read_overwrite(target_task: u32, address: u64, size: u64, data: u64, outsize: *mut u64) -> i32;
    fn sys_icache_invalidate(start: *mut std::ffi::c_void, len: usize);
    fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}}

const RTLD_DEFAULT: *mut std::ffi::c_void = -2isize as *mut std::ffi::c_void;
const VM_FLAGS_ANYWHERE: i32 = 0x0001;
const VM_PROT_READ: i32 = 0x01;
const VM_PROT_WRITE: i32 = 0x02;
const VM_PROT_EXECUTE: i32 = 0x04;
const VM_PROT_COPY: i32 = 0x10;

unsafe fn unprotect_page(addr: usize) -> i32 {{
    let page_addr = (addr & !0x3FFF) as u64;
    mach_vm_protect(mach_task_self(), page_addr, 0x4000, 0, VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY)
}}

unsafe fn protect_page(addr: usize) {{
    let page_addr = (addr & !0x3FFF) as u64;
    mach_vm_protect(mach_task_self(), page_addr, 0x4000, 0, VM_PROT_READ | VM_PROT_EXECUTE);
}}

unsafe fn allocate_near(target_addr: usize, size: usize) -> usize {{
    let page_size = 0x4000u64;
    let base = (target_addr & !0x3FFF) as i64;
    let offsets = [
        0x10000i64, -0x10000i64, 0x20000, -0x20000, 0x40000, -0x40000,
        0x80000, -0x80000, 0x100000, -0x100000, 0x400000, -0x400000,
        0x1000000, -0x1000000
    ];
    for offset in offsets {{
        let mut candidate = (base + offset) as u64;
        let kr = mach_vm_allocate(mach_task_self(), &mut candidate, page_size, 0);
        if kr == 0 {{
            mach_vm_protect(mach_task_self(), candidate, page_size, 0, VM_PROT_READ | VM_PROT_WRITE | VM_PROT_EXECUTE);
            return candidate as usize;
        }}
    }}
    let mut address: u64 = 0;
    mach_vm_allocate(mach_task_self(), &mut address, size as u64, VM_FLAGS_ANYWHERE);
    mach_vm_protect(mach_task_self(), address, size as u64, 0, VM_PROT_READ | VM_PROT_WRITE | VM_PROT_EXECUTE);
    address as usize
}}

struct HookInfo {{
    trampoline_addr: usize,
    handler: fn(&mut MtdiSafeContext),
}}

static HOOKS: OnceLock<Mutex<HashMap<usize, HookInfo>>> = OnceLock::new();

fn get_hooks() -> &'static Mutex<HashMap<usize, HookInfo>> {{
    HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
}}

thread_local! {{
    static HOOK_DEPTH: Cell<usize> = const {{ Cell::new(0) }};
}}

#[no_mangle]
pub extern "C" fn hook_dispatcher(ctx: &mut RegisterContext) {{
    let (trampoline_addr, handler) = {{
        let hooks = get_hooks().lock().unwrap();
        if let Some(hook_info) = hooks.get(&(ctx.hook_id as usize)) {{
            (hook_info.trampoline_addr, hook_info.handler)
        }} else {{
            return;
        }}
    }};

    ctx.x[16] = trampoline_addr as u64;

    HOOK_DEPTH.with(|depth| {{
        if depth.get() > 0 {{
            return; // Prevent recursive hook storm
        }}
        depth.set(1);
        let mut safe_ctx = MtdiSafeContext {{ raw: ctx }};

        {dispatch_logic}

        depth.set(0);
    }});
}}

fn can_branch_26(from: usize, to: usize) -> bool {{
    let diff = (to as i64) - (from as i64);
    diff >= -134217728 && diff <= 134217724
}}

fn make_branch_26(from: usize, to: usize) -> u32 {{
    let diff = (to as i64) - (from as i64);
    let imm26 = ((diff >> 2) as u32) & 0x03FFFFFF;
    0x14000000 | imm26
}}

fn is_pc_relative(insn: u32) -> bool {{
    if (insn & 0x1F000000) == 0x10000000 {{ return true; }}
    if (insn >> 26) == 0b000101 || (insn >> 26) == 0b100101 {{ return true; }}
    if (insn >> 24) == 0b01010100 {{ return true; }}
    if ((insn >> 24) & 0b01111111) == 0b00110100 {{ return true; }}
    if ((insn >> 24) & 0b01111111) == 0b00110110 {{ return true; }}
    if (insn & 0x3B000000) == 0x18000000 {{ return true; }}
    false
}}

fn relocate_instruction(instruction: u32, original_pc: usize, out_buffer: &mut Vec<u8>) {{
    if !is_pc_relative(instruction) {{
        out_buffer.extend_from_slice(&instruction.to_le_bytes());
        return;
    }}

    // PC-Relative relocations...
    if (instruction & 0x9F000000) == 0x10000000 || (instruction & 0x9F000000) == 0x90000000 {{
        let is_adrp = (instruction & 0x80000000) != 0;
        let rd = instruction & 0x1F;
        let immlo = (instruction >> 29) & 3;
        let immhi = (instruction >> 5) & 0x7FFFF;
        let mut imm = (immhi << 2) | immlo;
        if (imm & 0x100000) != 0 {{ imm |= 0xFFE00000; }}
        let imm = imm as i32 as i64;
        let target = if is_adrp {{ ((original_pc as u64 & !0xFFF) as i64 + (imm << 12)) as u64 }} else {{ (original_pc as i64 + imm) as u64 }};
        out_buffer.extend_from_slice(&(0x58000040 | rd).to_le_bytes());
        out_buffer.extend_from_slice(&0x14000003u32.to_le_bytes());
        out_buffer.extend_from_slice(&target.to_le_bytes());
        return;
    }}
    if (instruction >> 26) == 0b000101 || (instruction >> 26) == 0b100101 {{
        let is_bl = (instruction & 0x80000000) != 0;
        let mut imm = instruction & 0x03FFFFFF;
        if (imm & 0x02000000) != 0 {{ imm |= 0xFC000000; }}
        let target = (original_pc as i64 + (imm as i32 as i64 * 4)) as u64;
        if !is_bl {{
            out_buffer.extend_from_slice(&0x58000050u32.to_le_bytes());
            out_buffer.extend_from_slice(&0xD61F0200u32.to_le_bytes());
            out_buffer.extend_from_slice(&target.to_le_bytes());
        }} else {{
            out_buffer.extend_from_slice(&0x5800007Eu32.to_le_bytes());
            out_buffer.extend_from_slice(&0x58000090u32.to_le_bytes());
            out_buffer.extend_from_slice(&0xD61F0200u32.to_le_bytes());
            out_buffer.extend_from_slice(&(original_pc as u64 + 4).to_le_bytes());
            out_buffer.extend_from_slice(&target.to_le_bytes());
        }}
        return;
    }}
    if (instruction >> 24) == 0b01010100 {{
        let mut imm = (instruction >> 5) & 0x7FFFF;
        if (imm & 0x40000) != 0 {{ imm |= 0xFFF80000; }}
        let target = (original_pc as i64 + (imm as i32 as i64 * 4)) as u64;
        let b_inv_cond = 0x54000000 | (5 << 5) | ((instruction & 0xF) ^ 1);
        out_buffer.extend_from_slice(&b_inv_cond.to_le_bytes());
        out_buffer.extend_from_slice(&0x58000050u32.to_le_bytes());
        out_buffer.extend_from_slice(&0xD61F0200u32.to_le_bytes());
        out_buffer.extend_from_slice(&target.to_le_bytes());
        return;
    }}
    if ((instruction >> 24) & 0b01111111) == 0b00110100 {{
        let mut imm = (instruction >> 5) & 0x7FFFF;
        if (imm & 0x40000) != 0 {{ imm |= 0xFFF80000; }}
        let target = (original_pc as i64 + (imm as i32 as i64 * 4)) as u64;
        let inv_inst = ((instruction ^ (1 << 24)) & !(0x7FFFF << 5)) | (5 << 5);
        out_buffer.extend_from_slice(&inv_inst.to_le_bytes());
        out_buffer.extend_from_slice(&0x58000050u32.to_le_bytes());
        out_buffer.extend_from_slice(&0xD61F0200u32.to_le_bytes());
        out_buffer.extend_from_slice(&target.to_le_bytes());
        return;
    }}
    if ((instruction >> 24) & 0b01111111) == 0b00110110 {{
        let mut imm = (instruction >> 5) & 0x3FFF;
        if (imm & 0x2000) != 0 {{ imm |= 0xFFFFC000; }}
        let target = (original_pc as i64 + (imm as i32 as i64 * 4)) as u64;
        let inv_inst = ((instruction ^ (1 << 24)) & !(0x3FFF << 5)) | (5 << 5);
        out_buffer.extend_from_slice(&inv_inst.to_le_bytes());
        out_buffer.extend_from_slice(&0x58000050u32.to_le_bytes());
        out_buffer.extend_from_slice(&0xD61F0200u32.to_le_bytes());
        out_buffer.extend_from_slice(&target.to_le_bytes());
        return;
    }}
    if (instruction & 0x3B000000) == 0x18000000 {{
        let mut imm = (instruction >> 5) & 0x7FFFF;
        if (imm & 0x40000) != 0 {{ imm |= 0xFFF80000; }}
        let target = (original_pc as i64 + (imm as i32 as i64 * 4)) as u64;
        let value = unsafe {{ *(target as *const u64) }};
        out_buffer.extend_from_slice(&((instruction & 0xFF00001F) | (2 << 5)).to_le_bytes());
        out_buffer.extend_from_slice(&0x14000003u32.to_le_bytes());
        out_buffer.extend_from_slice(&value.to_le_bytes());
        return;
    }}
    out_buffer.extend_from_slice(&instruction.to_le_bytes());
}}

unsafe fn build_trampoline(target_addr: usize, stolen_len: usize) -> usize {{
    let num_insns = stolen_len / 4;
    let mut relocated = Vec::with_capacity(64);
    let mut has_ret = false;
    for i in 0..num_insns {{
        let insn = *((target_addr + i * 4) as *const u32);
        if insn == 0xD65F03C0 {{ has_ret = true; }}
        relocate_instruction(insn, target_addr + i * 4, &mut relocated);
    }}
    if !has_ret {{
        let mut branch_payload = [0u8; 16];
        branch_payload[0..4].copy_from_slice(&0x58000050u32.to_le_bytes());
        branch_payload[4..8].copy_from_slice(&0xD61F0200u32.to_le_bytes());
        branch_payload[8..16].copy_from_slice(&((target_addr + stolen_len) as u64).to_le_bytes());
        relocated.extend_from_slice(&branch_payload);
    }}
    let tramp_addr = allocate_near(target_addr, relocated.len());
    unprotect_page(tramp_addr);
    std::ptr::copy_nonoverlapping(relocated.as_ptr(), tramp_addr as *mut u8, relocated.len());
    sys_icache_invalidate(tramp_addr as *mut _, relocated.len());
    protect_page(tramp_addr);
    tramp_addr
}}

extern "C" {{
    fn hook_thunk();
}}

fn raw_install_hook(target_addr: usize, handler: fn(&mut MtdiSafeContext)) -> Result<usize, String> {{
    unsafe {{
        let stub_addr = allocate_near(target_addr, 32);
        let is_near = can_branch_26(target_addr, stub_addr);
        let stolen_len = if is_near {{ 4 }} else {{ 16 }};
        let trampoline_addr = build_trampoline(target_addr, stolen_len);

        let mut stub = [0u8; 32];
        stub[0..4].copy_from_slice(&0x58000070u32.to_le_bytes());
        stub[4..8].copy_from_slice(&0x58000091u32.to_le_bytes());
        stub[8..12].copy_from_slice(&0xD61F0220u32.to_le_bytes());
        stub[12..20].copy_from_slice(&(trampoline_addr as u64).to_le_bytes());
        stub[20..28].copy_from_slice(&(hook_thunk as usize as u64).to_le_bytes());

        unprotect_page(stub_addr);
        std::ptr::copy_nonoverlapping(stub.as_ptr(), stub_addr as *mut u8, 32);
        sys_icache_invalidate(stub_addr as *mut _, 32);
        protect_page(stub_addr);

        let kr = unprotect_page(target_addr);
        if kr != 0 {{
            return Err(format!("cannot unprotect page at {{:#x}} (PPL-protected shared-cache page?)", target_addr));
        }}
        if is_near {{
            std::ptr::copy_nonoverlapping(make_branch_26(target_addr, stub_addr).to_le_bytes().as_ptr(), target_addr as *mut u8, 4);
            sys_icache_invalidate(target_addr as *mut _, 4);
        }} else {{
            let mut jump_insns = [0u8; 16];
            jump_insns[0..4].copy_from_slice(&0x58000051u32.to_le_bytes());
            jump_insns[4..8].copy_from_slice(&0xD61F0220u32.to_le_bytes());
            jump_insns[8..16].copy_from_slice(&(stub_addr as u64).to_le_bytes());
            std::ptr::copy_nonoverlapping(jump_insns.as_ptr(), target_addr as *mut u8, 16);
            sys_icache_invalidate(target_addr as *mut _, 16);
        }}
        protect_page(target_addr);

        get_hooks().lock().unwrap().insert(trampoline_addr, HookInfo {{ trampoline_addr, handler }});
        Ok(trampoline_addr)
    }}
}}

// Safe Wrapper API

pub struct MtdiSafeContext<'a> {{
    raw: &'a mut RegisterContext,
}}

impl<'a> MtdiSafeContext<'a> {{
    pub fn arg(&self, index: usize) -> u64 {{
        if index < 8 {{ self.raw.x[index] }} else {{ 0 }}
    }}

    pub fn set_arg(&mut self, index: usize, val: u64) {{
        if index < 8 {{ self.raw.x[index] = val; }}
    }}

    pub fn return_val(&self) -> u64 {{
        self.raw.x[0]
    }}

    pub fn set_return_val(&mut self, val: u64) {{
        self.raw.x[0] = val;
    }}

    pub fn read_arg_str(&self, index: usize, max_len: usize) -> Option<String> {{
        let ptr = self.arg(index);
        if ptr == 0 {{ return None; }}
        let mut buffer = vec![0u8; max_len];
        let mut bytes_read: u64 = 0;
        let kr = unsafe {{ mach_vm_read_overwrite(mach_task_self(), ptr, max_len as u64, buffer.as_mut_ptr() as u64, &mut bytes_read) }};
        if kr != 0 || bytes_read == 0 {{ return None; }}
        let null_pos = buffer[..bytes_read as usize].iter().position(|&b| b == 0).unwrap_or(bytes_read as usize);
        String::from_utf8(buffer[..null_pos].to_vec()).ok()
    }}
}}

pub struct MtdiRegistry {{
    pub hooks: Vec<(&'static str, fn(&mut MtdiSafeContext))>,
}}

impl MtdiRegistry {{
    pub fn hook_symbol(&mut self, symbol: &'static str, handler: fn(&mut MtdiSafeContext)) {{
        self.hooks.push((symbol, handler));
    }}
}}

// Zero-Panic Math

mod user_sandbox {{
    #![forbid(unsafe_code)]
    use super::{{MtdiSafeContext, MtdiRegistry}};

    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
    pub struct SafeU64(pub u64);

    impl std::ops::Add for SafeU64 {{
        type Output = SafeU64;
        #[inline(always)]
        fn add(self, rhs: SafeU64) -> SafeU64 {{ SafeU64(self.0.wrapping_add(rhs.0)) }}
    }}

    impl std::ops::Sub for SafeU64 {{
        type Output = SafeU64;
        #[inline(always)]
        fn sub(self, rhs: SafeU64) -> SafeU64 {{ SafeU64(self.0.wrapping_sub(rhs.0)) }}
    }}

    impl std::ops::Mul for SafeU64 {{
        type Output = SafeU64;
        #[inline(always)]
        fn mul(self, rhs: SafeU64) -> SafeU64 {{ SafeU64(self.0.wrapping_mul(rhs.0)) }}
    }}

    impl SafeU64 {{
        #[inline(always)]
        pub fn checked_div(self, rhs: SafeU64) -> SafeU64 {{
            SafeU64(self.0.checked_div(rhs.0).unwrap_or(0))
        }}
        #[inline(always)]
        pub fn checked_rem(self, rhs: SafeU64) -> SafeU64 {{
            SafeU64(self.0.checked_rem(rhs.0).unwrap_or(0))
        }}
    }}

    pub trait SafeSliceExt<T> {{
        fn get_safe(&self, index: usize) -> T;
    }}

    impl<T: Copy + Default> SafeSliceExt<T> for &[T] {{
        #[inline(always)]
        fn get_safe(&self, index: usize) -> T {{
            if index < self.len() {{ self[index] }} else {{ T::default() }}
        }}
    }}

    impl<T: Copy + Default> SafeSliceExt<T> for [T] {{
        #[inline(always)]
        fn get_safe(&self, index: usize) -> T {{
            if index < self.len() {{ self[index] }} else {{ T::default() }}
        }}
    }}

    impl<T: Copy + Default, const N: usize> SafeSliceExt<T> for [T; N] {{
        #[inline(always)]
        fn get_safe(&self, index: usize) -> T {{
            if index < N {{ self[index] }} else {{ T::default() }}
        }}
    }}

    // INJECT USER CODE:
    {user_code}
}}

// ---------------------------------------------------------
// 4. MODULE INITIALIZER
// ---------------------------------------------------------

#[used]
#[link_section = "__DATA,__mod_init_func"]
static INIT: extern "C" fn() = mtdis_init;

extern "C" fn mtdis_init() {{
    let mut reg = MtdiRegistry {{ hooks: Vec::new() }};
    
    // Direct branchless registration (Total Functions don't panic)
    user_sandbox::register(&mut reg);

    for (symbol_name, handler) in reg.hooks {{
        let c_sym = match std::ffi::CString::new(symbol_name) {{
            Ok(s) => s,
            Err(_) => continue,
        }};
        let mut sym_addr = unsafe {{ dlsym(RTLD_DEFAULT, c_sym.as_ptr()) as usize }};
        if sym_addr == 0 {{
            if let Ok(c_under) = std::ffi::CString::new(format!("_{{}}", symbol_name)) {{
                sym_addr = unsafe {{ dlsym(RTLD_DEFAULT, c_under.as_ptr()) as usize }};
            }}
        }}
        if sym_addr == 0 {{
            eprintln!("[mtdis] Warning: Symbol '{{}}' not found in process.", symbol_name);
            continue;
        }}
        if let Err(e) = raw_install_hook(sym_addr, handler) {{
            eprintln!("[mtdis] Failed to hook {{}}: {{}}", symbol_name, e);
        }}
    }}
}}
"###)
}

#[cfg(test)]
mod tests {
    use super::SafetyVerifier;
    use syn::visit::Visit;

    /// Run the AST verifier over `code` and return the violation messages.
    fn verify(code: &str) -> Vec<String> {
        let ast = syn::parse_file(code).expect("test probe must parse");
        let mut verifier = SafetyVerifier { errors: Vec::new() };
        verifier.visit_file(&ast);
        verifier.errors
    }

    #[test]
    fn accepts_clean_probe() {
        let errs = verify(
            r#"
            pub fn on_open(ctx: &mut MtdiSafeContext) {
                if let Some(path) = ctx.read_arg_str(0, 256) {
                    println!("open: {}", path);
                }
            }
            pub fn register(reg: &mut MtdiRegistry) {
                reg.hook_symbol("open", on_open);
            }
            "#,
        );
        assert!(errs.is_empty(), "clean probe rejected: {errs:?}");
    }

    #[test]
    fn rejects_unwrap_and_expect() {
        let errs = verify(
            r#"
            pub fn on_open(ctx: &mut MtdiSafeContext) {
                let x = ctx.read_arg_str(0, 8).unwrap();
                let y = Some(1u64).expect("boom");
                let _ = (x, y);
            }
            "#,
        );
        assert_eq!(errs.len(), 2, "expected unwrap+expect violations: {errs:?}");
        assert!(errs.iter().all(|e| e.contains("forbidden")));
    }

    #[test]
    fn rejects_raw_indexing() {
        let errs = verify(
            r#"
            pub fn on_open(ctx: &mut MtdiSafeContext) {
                let v = [1u64, 2, 3];
                let _ = v[1];
            }
            "#,
        );
        assert_eq!(errs.len(), 1, "expected indexing violation: {errs:?}");
        assert!(errs[0].contains("get_safe"));
    }

    #[test]
    fn rejects_panicking_macros() {
        let errs = verify(
            r#"
            pub fn on_open(ctx: &mut MtdiSafeContext) {
                assert!(ctx.arg(0) != 0);
                unreachable!();
            }
            "#,
        );
        assert_eq!(errs.len(), 2, "expected assert!+unreachable! violations: {errs:?}");
    }

    #[test]
    fn rejects_raw_division_and_modulo() {
        let errs = verify(
            r#"
            pub fn on_open(ctx: &mut MtdiSafeContext) {
                let a = ctx.arg(0);
                let _ = a / 2;
                let _ = a % 2;
            }
            "#,
        );
        assert_eq!(errs.len(), 2, "expected div+rem violations: {errs:?}");
        assert!(
            errs.iter()
                .all(|e| e.contains("checked_div") || e.contains("checked_rem"))
        );
    }

    #[test]
    fn rejects_process_killing_calls() {
        let errs = verify(
            r#"
            pub fn on_open(ctx: &mut MtdiSafeContext) {
                std::process::abort();
            }
            "#,
        );
        assert_eq!(errs.len(), 1, "expected abort() violation: {errs:?}");
    }
}
