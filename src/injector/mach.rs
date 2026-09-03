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

/// emits movz/movk for a 64-bit absolute address
/// (shared-cache addresses are valid in the target)
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
    
    // 1mb for stack + payload
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

    // 1. write dylib path at the bottom
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

    // 2. resolve functions
    let dlopen_ptr = libc::dlopen as *const () as u64;
    let pthread_exit_ptr = libc::pthread_exit as *const () as u64;
    // pthread_create_from_mach_thread converts the raw mach thread into a
    // real pthread; dyld4's dlopen reads pthread TSD (clearErrorString),
    // and a raw mach thread has a null TSD base
    let pthread_create_from_mach_ptr = unsafe {
        libc::dlsym(libc::RTLD_DEFAULT, c"pthread_create_from_mach_thread".as_ptr())
    } as u64;
    let dlerror_ptr = unsafe {
        libc::dlsym(libc::RTLD_DEFAULT, c"dlerror".as_ptr())
    } as u64;

    // 3. thread state
    let mut state: arm_thread_state64_t = unsafe { std::mem::zeroed() };
    
    if pthread_create_from_mach_ptr != 0 {
        // bootstrap: stub hands dlopen to a real pthread (tls valid there);
        // the raw thread never terminates (pthread_exit crashes with null
        // tpidrro_el0), so it parks on nanosleep after calling
        let slot_addr = remote_address + SLOT_OFF;
        let path_addr = remote_address;
        let start_addr = remote_address + STUB_OFF + 104; // 18 entry + 8 park insns
        let marker_addr = remote_address + 0x100; // diagnostic 'R'/'D' bytes
        let timespec_addr = remote_address + 0x110; // {1, 0} sec/nsec
        let ack_addr = remote_address + 0x160; // dlopen result (load-ack)
        let err_addr = remote_address + 0x170; // dlerror() string pointer

        let mut stub: Vec<u32> = Vec::with_capacity(64);
        // entry: pthread_create_from_mach_thread(&slot, null, start, path)
        stub.extend_from_slice(&mov_addr(0, slot_addr)); // x0 = &slot
        stub.push(0xD2800001); // movz x1, #0 (attr NULL)
        stub.extend_from_slice(&mov_addr(2, start_addr)); // x2 = start_routine
        stub.extend_from_slice(&mov_addr(3, path_addr)); // x3 = dylib path
        stub.extend_from_slice(&mov_addr(16, pthread_create_from_mach_ptr));
        stub.push(0xD63F0200); // blr x16
        // park; never terminate the raw thread
        stub.extend_from_slice(&mov_addr(0, timespec_addr));
        stub.push(0xD2800001); // mov x1, #0 (rem = NULL)
        stub.push(0xD2800450); // movz x16, #34 (SYS_nanosleep)
        stub.push(0xD4000001); // svc #0x80
        stub.push(0x17FFFFF9); // b park-start (reload, re-sleep)

        // start(path=x0): marker='r'; dlopen(path, RTLD_NOW); marker+1='d'; park
        stub.push(0xAA0003F3); // mov x19, x0 (save path arg)
        stub.extend_from_slice(&mov_addr(1, marker_addr)); // &marker
        stub.push(0xD2800A40); // movz w0, #0x52 ('R')
        stub.push(0x39000020); // strb w0, [x1]

        stub.push(0xAA1303E0); // mov x0, x19 (restore path)
        stub.push(0xD2800041); // mov x1, #2 (RTLD_NOW)
        stub.extend_from_slice(&mov_addr(16, dlopen_ptr));
        stub.push(0xD63F0200); // blr x16

        // load-ack: ack_addr = dlopen's return (non-null = loaded)
        stub.extend_from_slice(&mov_addr(1, ack_addr));
        stub.push(0xF9000020); // str x0, [x1]
        // on failure capture dlerror() for the cli
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
        // diag: 'r'/'d' markers, park timespec {1,0}
        let diag: [u8; 24] = {
            let mut b = [0u8; 24];
            // markers start zeroed: the stub sets 'R'/'D' as it runs,
            // so the readback can tell "never started" from "dlopen failed"
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
        // stub page starts rw; make it rx or the first fetch faults (w^x)
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
        // fallback: raw thread straight into dlopen (pre-dyld4)
        state.__x[0] = remote_address as u64; // dylib path
        state.__x[1] = RTLD_NOW;
        state.__pc = dlopen_ptr;
        state.__lr = pthread_exit_ptr;
    }
    
    // sp = top of allocation, 16-byte aligned
    state.__sp = (remote_address + allocation_size - 16) & !0xF;

    // 4. create + start thread
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

    // wait for load-ack so injection regressions fail loudly, not silently
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
            // read markers: r = start ran, d = dlopen returned
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
            // dlopen failed: read dlerror ptr at +0x170, then the message
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
