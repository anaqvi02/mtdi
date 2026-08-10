use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::io::Write;

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::ptr;
use std::thread;

extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut mach_timebase_info_data_t) -> libc::c_int;
}
#[repr(C)]
struct mach_timebase_info_data_t {
    numer: u32,
    denom: u32,
}
static mut TIMEBASE: mach_timebase_info_data_t = mach_timebase_info_data_t { numer: 0, denom: 0 };
static mut INIT_TIMEOFDAY_USEC: u64 = 0;
static mut INIT_MACH_TIME: u64 = 0;


pub struct Slot {
    pub ready: AtomicBool,
    pub timestamp: u64,
    pub formatter: Option<fn(&Slot, bool, bool, &mut core::fmt::Formatter) -> core::fmt::Result>,
    pub args: [u64; 6],
    pub str1_len: usize,
    pub str2_len: usize,
    pub str_data: [u8; 1024],
}

impl Slot {
    fn get_str1(&self) -> &str {
        core::str::from_utf8(unsafe { core::slice::from_raw_parts(self.str_data.as_ptr(), self.str1_len) }).unwrap_or("<invalid>")
    }
    fn get_str2(&self) -> &str {
        core::str::from_utf8(unsafe { core::slice::from_raw_parts(self.str_data.as_ptr().add(self.str1_len), self.str2_len) }).unwrap_or("<invalid>")
    }
}

struct SlotFormatter<'a> {
    slot: &'a Slot,
    json: bool,
    ecs: bool,
}
impl<'a> core::fmt::Display for SlotFormatter<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(func) = self.slot.formatter {
            func(self.slot, self.json, self.ecs, f)
        } else {
            Ok(())
        }
    }
}

const MAX_THREADS: usize = 128;
const QUEUE_SIZE: usize = 1024;
const MASK: usize = QUEUE_SIZE - 1;

pub struct ThreadQueue {
    pub slots: [Slot; QUEUE_SIZE],
    pub write_head: usize,
}

static mut THREAD_QUEUES: *mut ThreadQueue = ptr::null_mut();
static ACTIVE_QUEUES: AtomicUsize = AtomicUsize::new(0);
static THREAD_KEY: AtomicUsize = AtomicUsize::new(0);

static LOG_FD: AtomicI32 = AtomicI32::new(2);
static FILTER_MASK: AtomicU32 = AtomicU32::new(0xFFFFFFFF);
static JSON_OUTPUT: AtomicBool = AtomicBool::new(false);
static ECS_OUTPUT: AtomicBool = AtomicBool::new(false);
static NDUMP_ENABLED: AtomicBool = AtomicBool::new(false);
static NDUMP_HINT_PRINTED: AtomicBool = AtomicBool::new(false);
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static NDUMP_FD: AtomicI32 = AtomicI32::new(-1);

static USER_ON_OPEN: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_CLOSE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_READ: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_WRITE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_SOCKET: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_CONNECT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_SEND: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_RECV: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_STAT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_EXECVE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_FORK: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_EXIT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_MMAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_MUNMAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_UNLINK: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_RENAME: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_LSTAT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_FSTAT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_BIND: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_LISTEN: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_ACCEPT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_SENDTO: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_RECVFROM: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_MKDIR: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static USER_ON_RMDIR: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INITIALIZE: unsafe extern "C" fn() = {
    unsafe extern "C" fn init() {

        unsafe {
            mach_timebase_info(&mut TIMEBASE);
            let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
            libc::gettimeofday(&mut tv, std::ptr::null_mut());
            INIT_TIMEOFDAY_USEC = (tv.tv_sec as u64) * 1_000_000 + (tv.tv_usec as u64);
            INIT_MACH_TIME = mach_absolute_time();
        }

        // Install our inline hooks
        if let Ok(tramp_addr) = hook::manager::install_hook("open", libc::open as usize, hook::manager::HookType::FastPath(my_open as usize)) {
            TRAMP_OPEN.store(tramp_addr, Ordering::SeqCst);
        } else {
            eprintln!("[mtdi] Failed to install hook for open!");
        }

        let env_swap = b"MTRACE_SWAP_DYLIB\0".as_ptr() as *const c_char;
        let swap_ptr = unsafe { libc::getenv(env_swap) };
        if !swap_ptr.is_null() {
            let handle = unsafe { libc::dlopen(swap_ptr, libc::RTLD_LAZY | libc::RTLD_LOCAL) };
            if !handle.is_null() {
                macro_rules! load_sym {
                    ($name:expr, $static_var:expr) => {
                        let sym = unsafe { libc::dlsym(handle, concat!($name, "\0").as_ptr() as *const c_char) };
                        if !sym.is_null() { $static_var.store(sym as *mut c_void, Ordering::Relaxed); }
                    };
                }
                load_sym!("on_open", USER_ON_OPEN);
                load_sym!("on_close", USER_ON_CLOSE);
                load_sym!("on_read", USER_ON_READ);
                load_sym!("on_write", USER_ON_WRITE);
                load_sym!("on_socket", USER_ON_SOCKET);
                load_sym!("on_connect", USER_ON_CONNECT);
                load_sym!("on_send", USER_ON_SEND);
                load_sym!("on_recv", USER_ON_RECV);
                load_sym!("on_stat", USER_ON_STAT);
                load_sym!("on_execve", USER_ON_EXECVE);
                load_sym!("on_fork", USER_ON_FORK);
                load_sym!("on_exit", USER_ON_EXIT);
                load_sym!("on_mmap", USER_ON_MMAP);
                load_sym!("on_munmap", USER_ON_MUNMAP);
                load_sym!("on_unlink", USER_ON_UNLINK);
                load_sym!("on_rename", USER_ON_RENAME);
                load_sym!("on_lstat", USER_ON_LSTAT);
                load_sym!("on_fstat", USER_ON_FSTAT);
                load_sym!("on_bind", USER_ON_BIND);
                load_sym!("on_listen", USER_ON_LISTEN);
                load_sym!("on_accept", USER_ON_ACCEPT);
                load_sym!("on_sendto", USER_ON_SENDTO);
                load_sym!("on_recvfrom", USER_ON_RECVFROM);
                load_sym!("on_mkdir", USER_ON_MKDIR);
                load_sym!("on_rmdir", USER_ON_RMDIR);
            }
        }

        let env_out = b"MTRACE_OUTPUT\0".as_ptr() as *const c_char;
        let out_ptr = unsafe { libc::getenv(env_out) };
        if !out_ptr.is_null() {
            let fd = unsafe { libc::open(out_ptr, libc::O_CREAT | libc::O_WRONLY | libc::O_APPEND | libc::O_CLOEXEC, 0o666) };
            if fd >= 0 {
                LOG_FD.store(fd, Ordering::Relaxed);
            }
        }
        
        let env_filter = b"MTRACE_FILTER\0".as_ptr() as *const c_char;
        let filter_ptr = unsafe { libc::getenv(env_filter) };
        if !filter_ptr.is_null() {
            FILTER_MASK.store(0, Ordering::Relaxed);
            if let Ok(filter_str) = core::str::from_utf8(unsafe { CStr::from_ptr(filter_ptr).to_bytes() }) {
                for s in filter_str.split(',') {
                    match s.trim() {
                        "open" => { FILTER_MASK.fetch_or(1 << 0, Ordering::Relaxed); },
                        "close" => { FILTER_MASK.fetch_or(1 << 1, Ordering::Relaxed); },
                        "read" => { FILTER_MASK.fetch_or(1 << 2, Ordering::Relaxed); },
                        "write" => { FILTER_MASK.fetch_or(1 << 3, Ordering::Relaxed); },
                        "socket" => { FILTER_MASK.fetch_or(1 << 4, Ordering::Relaxed); },
                        "connect" => { FILTER_MASK.fetch_or(1 << 5, Ordering::Relaxed); },
                        "send" => { FILTER_MASK.fetch_or(1 << 6, Ordering::Relaxed); },
                        "recv" => { FILTER_MASK.fetch_or(1 << 7, Ordering::Relaxed); },
                        "stat" => { FILTER_MASK.fetch_or(1 << 8, Ordering::Relaxed); },
                        "execve" => { FILTER_MASK.fetch_or(1 << 9, Ordering::Relaxed); },
                        "fork" => { FILTER_MASK.fetch_or(1 << 10, Ordering::Relaxed); },
                        "exit" => { FILTER_MASK.fetch_or(1 << 11, Ordering::Relaxed); },
                        "mmap" => { FILTER_MASK.fetch_or(1 << 12, Ordering::Relaxed); },
                        "munmap" => { FILTER_MASK.fetch_or(1 << 13, Ordering::Relaxed); },
                        "unlink" => { FILTER_MASK.fetch_or(1 << 14, Ordering::Relaxed); },
                        "rename" => { FILTER_MASK.fetch_or(1 << 15, Ordering::Relaxed); },
                        "lstat" => { FILTER_MASK.fetch_or(1 << 16, Ordering::Relaxed); },
                        "fstat" => { FILTER_MASK.fetch_or(1 << 17, Ordering::Relaxed); },
                        "bind" => { FILTER_MASK.fetch_or(1 << 18, Ordering::Relaxed); },
                        "listen" => { FILTER_MASK.fetch_or(1 << 19, Ordering::Relaxed); },
                        "accept" => { FILTER_MASK.fetch_or(1 << 20, Ordering::Relaxed); },
                        "sendto" => { FILTER_MASK.fetch_or(1 << 21, Ordering::Relaxed); },
                        "recvfrom" => { FILTER_MASK.fetch_or(1 << 22, Ordering::Relaxed); },
                        "mkdir" => { FILTER_MASK.fetch_or(1 << 23, Ordering::Relaxed); },
                        "rmdir" => { FILTER_MASK.fetch_or(1 << 24, Ordering::Relaxed); },
                        _ => {}
                    }
                }
            }
        }

        let env_json = b"MTRACE_JSON\0".as_ptr() as *const c_char;
        let json_ptr = unsafe { libc::getenv(env_json) };
        if !json_ptr.is_null() {
            JSON_OUTPUT.store(true, Ordering::Relaxed);
        }

        let env_ecs = b"MTRACE_ECS\0".as_ptr() as *const c_char;
        let ecs_ptr = unsafe { libc::getenv(env_ecs) };
        if !ecs_ptr.is_null() {
            ECS_OUTPUT.store(true, Ordering::Relaxed);
        }

        let env_ndump = b"MTRACE_NDUMP\0".as_ptr() as *const c_char;
        let ndump_ptr = unsafe { libc::getenv(env_ndump) };
        if !ndump_ptr.is_null() {
            NDUMP_ENABLED.store(true, Ordering::Relaxed);
            let ndump_path = b"mtrace_ndump.log\0".as_ptr() as *const c_char;
            let fd = unsafe { libc::open(ndump_path, libc::O_CREAT | libc::O_WRONLY | libc::O_APPEND, 0o644) };
            if fd >= 0 {
                NDUMP_FD.store(fd, Ordering::Relaxed);
            }
        }

        if ECS_OUTPUT.load(Ordering::Relaxed) {
            let msg = b"{\"@timestamp\":\"2000-01-01T00:00:00Z\",\"event\":{\"action\":\"init\"},\"message\":\"mactrace active\"}\n\0";
            unsafe { libc::write(LOG_FD.load(Ordering::Relaxed), msg.as_ptr() as *const c_void, msg.len() - 1); }
        } else if JSON_OUTPUT.load(Ordering::Relaxed) {
            let msg = b"{\"event\":\"mactrace_active\"}\n\0";
            unsafe { libc::write(LOG_FD.load(Ordering::Relaxed), msg.as_ptr() as *const c_void, msg.len() - 1); }
        } else {
            let msg = b"[mt] Active! Monitoring system calls...\n\0";
            unsafe { libc::write(LOG_FD.load(Ordering::Relaxed), msg.as_ptr() as *const c_void, msg.len() - 1); }
        }
        unsafe {
            let mut key: libc::pthread_key_t = 0;
            libc::pthread_key_create(&mut key, None);
            THREAD_KEY.store(key as usize, Ordering::SeqCst);
            
            let size = MAX_THREADS * core::mem::size_of::<ThreadQueue>();
            let ptr = libc::mmap(core::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0);
            core::ptr::write_bytes(ptr, 0, size);
            THREAD_QUEUES = ptr as *mut ThreadQueue;
        }

        thread::spawn(move || {
            let mut read_heads = [0usize; MAX_THREADS];
            loop {
                unsafe {
                    if THREAD_QUEUES.is_null() {
                        thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    
                    let active = ACTIVE_QUEUES.load(Ordering::Acquire);
                    let mut idle = true;
                    
                    for i in 0..active {
                        let q = &mut *THREAD_QUEUES.add(i);
                        let mut read_idx = read_heads[i];
                        
                        for _ in 0..64 { // process up to 64 per queue to ensure fairness
                            let slot_idx = read_idx & MASK;
                            let slot = &mut q.slots[slot_idx];
                            
                            if !slot.ready.load(Ordering::Acquire) {
                                break;
                            }
                            idle = false;
                        let is_json = JSON_OUTPUT.load(Ordering::Relaxed);
                        let is_ecs = ECS_OUTPUT.load(Ordering::Relaxed);
                        
                        let delta_mach = slot.timestamp.saturating_sub(INIT_MACH_TIME);
                        let delta_ns = delta_mach * (TIMEBASE.numer as u64) / (TIMEBASE.denom as u64);
                        let current_usec = INIT_TIMEOFDAY_USEC + (delta_ns / 1000);
                        let sec = current_usec / 1_000_000;
                        let usec = current_usec % 1_000_000;
                        
                        let mut buf = [0u8; 4096];
                        let mut slice = &mut buf[..];
                        
                        let formatter = SlotFormatter { slot, json: is_json, ecs: is_ecs };
                        
                        if is_ecs {
                            let mut tm: libc::tm = core::mem::zeroed();
                            let tv_sec = sec as libc::time_t;
                            libc::gmtime_r(&tv_sec, &mut tm);
                            let _ = write!(slice, "{{\"@timestamp\":\"{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z\",{}",
                                tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday,
                                tm.tm_hour, tm.tm_min, tm.tm_sec, usec / 1000,
                                formatter
                            );
                        } else if is_json {
                            let h = (sec / 3600) % 24;
                            let m = (sec / 60) % 60;
                            let s = sec % 60;
                            let _ = write!(slice, "{{\"timestamp\":\"{:02}:{:02}:{:02}.{:06}\",{}", h, m, s, usec, formatter);
                        } else {
                            let h = (sec / 3600) % 24;
                            let m = (sec / 60) % 60;
                            let s = sec % 60;
                            let _ = write!(slice, "[{:02}:{:02}:{:02}.{:06}] [mt] Caught {}\n", h, m, s, usec, formatter);
                        }
                        
                        let len = 4096 - slice.len();
                        libc::write(LOG_FD.load(Ordering::Relaxed), buf.as_ptr() as *const libc::c_void, len);
                        
                        slot.ready.store(false, Ordering::Release);
                        read_idx += 1;
                        }
                        read_heads[i] = read_idx;
                    }
                    if idle {
                        std::thread::sleep(std::time::Duration::from_nanos(100));
                    }
                }
            }
        });

        INITIALIZED.store(true, Ordering::Relaxed);
    }
    init
};

#[repr(C)]
pub struct Interpose {
    replacement: *const (),
    replacee: *const (),
}
unsafe impl Sync for Interpose {}

macro_rules! interpose {
    ($rep:ident, $orig:path) => {
        Interpose {
            replacement: $rep as *const (),
            replacee: $orig as *const (),
        }
    };
}

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
pub static INTERPOSE_ARRAY: [Interpose; 0] = [];

fn parse_sockaddr(addr: *const libc::sockaddr) -> String {
    if addr.is_null() { return "null".to_string(); }
    unsafe {
        let family = (*addr).sa_family as i32;
        if family == libc::AF_INET {
            let addr_in = &*(addr as *const libc::sockaddr_in);
            let ip = u32::from_be(addr_in.sin_addr.s_addr);
            let port = u16::from_be(addr_in.sin_port);
            return format!("{}.{}.{}.{}:{}", (ip >> 24) & 0xFF, (ip >> 16) & 0xFF, (ip >> 8) & 0xFF, ip & 0xFF, port);
        } else if family == libc::AF_INET6 {
            let addr_in6 = &*(addr as *const libc::sockaddr_in6);
            let port = u16::from_be(addr_in6.sin6_port);
            return format!("IPv6:[...]:{}", port);
        } else if family == libc::AF_UNIX {
            return "AF_UNIX".to_string();
        }
        format!("AF_UNKNOWN({})", family)
    }
}

fn maybe_print_ndump_hint() {
    if !INITIALIZED.load(Ordering::Relaxed) { return; }
    if !NDUMP_ENABLED.load(Ordering::Relaxed) {
        if !NDUMP_HINT_PRINTED.swap(true, Ordering::Relaxed) {
            let hint = b"[mt] Hint: Enable --ndump to dump network/I/O data into the log!\n\0";
            let fd = LOG_FD.load(Ordering::Relaxed);
            if fd >= 0 {
                unsafe { libc::write(fd, hint.as_ptr() as *const c_void, hint.len() - 1) };
            }
        }
    }
}

fn dump_buffer(action: &str, fd_or_socket: c_int, buf: *const c_void, len: usize) {
    if !INITIALIZED.load(Ordering::Relaxed) { return; }
    if NDUMP_ENABLED.load(Ordering::Relaxed) {
        let dump_fd = NDUMP_FD.load(Ordering::Relaxed);
        if dump_fd < 0 || buf.is_null() || len == 0 { return; }
        
        let header = format!("\n--- {} (fd/socket: {}, bytes: {}) ---\n", action, fd_or_socket, len);
        unsafe { libc::write(dump_fd, header.as_ptr() as *const c_void, header.len()) };
        
        let cap = core::cmp::min(len, 1024 * 1024); // Cap at 1MB per dump
        unsafe { libc::write(dump_fd, buf, cap) };
        
        unsafe { libc::write(dump_fd, b"\n".as_ptr() as *const c_void, 1) };
    } else {
        maybe_print_ndump_hint();
    }
}

#[inline(always)]
fn should_log(bit: u32) -> bool {
    (FILTER_MASK.load(Ordering::Relaxed) & (1 << bit)) != 0
}

struct JsonEscape<'a>(&'a str);
impl<'a> core::fmt::Display for JsonEscape<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for c in self.0.chars() {
            match c {
                '"' => write!(f, "\\\"")?,
                '\\' => write!(f, "\\\\")?,
                '\n' => write!(f, "\\n")?,
                '\r' => write!(f, "\\r")?,
                '\t' => write!(f, "\\t")?,
                c if c < '\x20' => write!(f, "\\u{:04x}", c as u32)?,
                c => write!(f, "{}", c)?,
            }
        }
        Ok(())
    }
}

fn get_timestamp_str(buf: &mut [u8]) -> usize {
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) };
    let sec = tv.tv_sec % 86400;
    let h = (sec / 3600) % 24;
    let m = (sec / 60) % 60;
    let s = sec % 60;
    
    let total_len = buf.len();
    let mut slice = &mut buf[..];
    let _ = write!(slice, "{:02}:{:02}:{:02}.{:06}", h, m, s, tv.tv_usec);
    total_len - slice.len()
}

fn get_iso8601_str(buf: &mut [u8]) -> usize {
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { core::mem::zeroed() };
    unsafe { libc::gmtime_r(&tv.tv_sec, &mut tm) };
    
    let total_len = buf.len();
    let mut slice = &mut buf[..];
    let _ = write!(slice, "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday,
        tm.tm_hour, tm.tm_min, tm.tm_sec, tv.tv_usec / 1000
    );
    total_len - slice.len()
}

fn c_char_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() { return "null".to_string(); }
    let len = unsafe { libc::strnlen(ptr, 1024) };
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    core::str::from_utf8(bytes).unwrap_or("<invalid_utf8>").to_string()
}

#[inline(always)]
fn push_binary_event(
    formatter: fn(&Slot, bool, bool, &mut core::fmt::Formatter) -> core::fmt::Result,
    args: [u64; 6],
    c_str1: *const c_char,
    c_str2: *const c_char,
) {
    let timestamp = unsafe { mach_absolute_time() };
    unsafe {
        if !THREAD_QUEUES.is_null() {
            let key = THREAD_KEY.load(Ordering::Relaxed) as libc::pthread_key_t;
            let mut q_ptr = libc::pthread_getspecific(key) as *mut ThreadQueue;
            
            if q_ptr.is_null() {
                let q_idx = ACTIVE_QUEUES.fetch_add(1, Ordering::Relaxed);
                if q_idx >= MAX_THREADS { return; } // Too many threads
                q_ptr = THREAD_QUEUES.add(q_idx);
                libc::pthread_setspecific(key, q_ptr as *mut libc::c_void);
            }
            
            let q = &mut *q_ptr;
            let write_idx = q.write_head;
            let slot_idx = write_idx & MASK;
            let slot = &mut q.slots[slot_idx];
            
            if slot.ready.load(Ordering::Acquire) {
                return; // Queue is full, drop event
            }
            
            slot.timestamp = timestamp;
            slot.formatter = Some(formatter);
            slot.args = args;
            
            let mut offset = 0;
            if !c_str1.is_null() {
                let len = libc::strnlen(c_str1, 512);
                ptr::copy_nonoverlapping(c_str1 as *const u8, slot.str_data.as_mut_ptr(), len);
                slot.str1_len = len;
                offset = len;
            } else {
                slot.str1_len = 0;
            }
            
            if !c_str2.is_null() {
                let len2 = libc::strnlen(c_str2, 512);
                let max_len2 = core::cmp::min(len2, 1024 - offset);
                ptr::copy_nonoverlapping(c_str2 as *const u8, slot.str_data.as_mut_ptr().add(offset), max_len2);
                slot.str2_len = max_len2;
            } else {
                slot.str2_len = 0;
            }
            
            q.write_head = write_idx + 1;
            slot.ready.store(true, Ordering::Release);
        }
    }
}

fn fmt_open(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let path = JsonEscape(s.get_str1());
    let oflag = s.args[0]; let mode = s.args[1];
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"open\"}},\"message\":\"[mt] Caught open(\"{}\", {}, {})\",\"mactrace\":{{\"path\":\"{}\",\"oflag\":{},\"mode\":{}}}", path, oflag, mode, path, oflag, mode) }
    else if j { write!(f, "\"syscall\":\"open\",\"args\":{{\"path\":\"{}\",\"oflag\":{},\"mode\":{}}}", path, oflag, mode) }
    else { write!(f, "open(\"{}\", {}, {})", s.get_str1(), oflag, mode) }
}

fn log_event(_syscall: &str, _args_content: core::fmt::Arguments, _plain_msg: core::fmt::Arguments) {
    // Dummy, they aren't used in bench
}

static TRAMP_OPEN: AtomicUsize = AtomicUsize::new(0);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_open(path: *const c_char, oflag: c_int, mode: c_int) -> c_int { unsafe {
    let p = USER_ON_OPEN.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*const c_char, c_int, c_int) -> c_int = core::mem::transmute(p);
        return func(path, oflag, mode);
    }
    
    let tramp_addr = TRAMP_OPEN.load(Ordering::Relaxed);
    let orig_open: unsafe extern "C" fn(*const c_char, c_int, c_int) -> c_int = core::mem::transmute(tramp_addr);
    
    if !should_log(0) { return orig_open(path, oflag, mode); }
    push_binary_event(fmt_open, [oflag as u64, mode as u64, 0, 0, 0, 0], path, core::ptr::null());
    orig_open(path, oflag, mode)
}}

fn fmt_close(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let fd = s.args[0] as i32;
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"close\"}},\"message\":\"[mt] Caught close({})\",\"mactrace\":{{\"fd\":{}}}", fd, fd) }
    else if j { write!(f, "\"syscall\":\"close\",\"args\":{{\"fd\":{}}}", fd) }
    else { write!(f, "close({})", fd) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_close(fd: c_int) -> c_int { unsafe {
    let p = USER_ON_CLOSE.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int) -> c_int = core::mem::transmute(p);
        return func(fd);
    }
    if !should_log(1) { return libc::close(fd); }
    push_binary_event(fmt_close, [fd as u64, 0, 0, 0, 0, 0], core::ptr::null(), core::ptr::null());
    libc::close(fd)
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_read(fd: c_int, buf: *mut c_void, count: usize) -> isize { unsafe {
    let p = USER_ON_READ.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *mut c_void, usize) -> isize = core::mem::transmute(p);
        return func(fd, buf, count);
    }
    if !should_log(2) { return unsafe { libc::read(fd, buf, count) } }
    let ret = unsafe { libc::read(fd, buf, count) };
    if ret > 0 { dump_buffer("read", fd, buf, ret as usize); }
    log_event(
        "read",
        format_args!("\"fd\":{},\"count\":{},\"ret\":{}", fd, count, ret),
        format_args!("read({}, buf, {}) -> {}", fd, count, ret)
    );
    ret
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_write(fd: c_int, buf: *const c_void, count: usize) -> isize { unsafe {
    let p = USER_ON_WRITE.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *const c_void, usize) -> isize = core::mem::transmute(p);
        return func(fd, buf, count);
    }
    if !should_log(3) { return unsafe { libc::write(fd, buf, count) } }
    dump_buffer("write", fd, buf, count);
    log_event(
        "write",
        format_args!("\"fd\":{},\"count\":{}", fd, count),
        format_args!("write({}, buf, {})", fd, count)
    );
    unsafe { libc::write(fd, buf, count) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int { unsafe {
    let p = USER_ON_SOCKET.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, c_int, c_int) -> c_int = core::mem::transmute(p);
        return func(domain, ty, protocol);
    }
    if !should_log(4) { return unsafe { libc::socket(domain, ty, protocol) } }
    let ret = unsafe { libc::socket(domain, ty, protocol) };
    log_event(
        "socket",
        format_args!("\"domain\":{},\"type\":{},\"protocol\":{},\"ret\":{}", domain, ty, protocol, ret),
        format_args!("socket({}, {}, {}) -> {}", domain, ty, protocol, ret)
    );
    ret
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_connect(socket: c_int, address: *const libc::sockaddr, len: libc::socklen_t) -> c_int { unsafe {
    let p = USER_ON_CONNECT.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *const libc::sockaddr, libc::socklen_t) -> c_int = core::mem::transmute(p);
        return func(socket, address, len);
    }
    if !should_log(5) { return unsafe { libc::connect(socket, address, len) } }
    let addr_str = parse_sockaddr(address);
    log_event(
        "connect",
        format_args!("\"socket\":{},\"address\":\"{}\",\"len\":{}", socket, JsonEscape(&addr_str), len),
        format_args!("connect({}, {}, {})", socket, addr_str, len)
    );
    unsafe { libc::connect(socket, address, len) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_send(socket: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize { unsafe {
    let p = USER_ON_SEND.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *const c_void, usize, c_int) -> isize = core::mem::transmute(p);
        return func(socket, buf, len, flags);
    }
    if !should_log(6) { return unsafe { libc::send(socket, buf, len, flags) } }
    dump_buffer("send", socket, buf, len);
    log_event(
        "send",
        format_args!("\"socket\":{},\"len\":{},\"flags\":{}", socket, len, flags),
        format_args!("send({}, buf, {}, {})", socket, len, flags)
    );
    unsafe { libc::send(socket, buf, len, flags) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_recv(socket: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize { unsafe {
    let p = USER_ON_RECV.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *mut c_void, usize, c_int) -> isize = core::mem::transmute(p);
        return func(socket, buf, len, flags);
    }
    if !should_log(7) { return unsafe { libc::recv(socket, buf, len, flags) } }
    let ret = unsafe { libc::recv(socket, buf, len, flags) };
    if ret > 0 { dump_buffer("recv", socket, buf, ret as usize); }
    log_event(
        "recv",
        format_args!("\"socket\":{},\"len\":{},\"flags\":{},\"ret\":{}", socket, len, flags, ret),
        format_args!("recv({}, buf, {}, {}) -> {}", socket, len, flags, ret)
    );
    ret
}}

fn fmt_stat(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let path = JsonEscape(s.get_str1());
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"stat\"}},\"message\":\"[mt] Caught stat(\"{}\", buf)\",\"mactrace\":{{\"path\":\"{}\"}}", path, path) }
    else if j { write!(f, "\"syscall\":\"stat\",\"args\":{{\"path\":\"{}\"}}", path) }
    else { write!(f, "stat(\"{}\", buf)", s.get_str1()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_stat(path: *const c_char, buf: *mut libc::stat) -> c_int { unsafe {
    let p = USER_ON_STAT.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*const c_char, *mut libc::stat) -> c_int = core::mem::transmute(p);
        return func(path, buf);
    }
    if !should_log(8) { return libc::stat(path, buf); }
    push_binary_event(fmt_stat, [0, 0, 0, 0, 0, 0], path, core::ptr::null());
    libc::stat(path, buf)
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_execve(path: *const c_char, argv: *const *mut c_char, envp: *const *mut c_char) -> c_int { unsafe {
    let p = USER_ON_EXECVE.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*const c_char, *const *mut c_char, *const *mut c_char) -> c_int = core::mem::transmute(p);
        return func(path, argv, envp);
    }
    if !should_log(9) { return unsafe { libc::execve(path, argv as *const *const c_char, envp as *const *const c_char) } }
    let path_string = c_char_to_string(path);
    let escaped = JsonEscape(&path_string);
    log_event(
        "execve",
        format_args!("\"path\":\"{}\"", escaped),
        format_args!("execve(\"{}\", argv, envp)", escaped)
    );
    unsafe { libc::execve(path, argv as *const *const c_char, envp as *const *const c_char) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_fork() -> libc::pid_t { unsafe {
    let p = USER_ON_FORK.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn() -> libc::pid_t = core::mem::transmute(p);
        return func();
    }
    if !should_log(10) { return unsafe { libc::fork() } }
    log_event(
        "fork",
        format_args!(""),
        format_args!("fork()")
    );
    unsafe { libc::fork() }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_exit(status: c_int) -> ! { unsafe {
    let p = USER_ON_EXIT.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int) -> ! = core::mem::transmute(p);
        func(status);
    }
    if should_log(11) {
        log_event(
            "exit",
            format_args!("\"status\":{}", status),
            format_args!("exit({})", status)
        );
    }
    unsafe { libc::exit(status) }
}}

fn fmt_mmap(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let len = s.args[0]; let prot = s.args[1]; let flags = s.args[2]; let fd = s.args[3] as i32; let offset = s.args[4];
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"mmap\"}},\"message\":\"[mt] Caught mmap(addr, {}, {}, {}, {}, {})\",\"mactrace\":{{\"len\":{},\"prot\":{},\"flags\":{},\"fd\":{},\"offset\":{}}}", len, prot, flags, fd, offset, len, prot, flags, fd, offset) }
    else if j { write!(f, "\"syscall\":\"mmap\",\"args\":{{\"len\":{},\"prot\":{},\"flags\":{},\"fd\":{},\"offset\":{}}}", len, prot, flags, fd, offset) }
    else { write!(f, "mmap(addr, {}, {}, {}, {}, {})", len, prot, flags, fd, offset) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, offset: libc::off_t) -> *mut c_void { unsafe {
    let p = USER_ON_MMAP.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*mut c_void, usize, c_int, c_int, c_int, libc::off_t) -> *mut c_void = core::mem::transmute(p);
        return func(addr, len, prot, flags, fd, offset);
    }
    if !should_log(12) { return libc::mmap(addr, len, prot, flags, fd, offset); }
    push_binary_event(fmt_mmap, [len as u64, prot as u64, flags as u64, fd as u64, offset as u64, 0], core::ptr::null(), core::ptr::null());
    libc::mmap(addr, len, prot, flags, fd, offset)
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_munmap(addr: *mut c_void, len: usize) -> c_int { unsafe {
    let p = USER_ON_MUNMAP.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*mut c_void, usize) -> c_int = core::mem::transmute(p);
        return func(addr, len);
    }
    if !should_log(13) { return unsafe { libc::munmap(addr, len) } }
    log_event(
        "munmap",
        format_args!("\"len\":{}", len),
        format_args!("munmap(addr, {})", len)
    );
    unsafe { libc::munmap(addr, len) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_unlink(path: *const c_char) -> c_int { unsafe {
    let p = USER_ON_UNLINK.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*const c_char) -> c_int = core::mem::transmute(p);
        return func(path);
    }
    if !should_log(14) { return unsafe { libc::unlink(path) } }
    let path_string = c_char_to_string(path);
    let escaped = JsonEscape(&path_string);
    log_event(
        "unlink",
        format_args!("\"path\":\"{}\"", escaped),
        format_args!("unlink(\"{}\")", escaped)
    );
    unsafe { libc::unlink(path) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_rename(old: *const c_char, new: *const c_char) -> c_int { unsafe {
    let p = USER_ON_RENAME.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int = core::mem::transmute(p);
        return func(old, new);
    }
    if !should_log(15) { return unsafe { libc::rename(old, new) } }
    let old_string = c_char_to_string(old);
    let new_string = c_char_to_string(new);
    let esc_old = JsonEscape(&old_string);
    let esc_new = JsonEscape(&new_string);
    log_event(
        "rename",
        format_args!("\"old\":\"{}\",\"new\":\"{}\"", esc_old, esc_new),
        format_args!("rename(\"{}\", \"{}\")", esc_old, esc_new)
    );
    unsafe { libc::rename(old, new) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_lstat(path: *const c_char, buf: *mut libc::stat) -> c_int { unsafe {
    let p = USER_ON_LSTAT.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*const c_char, *mut libc::stat) -> c_int = core::mem::transmute(p);
        return func(path, buf);
    }
    if !should_log(16) { return unsafe { libc::lstat(path, buf) } }
    let path_string = c_char_to_string(path);
    let escaped = JsonEscape(&path_string);
    log_event(
        "lstat",
        format_args!("\"path\":\"{}\"", escaped),
        format_args!("lstat(\"{}\", buf)", escaped)
    );
    unsafe { libc::lstat(path, buf) }
}}

fn fmt_fstat(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let fd = s.args[0] as i32;
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"fstat\"}},\"message\":\"[mt] Caught fstat({}, buf)\",\"mactrace\":{{\"fildes\":{}}}", fd, fd) }
    else if j { write!(f, "\"syscall\":\"fstat\",\"args\":{{\"fildes\":{}}}", fd) }
    else { write!(f, "fstat({}, buf)", fd) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_fstat(fildes: c_int, buf: *mut libc::stat) -> c_int { unsafe {
    let p = USER_ON_FSTAT.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *mut libc::stat) -> c_int = core::mem::transmute(p);
        return func(fildes, buf);
    }
    if !should_log(17) { return libc::fstat(fildes, buf); }
    push_binary_event(fmt_fstat, [fildes as u64, 0, 0, 0, 0, 0], core::ptr::null(), core::ptr::null());
    libc::fstat(fildes, buf)
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_bind(socket: c_int, address: *const libc::sockaddr, address_len: libc::socklen_t) -> c_int { unsafe {
    let p = USER_ON_BIND.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *const libc::sockaddr, libc::socklen_t) -> c_int = core::mem::transmute(p);
        return func(socket, address, address_len);
    }
    if !should_log(18) { return unsafe { libc::bind(socket, address, address_len) } }
    let addr_str = parse_sockaddr(address);
    log_event(
        "bind",
        format_args!("\"socket\":{},\"address\":\"{}\",\"address_len\":{}", socket, JsonEscape(&addr_str), address_len),
        format_args!("bind({}, {}, {})", socket, addr_str, address_len)
    );
    unsafe { libc::bind(socket, address, address_len) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_listen(socket: c_int, backlog: c_int) -> c_int { unsafe {
    let p = USER_ON_LISTEN.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, c_int) -> c_int = core::mem::transmute(p);
        return func(socket, backlog);
    }
    if !should_log(19) { return unsafe { libc::listen(socket, backlog) } }
    log_event(
        "listen",
        format_args!("\"socket\":{},\"backlog\":{}", socket, backlog),
        format_args!("listen({}, {})", socket, backlog)
    );
    unsafe { libc::listen(socket, backlog) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_accept(socket: c_int, address: *mut libc::sockaddr, address_len: *mut libc::socklen_t) -> c_int { unsafe {
    let p = USER_ON_ACCEPT.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *mut libc::sockaddr, *mut libc::socklen_t) -> c_int = core::mem::transmute(p);
        return func(socket, address, address_len);
    }
    if !should_log(20) { return unsafe { libc::accept(socket, address, address_len) } }
    let ret = unsafe { libc::accept(socket, address, address_len) };
    let addr_str = parse_sockaddr(address);
    log_event(
        "accept",
        format_args!("\"socket\":{},\"address\":\"{}\",\"ret\":{}", socket, JsonEscape(&addr_str), ret),
        format_args!("accept({}, {}, address_len) -> {}", socket, addr_str, ret)
    );
    ret
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_sendto(socket: c_int, buf: *const c_void, len: usize, flags: c_int, dest_addr: *const libc::sockaddr, dest_len: libc::socklen_t) -> isize { unsafe {
    let p = USER_ON_SENDTO.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *const c_void, usize, c_int, *const libc::sockaddr, libc::socklen_t) -> isize = core::mem::transmute(p);
        return func(socket, buf, len, flags, dest_addr, dest_len);
    }
    if !should_log(21) { return unsafe { libc::sendto(socket, buf, len, flags, dest_addr, dest_len) } }
    let addr_str = parse_sockaddr(dest_addr);
    dump_buffer("sendto", socket, buf, len);
    log_event(
        "sendto",
        format_args!("\"socket\":{},\"len\":{},\"flags\":{},\"dest_addr\":\"{}\",\"dest_len\":{}", socket, len, flags, JsonEscape(&addr_str), dest_len),
        format_args!("sendto({}, buf, {}, {}, {}, {})", socket, len, flags, addr_str, dest_len)
    );
    unsafe { libc::sendto(socket, buf, len, flags, dest_addr, dest_len) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_recvfrom(socket: c_int, buf: *mut c_void, len: usize, flags: c_int, address: *mut libc::sockaddr, address_len: *mut libc::socklen_t) -> isize { unsafe {
    let p = USER_ON_RECVFROM.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(c_int, *mut c_void, usize, c_int, *mut libc::sockaddr, *mut libc::socklen_t) -> isize = core::mem::transmute(p);
        return func(socket, buf, len, flags, address, address_len);
    }
    if !should_log(22) { return unsafe { libc::recvfrom(socket, buf, len, flags, address, address_len) } }
    let ret = unsafe { libc::recvfrom(socket, buf, len, flags, address, address_len) };
    let addr_str = parse_sockaddr(address);
    if ret > 0 { dump_buffer("recvfrom", socket, buf, ret as usize); }
    log_event(
        "recvfrom",
        format_args!("\"socket\":{},\"len\":{},\"flags\":{},\"address\":\"{}\",\"ret\":{}", socket, len, flags, JsonEscape(&addr_str), ret),
        format_args!("recvfrom({}, buf, {}, {}, {}, address_len) -> {}", socket, len, flags, addr_str, ret)
    );
    ret
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_mkdir(path: *const c_char, mode: libc::mode_t) -> c_int { unsafe {
    let p = USER_ON_MKDIR.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*const c_char, libc::mode_t) -> c_int = core::mem::transmute(p);
        return func(path, mode);
    }
    if !should_log(23) { return unsafe { libc::mkdir(path, mode) } }
    let path_string = c_char_to_string(path);
    let escaped = JsonEscape(&path_string);
    log_event(
        "mkdir",
        format_args!("\"path\":\"{}\",\"mode\":{}", escaped, mode),
        format_args!("mkdir(\"{}\", {})", escaped, mode)
    );
    unsafe { libc::mkdir(path, mode) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_rmdir(path: *const c_char) -> c_int { unsafe {
    let p = USER_ON_RMDIR.load(Ordering::Relaxed);
    if !p.is_null() {
        let func: unsafe extern "C" fn(*const c_char) -> c_int = core::mem::transmute(p);
        return func(path);
    }
    if !should_log(24) { return unsafe { libc::rmdir(path) } }
    let path_string = c_char_to_string(path);
    let escaped = JsonEscape(&path_string);
    log_event(
        "rmdir",
        format_args!("\"path\":\"{}\"", escaped),
        format_args!("rmdir(\"{}\")", escaped)
    );
    unsafe { libc::rmdir(path) }
}}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mtrace_log(msg: *const libc::c_char) {
    if msg.is_null() { return; }
    let len = unsafe { libc::strlen(msg) };
    let mut buf = [0u8; 4096];
    let copy_len = core::cmp::min(len, 4096);
    unsafe {
        if !THREAD_QUEUES.is_null() {
            let key = THREAD_KEY.load(Ordering::Relaxed) as libc::pthread_key_t;
            let mut q_ptr = libc::pthread_getspecific(key) as *mut ThreadQueue;
            
            if q_ptr.is_null() {
                let q_idx = ACTIVE_QUEUES.fetch_add(1, Ordering::Relaxed);
                if q_idx >= MAX_THREADS { return; } // Too many threads
                q_ptr = THREAD_QUEUES.add(q_idx);
                libc::pthread_setspecific(key, q_ptr as *mut libc::c_void);
            }
            
            let q = &mut *q_ptr;
            let write_idx = q.write_head;
            let slot_idx = write_idx & MASK;
            let slot = &mut q.slots[slot_idx];
            
            if slot.ready.load(Ordering::Acquire) {
                return; // Queue is full, drop event
            }
            
            ptr::copy_nonoverlapping(msg as *const u8, slot.str_data.as_mut_ptr(), copy_len);
            slot.str1_len = copy_len;
            q.write_head = write_idx + 1;
            slot.ready.store(true, Ordering::Release);
        } else {
            ptr::copy_nonoverlapping(msg as *const u8, buf.as_mut_ptr(), copy_len);
            libc::write(LOG_FD.load(Ordering::Relaxed), buf.as_ptr() as *const libc::c_void, copy_len);
        }
    }
}pub mod hook;
