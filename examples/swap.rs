#![allow(clashing_extern_declarations)]

use std::os::raw::{c_char, c_int, c_void};

extern "C" {
    fn __error() -> *mut c_int;
    fn syscall(number: c_int, ...) -> c_int;
    
    // For 64-bit returns like mmap/read/write
    #[link_name = "syscall"]
    fn syscall_ptr(number: c_int, ...) -> *mut c_void;
    
    #[link_name = "syscall"]
    fn syscall_isize(number: c_int, ...) -> isize;
}

// macOS Syscall Numbers
const SYS_EXIT: c_int = 1;
const SYS_FORK: c_int = 2;
const SYS_READ: c_int = 3;
const SYS_WRITE: c_int = 4;
const SYS_OPEN: c_int = 5;
const SYS_CLOSE: c_int = 6;
const SYS_EXECVE: c_int = 59;
const SYS_MUNMAP: c_int = 73;
const SYS_SOCKET: c_int = 97;
const SYS_CONNECT: c_int = 98;
const SYS_UNLINK: c_int = 10;
const SYS_RECVFROM: c_int = 29;
const SYS_ACCEPT: c_int = 30;
const SYS_BIND: c_int = 104;
const SYS_LISTEN: c_int = 106;
const SYS_RENAME: c_int = 128;
const SYS_SENDTO: c_int = 133;
const SYS_MKDIR: c_int = 136;
const SYS_RMDIR: c_int = 137;
const SYS_STAT64: c_int = 338;
const SYS_FSTAT64: c_int = 339;
const SYS_LSTAT64: c_int = 340;
const SYS_MMAP: c_int = 197;

#[no_mangle]
pub unsafe extern "C" fn on_open(path: *const c_char, oflag: c_int, mode: c_int) -> c_int {
    // let path_str = std::ffi::CStr::from_ptr(path).to_str().unwrap_or("");
    // TODO: Add logic here to sandbox or mutate the open() call
    syscall(SYS_OPEN, path, oflag, mode)
}

#[no_mangle]
pub unsafe extern "C" fn on_close(fd: c_int) -> c_int {
    // TODO: Add logic here for close()
    syscall(SYS_CLOSE, fd)
}

#[no_mangle]
pub unsafe extern "C" fn on_read(fd: c_int, buf: *mut c_void, count: usize) -> isize {
    // TODO: Add logic here for read()
    syscall_isize(SYS_READ, fd, buf, count)
}

#[no_mangle]
pub unsafe extern "C" fn on_write(fd: c_int, buf: *const c_void, count: usize) -> isize {
    // TODO: Add logic here for write()
    syscall_isize(SYS_WRITE, fd, buf, count)
}

#[no_mangle]
pub unsafe extern "C" fn on_socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int {
    // TODO: Add logic here for socket()
    syscall(SYS_SOCKET, domain, ty, protocol)
}

#[no_mangle]
pub unsafe extern "C" fn on_connect(socket: c_int, address: *const c_void, len: u32) -> c_int {
    // TODO: Add logic here for connect()
    syscall(SYS_CONNECT, socket, address, len)
}

#[no_mangle]
pub unsafe extern "C" fn on_send(socket: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize {
    // send() is identical to sendto() with NULL address in macOS kernel
    syscall_isize(SYS_SENDTO, socket, buf, len, flags, std::ptr::null::<c_void>(), 0)
}

#[no_mangle]
pub unsafe extern "C" fn on_recv(socket: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize {
    // recv() is identical to recvfrom() with NULL address in macOS kernel
    syscall_isize(SYS_RECVFROM, socket, buf, len, flags, std::ptr::null_mut::<c_void>(), std::ptr::null_mut::<u32>())
}

#[no_mangle]
pub unsafe extern "C" fn on_stat(path: *const c_char, buf: *mut c_void) -> c_int {
    // TODO: Add logic here for stat()
    syscall(SYS_STAT64, path, buf)
}

#[no_mangle]
pub unsafe extern "C" fn on_execve(path: *const c_char, argv: *const *mut c_char, envp: *const *mut c_char) -> c_int {
    // TODO: Add logic here for execve()
    syscall(SYS_EXECVE, path, argv, envp)
}

#[no_mangle]
pub unsafe extern "C" fn on_fork() -> i32 {
    // TODO: Add logic here for fork()
    syscall(SYS_FORK)
}

#[no_mangle]
pub unsafe extern "C" fn on_exit(status: c_int) -> ! {
    // TODO: Add logic here for exit()
    syscall(SYS_EXIT, status);
    core::hint::unreachable_unchecked()
}

#[no_mangle]
pub unsafe extern "C" fn on_mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, offset: i64) -> *mut c_void {
    // TODO: Add logic here for mmap()
    syscall_ptr(SYS_MMAP, addr, len, prot, flags, fd, offset)
}

#[no_mangle]
pub unsafe extern "C" fn on_munmap(addr: *mut c_void, len: usize) -> c_int {
    // TODO: Add logic here for munmap()
    syscall(SYS_MUNMAP, addr, len)
}

#[no_mangle]
pub unsafe extern "C" fn on_unlink(path: *const c_char) -> c_int {
    syscall(SYS_UNLINK, path)
}

#[no_mangle]
pub unsafe extern "C" fn on_rename(old: *const c_char, new: *const c_char) -> c_int {
    syscall(SYS_RENAME, old, new)
}

#[no_mangle]
pub unsafe extern "C" fn on_lstat(path: *const c_char, buf: *mut c_void) -> c_int {
    syscall(SYS_LSTAT64, path, buf)
}

#[no_mangle]
pub unsafe extern "C" fn on_fstat(fildes: c_int, buf: *mut c_void) -> c_int {
    syscall(SYS_FSTAT64, fildes, buf)
}

#[no_mangle]
pub unsafe extern "C" fn on_bind(socket: c_int, address: *const c_void, address_len: u32) -> c_int {
    syscall(SYS_BIND, socket, address, address_len)
}

#[no_mangle]
pub unsafe extern "C" fn on_listen(socket: c_int, backlog: c_int) -> c_int {
    syscall(SYS_LISTEN, socket, backlog)
}

#[no_mangle]
pub unsafe extern "C" fn on_accept(socket: c_int, address: *mut c_void, address_len: *mut u32) -> c_int {
    syscall(SYS_ACCEPT, socket, address, address_len)
}

#[no_mangle]
pub unsafe extern "C" fn on_sendto(socket: c_int, buf: *const c_void, len: usize, flags: c_int, dest_addr: *const c_void, dest_len: u32) -> isize {
    syscall_isize(SYS_SENDTO, socket, buf, len, flags, dest_addr, dest_len)
}

#[no_mangle]
pub unsafe extern "C" fn on_recvfrom(socket: c_int, buf: *mut c_void, len: usize, flags: c_int, address: *mut c_void, address_len: *mut u32) -> isize {
    syscall_isize(SYS_RECVFROM, socket, buf, len, flags, address, address_len)
}

#[no_mangle]
pub unsafe extern "C" fn on_mkdir(path: *const c_char, mode: u16) -> c_int {
    syscall(SYS_MKDIR, path, mode)
}

#[no_mangle]
pub unsafe extern "C" fn on_rmdir(path: *const c_char) -> c_int {
    syscall(SYS_RMDIR, path)
}
