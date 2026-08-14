use mach2::mach_types::task_name_t;
use mach2::vm::mach_vm_allocate;
use mach2::vm::mach_vm_protect;
use mach2::vm::mach_vm_read_overwrite;
use mach2::vm::mach_vm_write;
use mach2::vm_prot::{VM_PROT_EXECUTE, VM_PROT_READ};
use mach2::vm_types::mach_vm_address_t;
use mach2::structs::arm_thread_state64_t;
use libc::c_int;
use mach2::traps::{mach_task_self, task_for_pid};
use mach2::port::mach_port_t;
use mach2::mach_types::{task_t, thread_act_t};

extern "C" {
    pub fn thread_create_running(
        parent_task: task_t,
        flavor: c_int,
        new_state: *mut arm_thread_state64_t,
        new_state_count: u32,
        child_act: *mut thread_act_t
    ) -> c_int;
}

const ARM_THREAD_STATE64: c_int = 6;
const ARM_THREAD_STATE64_COUNT: u32 = 68; // 272 bytes / 4

const RTLD_NOW: u64 = 2; // Darwin value (not 1 like Linux)
const STUB_OFF: u64 = 0x4000; // remote allocation: bootstrap stub (own 16K page, set RX)
const SLOT_OFF: u64 = 0x8000; // remote allocation: scratch pthread_t* slot (RW page)

/// Emits `movz reg, #imm16; movk reg, #imm16, lsl 16/32/48` for a 64-bit
/// absolute address. Shared-cache addresses and our own allocation are both
/// valid in the target, so a raw 64-bit immediate is safe.
fn mov_addr(reg: u32, addr: u64) -> [u32; 4] {
    let lo = (addr & 0xFFFF) as u32;
    let mut out = [0u32; 4];
    out[0] = 0xD2800000 | (lo << 5) | reg; // movz reg, #lo16
    out[1] = 0xF2800000 | (1 << 21) | (((addr >> 16) & 0xFFFF) as u32) << 5 | reg; // movk lsl16
    out[2] = 0xF2800000 | (2 << 21) | (((addr >> 32) & 0xFFFF) as u32) << 5 | reg; // movk lsl32
    out[3] = 0xF2800000 | (3 << 21) | (((addr >> 48) & 0xFFFF) as u32) << 5 | reg; // movk lsl48
    out
}

pub fn inject_into_pid(pid: i32, dylib_path: &std::path::Path) {
    println!("[mtdi] Attaching to live PID: {}", pid);
    eprintln!(
        "[mtdi] Note: macOS may pop a consent prompt (\"{} wants to control this process\") — it blocks until a human clicks Allow. This is not a hang.",
        std::env::current_exe().map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "mtdi".into())).unwrap_or_else(|_| "mtdi".into())
    );

    let mut target_task: mach_port_t = 0;
    
    let kr = unsafe { 
        task_for_pid(mach_task_self(), pid, &mut target_task) 
    };

    if kr != mach2::kern_return::KERN_SUCCESS {
        eprintln!("Failed to get task port! Kernel return code: {}", kr);
        eprintln!("Are you running with sudo? (Or is SIP blocking it?)");
        std::process::exit(1);
    }

    println!("Success! Acquired task port: {}", target_task);
    
    // Allocate 1MB for the thread stack + string payload
    let mut remote_address: mach_vm_address_t = 0;
    let allocation_size = 1024 * 1024; // 1MB stack

    let kr = unsafe {
        mach_vm_allocate(target_task as task_name_t, &mut remote_address, allocation_size, 1)
    };

    if kr != mach2::kern_return::KERN_SUCCESS {
        eprintln!("Failed to allocate remote memory! Kernel return code: {}", kr);
        std::process::exit(1);
    }

    println!("Successfully allocated remote memory at: {:#x}", remote_address);

    // 1. Write the dylib path into the very bottom of the allocated memory
    let mut path_bytes = dylib_path.to_string_lossy().into_owned().into_bytes();
    path_bytes.push(0); // Null terminator

    let kr = unsafe {
        mach_vm_write(
            target_task as task_name_t,
            remote_address,
            path_bytes.as_ptr() as usize,
            path_bytes.len() as u32,
        )
    };

    if kr != mach2::kern_return::KERN_SUCCESS {
        eprintln!("Failed to write dylib path into remote memory! KR: {}", kr);
        std::process::exit(1);
    }

    println!("Successfully wrote dylib path to remote memory.");

    // 2. Resolve the functions we need
    let dlopen_ptr = libc::dlopen as *const () as u64;
    let pthread_exit_ptr = libc::pthread_exit as *const () as u64;
    // Apple extension that converts the CALLING raw mach thread into a real
    // pthread before running a start routine. Without it, dyld4's dlopen
    // faults immediately: clearErrorString() reads this thread's pthread TSD
    // (pthread_getspecific), and a raw mach thread has a NULL TSD base.
    let pthread_create_from_mach_ptr = unsafe {
        libc::dlsym(libc::RTLD_DEFAULT, b"pthread_create_from_mach_thread\0".as_ptr() as *const libc::c_char)
    } as u64;
    let dlerror_ptr = unsafe {
        libc::dlsym(libc::RTLD_DEFAULT, b"dlerror\0".as_ptr() as *const libc::c_char)
    } as u64;

    // 3. Set up the thread state
    let mut state: arm_thread_state64_t = unsafe { std::mem::zeroed() };
    
    if pthread_create_from_mach_ptr != 0 {
        // Bootstrap path: PC = our stub, which hands the dlopen work to a
        // proper pthread (pthread_create_from_mach_thread, or a spawned
        // thread — either way TLS is valid there). The raw mach thread NEVER
        // terminates: pthread_exit/_exit on it crashes, because its
        // TPIDRRO_EL0 (TLS base) is NULL and libpthread reads it during
        // teardown. So after the call it parks on nanosleep instead.
        let slot_addr = remote_address + SLOT_OFF;
        let path_addr = remote_address;
        let start_addr = remote_address + STUB_OFF + 104; // 18 entry + 8 park insns
        let marker_addr = remote_address + 0x100; // diagnostic 'R'/'D' bytes
        let timespec_addr = remote_address + 0x110; // {1, 0} sec/nsec
        let ack_addr = remote_address + 0x160; // dlopen result (load-ack)
        let err_addr = remote_address + 0x170; // dlerror() string pointer

        let mut stub: Vec<u32> = Vec::with_capacity(64);
        // entry: pthread_create_from_mach_thread(&slot, NULL, start, path)
        stub.extend_from_slice(&mov_addr(0, slot_addr)); // x0 = &slot
        stub.push(0xD2800001); // movz x1, #0 (attr NULL)
        stub.extend_from_slice(&mov_addr(2, start_addr)); // x2 = start_routine
        stub.extend_from_slice(&mov_addr(3, path_addr)); // x3 = dylib path
        stub.extend_from_slice(&mov_addr(16, pthread_create_from_mach_ptr));
        stub.push(0xD63F0200); // blr x16
        // park (never terminate the raw thread)
        stub.extend_from_slice(&mov_addr(0, timespec_addr));
        stub.push(0xD2800001); // mov x1, #0 (rem = NULL)
        stub.push(0xD2800450); // movz x16, #34 (SYS_nanosleep)
        stub.push(0xD4000001); // svc #0x80
        stub.push(0x17FFFFF9); // b park-start (reload, re-sleep)

        // start_routine(arg=path in x0):
        //   *(u8*)marker      = 'R'  (plain store; CLI reads it back)
        //   dlopen(path, RTLD_NOW)   (path kept in callee-saved x19)
        //   *(u8*)marker+1    = 'D'
        //   park (never return)
        stub.push(0xAA0003F3); // mov x19, x0 (save path arg)
        stub.extend_from_slice(&mov_addr(1, marker_addr)); // &marker
        stub.push(0xD2800A40); // movz w0, #0x52 ('R')
        stub.push(0x39000020); // strb w0, [x1]

        stub.push(0xAA1303E0); // mov x0, x19 (restore path)
        stub.push(0xD2800041); // mov x1, #2 (RTLD_NOW)
        stub.extend_from_slice(&mov_addr(16, dlopen_ptr));
        stub.push(0xD63F0200); // blr x16

        // load-ack: *(u64*)ack_addr = dlopen's return (non-NULL = loaded)
        stub.extend_from_slice(&mov_addr(1, ack_addr));
        stub.push(0xF9000020); // str x0, [x1]
        // dlopen failed? capture dlerror() so the CLI can report the reason.
        stub.extend_from_slice(&mov_addr(16, dlerror_ptr));
        stub.push(0xD63F0200); // blr x16
        stub.extend_from_slice(&mov_addr(1, err_addr));
        stub.push(0xF9000020); // str x0, [x1]

        stub.extend_from_slice(&mov_addr(1, marker_addr + 1));
        stub.push(0xD2800880); // movz w0, #0x44 ('D')
        stub.push(0x39000020); // strb w0, [x1]

        stub.extend_from_slice(&mov_addr(0, timespec_addr));
        stub.push(0xD2800001); // mov x1, #0
        stub.push(0xD2800450); // movz x16, #34 (SYS_nanosleep)
        stub.push(0xD4000001); // svc #0x80
        stub.push(0x17FFFFF9); // b park-start

        let mut stub_bytes: Vec<u8> = Vec::with_capacity(stub.len() * 4);
        for insn in &stub {
            stub_bytes.extend_from_slice(&insn.to_le_bytes());
        }
        let kr = unsafe {
            mach_vm_write(
                target_task as task_name_t,
                remote_address + STUB_OFF,
                stub_bytes.as_ptr() as usize,
                stub_bytes.len() as u32,
            )
        };
        if kr != mach2::kern_return::KERN_SUCCESS {
            eprintln!("Failed to write bootstrap stub into remote memory! KR: {}", kr);
            std::process::exit(1);
        }
        // diagnostics: 'R' at marker, 'D' at marker+1, timespec {1,0}
        let diag: [u8; 24] = {
            let mut b = [0u8; 24];
            b[0] = b'R';
            b[1] = b'D';
            b[0x10] = 1; // tv_sec = 1
            b
        };
        let kr = unsafe {
            mach_vm_write(
                target_task as task_name_t,
                remote_address + 0x100,
                diag.as_ptr() as usize,
                diag.len() as u32,
            )
        };
        if kr != mach2::kern_return::KERN_SUCCESS {
            eprintln!("Failed to write diag bytes! KR: {}", kr);
            std::process::exit(1);
        }
        // The stub page starts RW from mach_vm_allocate; make it executable or
        // the very first instruction fetch faults (W^X).
        let kr = unsafe {
            mach_vm_protect(
                target_task as task_name_t,
                remote_address + STUB_OFF,
                0x4000,
                0,
                VM_PROT_READ | VM_PROT_EXECUTE,
            )
        };
        if kr != mach2::kern_return::KERN_SUCCESS {
            eprintln!("Failed to make stub page executable! KR: {}", kr);
            std::process::exit(1);
        }

        state.__pc = remote_address + STUB_OFF;
        state.__lr = pthread_exit_ptr;
    } else {
        // Legacy fallback: raw thread straight into dlopen (pre-dyld4 or if
        // the bootstrap symbol is ever absent).
        state.__x[0] = remote_address as u64; // dylib path
        state.__x[1] = RTLD_NOW;
        state.__pc = dlopen_ptr;
        state.__lr = pthread_exit_ptr;
    }
    
    // SP = top of the 1MB allocation (16-byte aligned)
    state.__sp = (remote_address + allocation_size - 16) & !0xF;

    // 4. Create and start the thread
    let mut child_thread: thread_act_t = 0;
    
    let kr = unsafe {
        thread_create_running(
            target_task as task_t,
            ARM_THREAD_STATE64,
            &mut state as *mut _,
            ARM_THREAD_STATE64_COUNT,
            &mut child_thread
        )
    };

    if kr != mach2::kern_return::KERN_SUCCESS {
        eprintln!("Failed to spawn remote thread! KR: {}", kr);
        std::process::exit(1);
    }

    // #2 load-ack: wait (bounded) for the start routine to store dlopen's
    // return value, then report whether the dylib actually loaded. Turns
    // injection regressions into detectable failures instead of theater.
    if pthread_create_from_mach_ptr != 0 {
        let mut ack: u64 = 0;
        let mut outsize: mach2::vm_types::mach_vm_size_t = 8;
        for _ in 0..50 {
            unsafe {
                mach_vm_read_overwrite(
                    target_task as task_name_t,
                    remote_address + 0x160,
                    8,
                    &mut ack as *mut u64 as u64,
                    &mut outsize,
                );
            }
            if ack != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if ack != 0 {
            println!("[mtdi] Dylib loaded into PID {} (handle {:#x})", pid, ack);
        } else {
            // Read the diagnostic markers to explain the failure mode:
            // R set = start routine ran; D set = dlopen returned.
            let mut marks = [0u8; 2];
            let mut msize: mach2::vm_types::mach_vm_size_t = 2;
            unsafe {
                mach_vm_read_overwrite(
                    target_task as task_name_t,
                    remote_address + 0x100,
                    2,
                    marks.as_mut_ptr() as u64,
                    &mut msize,
                );
            }
            let mut ack_bytes = [0u8; 8];
            let mut asize: mach2::vm_types::mach_vm_size_t = 8;
            let akr = unsafe {
                mach_vm_read_overwrite(
                    target_task as task_name_t,
                    remote_address + 0x160,
                    8,
                    ack_bytes.as_mut_ptr() as u64,
                    &mut asize,
                )
            };
            // If dlopen failed, the stub captured dlerror()'s message pointer
            // at +0x170 — read the pointer, then the message itself.
            let mut err_detail = String::new();
            let mut err_ptr: u64 = 0;
            let mut esize: mach2::vm_types::mach_vm_size_t = 8;
            unsafe {
                mach_vm_read_overwrite(
                    target_task as task_name_t,
                    remote_address + 0x170,
                    8,
                    &mut err_ptr as *mut u64 as u64,
                    &mut esize,
                );
            }
            if err_ptr != 0 {
                let mut buf = [0u8; 256];
                let mut bsize: mach2::vm_types::mach_vm_size_t = 256;
                let kr = unsafe {
                    mach_vm_read_overwrite(
                        target_task as task_name_t,
                        err_ptr,
                        256,
                        buf.as_mut_ptr() as u64,
                        &mut bsize,
                    )
                };
                if kr == mach2::kern_return::KERN_SUCCESS {
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    err_detail = format!(": {}", String::from_utf8_lossy(&buf[..end]));
                }
            }
            eprintln!(
                "[mtdi] WARNING: dylib did NOT load into PID {} (markers R={}, D={}; ack bytes {:02x?} kr={}{})",
                pid,
                marks[0] == b'R',
                marks[1] == b'D',
                &ack_bytes,
                akr,
                err_detail
            );
        }
    }
}
