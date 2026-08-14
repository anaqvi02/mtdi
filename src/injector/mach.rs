use mach2::mach_types::task_name_t;
use mach2::vm::mach_vm_allocate;
use mach2::vm_types::mach_vm_address_t;
use mach2::vm::mach_vm_write;
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

pub fn inject_into_pid(pid: i32, dylib_path: &std::path::Path) {
    println!("[mtdi] Attaching to live PID: {}", pid);

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

    // 2. Resolve dlopen and pthread_exit
    let dlopen_ptr = libc::dlopen as *const () as u64;
    let pthread_exit_ptr = libc::pthread_exit as *const () as u64;

    // 3. Set up the thread state
    let mut state: arm_thread_state64_t = unsafe { std::mem::zeroed() };
    
    // x0 = pointer to our dylib path string (bottom of allocation)
    state.__x[0] = remote_address as u64;
    // x1 = RTLD_NOW
    state.__x[1] = 2; // RTLD_NOW is 2 on macOS

    // PC = dlopen
    state.__pc = dlopen_ptr;
    
    // LR = pthread_exit (so when dlopen finishes, the thread gracefully commits suicide)
    state.__lr = pthread_exit_ptr;
    
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

    println!("BOOM! Thread created and executing shellcode. PID {} has been hijacked.", pid);
}
