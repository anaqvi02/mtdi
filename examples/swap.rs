// examples/swap.rs
// "Swap dylib" template for mtrace. Compile this to a dylib and point
// MTRACE_SWAP_DYLIB at it. The engine dlopens it at startup and, if the
// symbol is present, forwards open() calls to `on_open` instead of the
// real libc open.
//
// NOTE: the engine currently hooks only `open`, so this template only
// defines on_open. (Deleting unused functions is fine — the engine only
// loads the symbols it knows about.)
//
// Build:  rustc --edition=2021 --crate-type cdylib -O examples/swap.rs -o swap.dylib
// Run:    MTRACE_SWAP_DYLIB=$PWD/swap.dylib mtrace ./your_binary
//         (or pass it via the CLI when the swap flag exists)

#![allow(clashing_extern_declarations)]

use std::os::raw::{c_char, c_int};

extern "C" {
    // Raw syscall forwarding: bypasses libc (and therefore our own hook),
    // so the swap can't recurse back into on_open.
    #[link_name = "syscall"]
    fn syscall(number: c_int, ...) -> c_int;
}

// macOS Syscall Numbers (arm64)
const SYS_OPEN: c_int = 5;

#[no_mangle]
pub unsafe extern "C" fn on_open(path: *const c_char, oflag: c_int, mode: c_int) -> c_int {
    // TODO: Add logic here to sandbox or mutate the open() call.
    // E.g. return EACCES for paths you don't want opened, rewrite the
    // path, or spoof the result entirely.
    syscall(SYS_OPEN, path, oflag, mode)
}
