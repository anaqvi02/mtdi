// swap dylib template: MTDI_SWAP_DYLIB routes open() here
// note: engine only hooks open, so only on_open is defined
// build: rustc --edition=2021 --crate-type cdylib -O probes/swap.rs -o swap.dylib
// run: MTDI_SWAP_DYLIB=$PWD/swap.dylib mtdi ./your_binary

#![allow(clashing_extern_declarations)]

use std::os::raw::{c_char, c_int};

extern "C" {
    // raw syscall forwarding bypasses libc (and our own hook),
    // so no recursion into on_open
    #[link_name = "syscall"]
    fn syscall(number: c_int, ...) -> c_int;
}

// macOS syscall numbers (arm64)
const SYS_OPEN: c_int = 5;

#[no_mangle]
pub unsafe extern "C" fn on_open(path: *const c_char, oflag: c_int, mode: c_int) -> c_int {
    // todo: sandbox/mutate here (EACCES, rewrite path, spoof result)
    syscall(SYS_OPEN, path, oflag, mode)
}
