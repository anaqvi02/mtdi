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


#[repr(align(128))]
pub struct Slot {
    pub timestamp: u64,
    pub formatter: Option<SlotFormatterFn>,
    pub args: [u64; 6],
    pub str1_len: usize,
    pub str2_len: usize,
    pub str_data: [u8; 1024],
    _pad: [u8; 48],
}

/// Formats one captured event into the output buffer (plain / NDJSON / ECS).
pub type SlotFormatterFn = fn(&Slot, bool, bool, &mut core::fmt::Formatter) -> core::fmt::Result;

impl Slot {
    fn get_str1(&self) -> &str {
        core::str::from_utf8(unsafe { core::slice::from_raw_parts(self.str_data.as_ptr(), self.str1_len) }).unwrap_or("<invalid>")
    }

    fn get_str2(&self) -> &str {
        core::str::from_utf8(unsafe {
            core::slice::from_raw_parts(self.str_data.as_ptr().add(self.str1_len), self.str2_len)
        })
        .unwrap_or("<invalid>")
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

#[repr(align(128))]
pub struct ThreadQueue {
    pub ready: [core::sync::atomic::AtomicU8; QUEUE_SIZE],
    pub slots: [Slot; QUEUE_SIZE],
    pub write_head: usize,
    _pad: [u8; 120],
}

static mut THREAD_QUEUES: *mut ThreadQueue = ptr::null_mut();
static ACTIVE_QUEUES: AtomicUsize = AtomicUsize::new(0);
static THREAD_KEY: AtomicUsize = AtomicUsize::new(0);

static LOG_FD: AtomicI32 = AtomicI32::new(2);
static FILTER_MASK: AtomicU32 = AtomicU32::new(0xFFFFFFFF);
static JSON_OUTPUT: AtomicBool = AtomicBool::new(false);
static ECS_OUTPUT: AtomicBool = AtomicBool::new(false);

static USER_ON_OPEN: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INITIALIZE: unsafe extern "C" fn() = {
    unsafe extern "C" fn init() {

        unsafe {
            mach_timebase_info(&raw mut TIMEBASE);
            let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
            libc::gettimeofday(&mut tv, std::ptr::null_mut());
            INIT_TIMEOFDAY_USEC = (tv.tv_sec as u64) * 1_000_000 + (tv.tv_usec as u64);
            INIT_MACH_TIME = mach_absolute_time();
        }

        // Install our inline hooks — all 25 libc syscalls, FastPath style.
        macro_rules! install {
            ($name:literal, $libc:ident, $handler:ident, $tramp:ident) => {
                match hook::manager::install_hook($name, libc::$libc as usize, hook::manager::HookType::FastPath($handler as usize)) {
                    Ok(t) => $tramp.store(t, Ordering::SeqCst),
                    Err(e) => eprintln!("[mtdi] Hook skipped for {}: {}", $name, e),
                }
            };
        }
        install!("open", open, my_open, TRAMP_OPEN);
        install!("close", close, my_close, TRAMP_CLOSE);
        install!("read", read, my_read, TRAMP_READ);
        install!("write", write, my_write, TRAMP_WRITE);
        install!("socket", socket, my_socket, TRAMP_SOCKET);
        install!("connect", connect, my_connect, TRAMP_CONNECT);
        install!("send", send, my_send, TRAMP_SEND);
        install!("recv", recv, my_recv, TRAMP_RECV);
        install!("stat", stat, my_stat, TRAMP_STAT);
        install!("execve", execve, my_execve, TRAMP_EXECVE);
        install!("fork", fork, my_fork, TRAMP_FORK);
        install!("exit", exit, my_exit, TRAMP_EXIT);
        install!("mmap", mmap, my_mmap, TRAMP_MMAP);
        install!("munmap", munmap, my_munmap, TRAMP_MUNMAP);
        install!("unlink", unlink, my_unlink, TRAMP_UNLINK);
        install!("rename", rename, my_rename, TRAMP_RENAME);
        install!("lstat", lstat, my_lstat, TRAMP_LSTAT);
        install!("fstat", fstat, my_fstat, TRAMP_FSTAT);
        install!("bind", bind, my_bind, TRAMP_BIND);
        install!("listen", listen, my_listen, TRAMP_LISTEN);
        install!("accept", accept, my_accept, TRAMP_ACCEPT);
        install!("sendto", sendto, my_sendto, TRAMP_SENDTO);
        install!("recvfrom", recvfrom, my_recvfrom, TRAMP_RECVFROM);
        install!("mkdir", mkdir, my_mkdir, TRAMP_MKDIR);
        install!("rmdir", rmdir, my_rmdir, TRAMP_RMDIR);

        // Optional "swap dylib" override: if MTDI_SWAP_DYLIB is set, dlopen it and
        // load its on_open symbol. When present, my_open forwards to it instead of the
        // real open, letting the swap dylib sandbox/rewrite open() calls.
        let env_swap = c"MTDI_SWAP_DYLIB".as_ptr();
        let swap_ptr = unsafe { libc::getenv(env_swap) };
        if !swap_ptr.is_null() {
            let handle = unsafe { libc::dlopen(swap_ptr, libc::RTLD_LAZY | libc::RTLD_LOCAL) };
            if !handle.is_null() {
                let sym = unsafe { libc::dlsym(handle, c"on_open".as_ptr()) };
                if !sym.is_null() {
                    USER_ON_OPEN.store(sym as *mut c_void, Ordering::Relaxed);
                }
            }
        }

        let env_out = c"MTDI_OUTPUT".as_ptr();
        let out_ptr = unsafe { libc::getenv(env_out) };
        if !out_ptr.is_null() {
            let fd = unsafe {
                libc::open(
                    out_ptr,
                    libc::O_CREAT | libc::O_WRONLY | libc::O_APPEND | libc::O_CLOEXEC,
                    0o666,
                )
            };
            if fd >= 0 {
                LOG_FD.store(fd, Ordering::Relaxed);
            }
        }

        let env_filter = c"MTDI_FILTER".as_ptr();
        let filter_ptr = unsafe { libc::getenv(env_filter) };
        if !filter_ptr.is_null() {
            FILTER_MASK.store(0, Ordering::Relaxed);
            if let Ok(filter_str) = core::str::from_utf8(unsafe { CStr::from_ptr(filter_ptr).to_bytes() }) {
                for s in filter_str.split(',') {
                    match s.trim() {
                        "open" => { FILTER_MASK.fetch_or(1 << 0, Ordering::Relaxed); }
                        "close" => { FILTER_MASK.fetch_or(1 << 1, Ordering::Relaxed); }
                        "read" => { FILTER_MASK.fetch_or(1 << 2, Ordering::Relaxed); }
                        "write" => { FILTER_MASK.fetch_or(1 << 3, Ordering::Relaxed); }
                        "socket" => { FILTER_MASK.fetch_or(1 << 4, Ordering::Relaxed); }
                        "connect" => { FILTER_MASK.fetch_or(1 << 5, Ordering::Relaxed); }
                        "send" => { FILTER_MASK.fetch_or(1 << 6, Ordering::Relaxed); }
                        "recv" => { FILTER_MASK.fetch_or(1 << 7, Ordering::Relaxed); }
                        "stat" => { FILTER_MASK.fetch_or(1 << 8, Ordering::Relaxed); }
                        "execve" => { FILTER_MASK.fetch_or(1 << 9, Ordering::Relaxed); }
                        "fork" => { FILTER_MASK.fetch_or(1 << 10, Ordering::Relaxed); }
                        "exit" => { FILTER_MASK.fetch_or(1 << 11, Ordering::Relaxed); }
                        "mmap" => { FILTER_MASK.fetch_or(1 << 12, Ordering::Relaxed); }
                        "munmap" => { FILTER_MASK.fetch_or(1 << 13, Ordering::Relaxed); }
                        "unlink" => { FILTER_MASK.fetch_or(1 << 14, Ordering::Relaxed); }
                        "rename" => { FILTER_MASK.fetch_or(1 << 15, Ordering::Relaxed); }
                        "lstat" => { FILTER_MASK.fetch_or(1 << 16, Ordering::Relaxed); }
                        "fstat" => { FILTER_MASK.fetch_or(1 << 17, Ordering::Relaxed); }
                        "bind" => { FILTER_MASK.fetch_or(1 << 18, Ordering::Relaxed); }
                        "listen" => { FILTER_MASK.fetch_or(1 << 19, Ordering::Relaxed); }
                        "accept" => { FILTER_MASK.fetch_or(1 << 20, Ordering::Relaxed); }
                        "sendto" => { FILTER_MASK.fetch_or(1 << 21, Ordering::Relaxed); }
                        "recvfrom" => { FILTER_MASK.fetch_or(1 << 22, Ordering::Relaxed); }
                        "mkdir" => { FILTER_MASK.fetch_or(1 << 23, Ordering::Relaxed); }
                        "rmdir" => { FILTER_MASK.fetch_or(1 << 24, Ordering::Relaxed); }
                        _ => {}
                    }
                }
            }
        }

        let env_json = c"MTDI_JSON".as_ptr();
        let json_ptr = unsafe { libc::getenv(env_json) };
        if !json_ptr.is_null() {
            JSON_OUTPUT.store(true, Ordering::Relaxed);
        }

        let env_ecs = c"MTDI_ECS".as_ptr();
        let ecs_ptr = unsafe { libc::getenv(env_ecs) };
        if !ecs_ptr.is_null() {
            ECS_OUTPUT.store(true, Ordering::Relaxed);
        }

        if ECS_OUTPUT.load(Ordering::Relaxed) {
            let msg = b"{\"@timestamp\":\"2000-01-01T00:00:00Z\",\"event\":{\"action\":\"init\"},\"message\":\"mtdi active\"}\n\0";
            unsafe { libc::write(LOG_FD.load(Ordering::Relaxed), msg.as_ptr() as *const c_void, msg.len() - 1); }
        } else if JSON_OUTPUT.load(Ordering::Relaxed) {
            let msg = b"{\"event\":\"mtdi_active\"}\n\0";
            unsafe { libc::write(LOG_FD.load(Ordering::Relaxed), msg.as_ptr() as *const c_void, msg.len() - 1); }
        } else {
            let msg = b"[mtdi] Active! Monitoring system calls...\n\0";
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
                    
                    for (i, head) in read_heads.iter_mut().enumerate().take(active) {
                        let q = &mut *THREAD_QUEUES.add(i);
                        let mut read_idx = *head;
                        
                        for _ in 0..64 { // process up to 64 per queue to ensure fairness
                            let slot_idx = read_idx & MASK;
                            let slot = &mut q.slots[slot_idx];
                            
                            if q.ready[slot_idx].load(Ordering::Acquire) == 0 {
                                break;
                            }
                            idle = false;
                        let is_json = JSON_OUTPUT.load(Ordering::Relaxed);
                        let is_ecs = ECS_OUTPUT.load(Ordering::Relaxed);
                        
                        let delta_mach = slot.timestamp.saturating_sub(INIT_MACH_TIME);
                        let num = TIMEBASE.numer as u64;
                        let den = TIMEBASE.denom as u64;
                        let delta_ns = delta_mach * num / den;
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
                            let _ = writeln!(slice, "{{\"@timestamp\":\"{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z\",{}}}",
                                tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday,
                                tm.tm_hour, tm.tm_min, tm.tm_sec, usec / 1000,
                                formatter
                            );
                        } else if is_json {
                            let h = (sec / 3600) % 24;
                            let m = (sec / 60) % 60;
                            let s = sec % 60;
                            let _ = writeln!(slice, "{{\"timestamp\":\"{:02}:{:02}:{:02}.{:06}\",{}}}", h, m, s, usec, formatter);
                        } else {
                            let h = (sec / 3600) % 24;
                            let m = (sec / 60) % 60;
                            let s = sec % 60;
                            let _ = writeln!(slice, "[{:02}:{:02}:{:02}.{:06}] [mtdi] Caught {}", h, m, s, usec, formatter);
                        }
                        
                        let len = 4096 - slice.len();
                        // Guard against re-entering my_write: the reader's own
                        // output write must never be traced back into itself.
                        READER_WRITING.store(true, Ordering::Relaxed);
                        libc::write(LOG_FD.load(Ordering::Relaxed), buf.as_ptr() as *const libc::c_void, len);
                        READER_WRITING.store(false, Ordering::Relaxed);
                        
                        q.ready[slot_idx].store(0, Ordering::Release);
                        read_idx += 1;
                        }
                        *head = read_idx;
                    }
                    if idle {
                        // Sleep for real when the queue is empty. A tight poll
                        // (100ns) hammers mach_wait_until at ~1M wakeups/sec and
                        // taxes every core on the machine (measured: ~1.5-4x
                        // wall-clock on unrelated work). 1ms drops that to
                        // ~1k/sec (invisible); under sustained event flow the
                        // queue is never empty, so this never adds latency.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
        });
    }
    init
};

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

#[inline(always)]
fn push_binary_event(
    formatter: SlotFormatterFn,
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
            
            if q.ready[slot_idx].load(Ordering::Acquire) != 0 {
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
            q.ready[slot_idx].store(1, Ordering::Release);
        }
    }
}

fn fmt_open(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let path = JsonEscape(s.get_str1());
    let oflag = s.args[0]; let mode = s.args[1];
    // The reader thread opens the outer JSON object ({"timestamp":...), runs this
    // formatter, then closes it and appends the newline.
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"open\"}},\"message\":\"[mtdi] Caught open({}, {}, {})\",\"mtdi\":{{\"path\":\"{}\",\"oflag\":{},\"mode\":{}}}", path, oflag, mode, path, oflag, mode) }
    else if j { write!(f, "\"syscall\":\"open\",\"args\":{{\"path\":\"{}\",\"oflag\":{},\"mode\":{}}}", path, oflag, mode) }
    else { write!(f, "open(\"{}\", {}, {})", s.get_str1(), oflag, mode) }
}

static TRAMP_OPEN: AtomicUsize = AtomicUsize::new(0);

/// FastPath handler for `open(2)`.
///
/// # Safety
/// `path` must be a valid NUL-terminated string for the duration of the call.
/// This is installed as a detour target by the engine and may be invoked with
/// arbitrary register state; it preserves all registers by construction.
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
    // NOTE: on Darwin arm64 the variadic tail of open() (the mode) is passed on the
    // caller's stack, not in x2, so `mode` is only reliable for non-variadic callers
    // (e.g. clang builds that pass it in registers). Cosmetic trace detail only.
    push_binary_event(fmt_open, [oflag as u64, mode as u64, 0, 0, 0, 0], path, core::ptr::null());
    orig_open(path, oflag, mode)
}}

fn fmt_raw(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let msg = JsonEscape(s.get_str1());
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"log\"}},\"message\":\"[mtdi] {}\",\"mtdi\":{{\"log\":\"{}\"}}", msg, msg) }
    else if j { write!(f, "\"event\":\"log\",\"message\":\"{}\"", msg) }
    else { write!(f, "{}", s.get_str1()) }
}

/// Emits a raw log line through the event pipeline.
///
/// # Safety
/// `msg` must be a valid NUL-terminated string for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mtdi_log(msg: *const libc::c_char) {
    if msg.is_null() { return; }
    unsafe {
        if !THREAD_QUEUES.is_null() {
            push_binary_event(fmt_raw, [0, 0, 0, 0, 0, 0], msg, core::ptr::null());
        } else {
            let len = libc::strlen(msg);
            let mut buf = [0u8; 4096];
            let copy_len = core::cmp::min(len, 4096);
            ptr::copy_nonoverlapping(msg as *const u8, buf.as_mut_ptr(), copy_len);
            libc::write(LOG_FD.load(Ordering::Relaxed), buf.as_ptr() as *const libc::c_void, copy_len);
        }
    }
}
// ---------------------------------------------------------------------------
// Built-in FastPath hooks for the remaining libc syscalls.
//
// Every handler follows the my_open shape: atomic trampoline load, filter
// check, one push_binary_event (allocation-free ring-buffer write), then
// forward through the trampoline. All formatting happens on the reader
// thread, so the hot path never allocates or formats.
// ---------------------------------------------------------------------------

/// Set while the reader thread is performing its own output write, so
/// my_write never traces the logger writing to itself.
static READER_WRITING: AtomicBool = AtomicBool::new(false);

fn fmt_close(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let fd = s.args[0] as i32;
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"close\"}},\"message\":\"[mtdi] Caught close({})\",\"mtdi\":{{\"fd\":{}}}", fd, fd) }
    else if j { write!(f, "\"syscall\":\"close\",\"args\":{{\"fd\":{}}}", fd) }
    else { write!(f, "close({})", fd) }
}
fn fmt_read(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (fd, buf, count) = (s.args[0] as i32, s.args[1], s.args[2]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"read\"}},\"message\":\"[mtdi] Caught read({}, {:#x}, {})\",\"mtdi\":{{\"fd\":{},\"buf\":\"{:#x}\",\"count\":{}}}", fd, buf, count, fd, buf, count) }
    else if j { write!(f, "\"syscall\":\"read\",\"args\":{{\"fd\":{},\"buf\":\"{:#x}\",\"count\":{}}}", fd, buf, count) }
    else { write!(f, "read({}, {:#x}, {})", fd, buf, count) }
}
fn fmt_write(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (fd, buf, count) = (s.args[0] as i32, s.args[1], s.args[2]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"write\"}},\"message\":\"[mtdi] Caught write({}, {:#x}, {})\",\"mtdi\":{{\"fd\":{},\"buf\":\"{:#x}\",\"count\":{}}}", fd, buf, count, fd, buf, count) }
    else if j { write!(f, "\"syscall\":\"write\",\"args\":{{\"fd\":{},\"buf\":\"{:#x}\",\"count\":{}}}", fd, buf, count) }
    else { write!(f, "write({}, {:#x}, {})", fd, buf, count) }
}
fn fmt_socket(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (d, ty, p) = (s.args[0], s.args[1], s.args[2]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"socket\"}},\"message\":\"[mtdi] Caught socket({}, {}, {})\",\"mtdi\":{{\"domain\":{},\"type\":{},\"protocol\":{}}}", d, ty, p, d, ty, p) }
    else if j { write!(f, "\"syscall\":\"socket\",\"args\":{{\"domain\":{},\"type\":{},\"protocol\":{}}}", d, ty, p) }
    else { write!(f, "socket({}, {}, {})", d, ty, p) }
}
fn fmt_connect(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (sockfd, addr, len) = (s.args[0], s.args[1], s.args[2]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"connect\"}},\"message\":\"[mtdi] Caught connect({}, {:#x}, {})\",\"mtdi\":{{\"sockfd\":{},\"addr\":\"{:#x}\",\"len\":{}}}", sockfd, addr, len, sockfd, addr, len) }
    else if j { write!(f, "\"syscall\":\"connect\",\"args\":{{\"sockfd\":{},\"addr\":\"{:#x}\",\"len\":{}}}", sockfd, addr, len) }
    else { write!(f, "connect({}, {:#x}, {})", sockfd, addr, len) }
}
fn fmt_send(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (sockfd, buf, len, flags) = (s.args[0], s.args[1], s.args[2], s.args[3]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"send\"}},\"message\":\"[mtdi] Caught send({}, {:#x}, {}, {})\",\"mtdi\":{{\"sockfd\":{},\"buf\":\"{:#x}\",\"len\":{},\"flags\":{}}}", sockfd, buf, len, flags, sockfd, buf, len, flags) }
    else if j { write!(f, "\"syscall\":\"send\",\"args\":{{\"sockfd\":{},\"buf\":\"{:#x}\",\"len\":{},\"flags\":{}}}", sockfd, buf, len, flags) }
    else { write!(f, "send({}, {:#x}, {}, {})", sockfd, buf, len, flags) }
}
fn fmt_recv(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (sockfd, buf, len, flags) = (s.args[0], s.args[1], s.args[2], s.args[3]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"recv\"}},\"message\":\"[mtdi] Caught recv({}, {:#x}, {}, {})\",\"mtdi\":{{\"sockfd\":{},\"buf\":\"{:#x}\",\"len\":{},\"flags\":{}}}", sockfd, buf, len, flags, sockfd, buf, len, flags) }
    else if j { write!(f, "\"syscall\":\"recv\",\"args\":{{\"sockfd\":{},\"buf\":\"{:#x}\",\"len\":{},\"flags\":{}}}", sockfd, buf, len, flags) }
    else { write!(f, "recv({}, {:#x}, {}, {})", sockfd, buf, len, flags) }
}
fn fmt_stat(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let path = JsonEscape(s.get_str1());
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"stat\"}},\"message\":\"[mtdi] Caught stat(\\\"{}\\\")\",\"mtdi\":{{\"path\":\"{}\"}}", path, path) }
    else if j { write!(f, "\"syscall\":\"stat\",\"args\":{{\"path\":\"{}\"}}", path) }
    else { write!(f, "stat(\"{}\")", s.get_str1()) }
}
fn fmt_execve(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (path, argv, envp) = (JsonEscape(s.get_str1()), s.args[1], s.args[2]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"execve\"}},\"message\":\"[mtdi] Caught execve(\\\"{}\\\", {:#x}, {:#x})\",\"mtdi\":{{\"path\":\"{}\",\"argv\":\"{:#x}\",\"envp\":\"{:#x}\"}}", path, argv, envp, path, argv, envp) }
    else if j { write!(f, "\"syscall\":\"execve\",\"args\":{{\"path\":\"{}\",\"argv\":\"{:#x}\",\"envp\":\"{:#x}\"}}", path, argv, envp) }
    else { write!(f, "execve(\"{}\", {:#x}, {:#x})", s.get_str1(), argv, envp) }
}
fn fmt_fork(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let ret = s.args[0];
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"fork\"}},\"message\":\"[mtdi] Caught fork() -> {}\",\"mtdi\":{{\"ret\":{}}}", ret, ret) }
    else if j { write!(f, "\"syscall\":\"fork\",\"args\":{{\"ret\":{}}}", ret) }
    else { write!(f, "fork() -> {}", ret) }
}
fn fmt_exit(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let status = s.args[0] as i32;
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"exit\"}},\"message\":\"[mtdi] Caught exit({})\",\"mtdi\":{{\"status\":{}}}", status, status) }
    else if j { write!(f, "\"syscall\":\"exit\",\"args\":{{\"status\":{}}}", status) }
    else { write!(f, "exit({})", status) }
}
fn fmt_mmap(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (addr, len, prot, flags, fd, offset) = (s.args[0], s.args[1], s.args[2], s.args[3], s.args[4], s.args[5]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"mmap\"}},\"message\":\"[mtdi] Caught mmap({:#x}, {}, {:#x}, {:#x}, {}, {})\",\"mtdi\":{{\"addr\":\"{:#x}\",\"len\":{},\"prot\":\"{:#x}\",\"flags\":\"{:#x}\",\"fd\":{},\"offset\":{}}}", addr, len, prot, flags, fd, offset, addr, len, prot, flags, fd, offset) }
    else if j { write!(f, "\"syscall\":\"mmap\",\"args\":{{\"addr\":\"{:#x}\",\"len\":{},\"prot\":\"{:#x}\",\"flags\":\"{:#x}\",\"fd\":{},\"offset\":{}}}", addr, len, prot, flags, fd, offset) }
    else { write!(f, "mmap({:#x}, {}, {:#x}, {:#x}, {}, {})", addr, len, prot, flags, fd, offset) }
}
fn fmt_munmap(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (addr, len) = (s.args[0], s.args[1]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"munmap\"}},\"message\":\"[mtdi] Caught munmap({:#x}, {})\",\"mtdi\":{{\"addr\":\"{:#x}\",\"len\":{}}}", addr, len, addr, len) }
    else if j { write!(f, "\"syscall\":\"munmap\",\"args\":{{\"addr\":\"{:#x}\",\"len\":{}}}", addr, len) }
    else { write!(f, "munmap({:#x}, {})", addr, len) }
}
fn fmt_unlink(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let path = JsonEscape(s.get_str1());
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"unlink\"}},\"message\":\"[mtdi] Caught unlink(\\\"{}\\\")\",\"mtdi\":{{\"path\":\"{}\"}}", path, path) }
    else if j { write!(f, "\"syscall\":\"unlink\",\"args\":{{\"path\":\"{}\"}}", path) }
    else { write!(f, "unlink(\"{}\")", s.get_str1()) }
}
fn fmt_rename(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (old, new) = (JsonEscape(s.get_str1()), JsonEscape(s.get_str2()));
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"rename\"}},\"message\":\"[mtdi] Caught rename(\\\"{}\\\", \\\"{}\\\")\",\"mtdi\":{{\"old\":\"{}\",\"new\":\"{}\"}}", old, new, old, new) }
    else if j { write!(f, "\"syscall\":\"rename\",\"args\":{{\"old\":\"{}\",\"new\":\"{}\"}}", old, new) }
    else { write!(f, "rename(\"{}\", \"{}\")", s.get_str1(), s.get_str2()) }
}
fn fmt_lstat(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let path = JsonEscape(s.get_str1());
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"lstat\"}},\"message\":\"[mtdi] Caught lstat(\\\"{}\\\")\",\"mtdi\":{{\"path\":\"{}\"}}", path, path) }
    else if j { write!(f, "\"syscall\":\"lstat\",\"args\":{{\"path\":\"{}\"}}", path) }
    else { write!(f, "lstat(\"{}\")", s.get_str1()) }
}
fn fmt_fstat(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let fd = s.args[0] as i32;
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"fstat\"}},\"message\":\"[mtdi] Caught fstat({})\",\"mtdi\":{{\"fd\":{}}}", fd, fd) }
    else if j { write!(f, "\"syscall\":\"fstat\",\"args\":{{\"fd\":{}}}", fd) }
    else { write!(f, "fstat({})", fd) }
}
fn fmt_bind(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (sockfd, addr, len) = (s.args[0], s.args[1], s.args[2]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"bind\"}},\"message\":\"[mtdi] Caught bind({}, {:#x}, {})\",\"mtdi\":{{\"sockfd\":{},\"addr\":\"{:#x}\",\"len\":{}}}", sockfd, addr, len, sockfd, addr, len) }
    else if j { write!(f, "\"syscall\":\"bind\",\"args\":{{\"sockfd\":{},\"addr\":\"{:#x}\",\"len\":{}}}", sockfd, addr, len) }
    else { write!(f, "bind({}, {:#x}, {})", sockfd, addr, len) }
}
fn fmt_listen(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (sockfd, backlog) = (s.args[0], s.args[1]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"listen\"}},\"message\":\"[mtdi] Caught listen({}, {})\",\"mtdi\":{{\"sockfd\":{},\"backlog\":{}}}", sockfd, backlog, sockfd, backlog) }
    else if j { write!(f, "\"syscall\":\"listen\",\"args\":{{\"sockfd\":{},\"backlog\":{}}}", sockfd, backlog) }
    else { write!(f, "listen({}, {})", sockfd, backlog) }
}
fn fmt_accept(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (sockfd, addr, addrlen) = (s.args[0], s.args[1], s.args[2]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"accept\"}},\"message\":\"[mtdi] Caught accept({}, {:#x}, {:#x})\",\"mtdi\":{{\"sockfd\":{},\"addr\":\"{:#x}\",\"addrlen\":\"{:#x}\"}}", sockfd, addr, addrlen, sockfd, addr, addrlen) }
    else if j { write!(f, "\"syscall\":\"accept\",\"args\":{{\"sockfd\":{},\"addr\":\"{:#x}\",\"addrlen\":\"{:#x}\"}}", sockfd, addr, addrlen) }
    else { write!(f, "accept({}, {:#x}, {:#x})", sockfd, addr, addrlen) }
}
fn fmt_sendto(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (sockfd, buf, len, flags, dest, dest_len) = (s.args[0], s.args[1], s.args[2], s.args[3], s.args[4], s.args[5]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"sendto\"}},\"message\":\"[mtdi] Caught sendto({}, {:#x}, {}, {}, {:#x}, {})\",\"mtdi\":{{\"sockfd\":{},\"buf\":\"{:#x}\",\"len\":{},\"flags\":{},\"dest\":\"{:#x}\",\"dest_len\":{}}}", sockfd, buf, len, flags, dest, dest_len, sockfd, buf, len, flags, dest, dest_len) }
    else if j { write!(f, "\"syscall\":\"sendto\",\"args\":{{\"sockfd\":{},\"buf\":\"{:#x}\",\"len\":{},\"flags\":{},\"dest\":\"{:#x}\",\"dest_len\":{}}}", sockfd, buf, len, flags, dest, dest_len) }
    else { write!(f, "sendto({}, {:#x}, {}, {}, {:#x}, {})", sockfd, buf, len, flags, dest, dest_len) }
}
fn fmt_recvfrom(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (sockfd, buf, len, flags, src, src_len) = (s.args[0], s.args[1], s.args[2], s.args[3], s.args[4], s.args[5]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"recvfrom\"}},\"message\":\"[mtdi] Caught recvfrom({}, {:#x}, {}, {}, {:#x}, {:#x})\",\"mtdi\":{{\"sockfd\":{},\"buf\":\"{:#x}\",\"len\":{},\"flags\":{},\"src\":\"{:#x}\",\"src_len\":\"{:#x}\"}}", sockfd, buf, len, flags, src, src_len, sockfd, buf, len, flags, src, src_len) }
    else if j { write!(f, "\"syscall\":\"recvfrom\",\"args\":{{\"sockfd\":{},\"buf\":\"{:#x}\",\"len\":{},\"flags\":{},\"src\":\"{:#x}\",\"src_len\":\"{:#x}\"}}", sockfd, buf, len, flags, src, src_len) }
    else { write!(f, "recvfrom({}, {:#x}, {}, {}, {:#x}, {:#x})", sockfd, buf, len, flags, src, src_len) }
}
fn fmt_mkdir(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let (path, mode) = (JsonEscape(s.get_str1()), s.args[1]);
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"mkdir\"}},\"message\":\"[mtdi] Caught mkdir(\\\"{}\\\", {:#o})\",\"mtdi\":{{\"path\":\"{}\",\"mode\":\"{:#o}\"}}", path, mode, path, mode) }
    else if j { write!(f, "\"syscall\":\"mkdir\",\"args\":{{\"path\":\"{}\",\"mode\":\"{:#o}\"}}", path, mode) }
    else { write!(f, "mkdir(\"{}\", {:#o})", s.get_str1(), mode) }
}
fn fmt_rmdir(s: &Slot, j: bool, e: bool, f: &mut core::fmt::Formatter) -> core::fmt::Result {
    let path = JsonEscape(s.get_str1());
    if e { write!(f, "\"event\":{{\"category\":[\"process\"],\"action\":\"rmdir\"}},\"message\":\"[mtdi] Caught rmdir(\\\"{}\\\")\",\"mtdi\":{{\"path\":\"{}\"}}", path, path) }
    else if j { write!(f, "\"syscall\":\"rmdir\",\"args\":{{\"path\":\"{}\"}}", path) }
    else { write!(f, "rmdir(\"{}\")", s.get_str1()) }
}

/// Generates a FastPath handler in the my_open shape: trampoline load,
/// filter check, one push_binary_event, then forward through the trampoline.
macro_rules! fastpath_hook {
    (
        $tramp:ident, $handler:ident, $bit:expr, $ret:ty, $fmt:ident,
        args = [$($pack:expr),* $(,)?],
        strings = ($s1:expr, $s2:expr),
        $($arg:ident: $ty:ty),* $(,)?
    ) => {
        static $tramp: AtomicUsize = AtomicUsize::new(0);

        /// # Safety
        /// Installed as a detour target by the engine; forwards via `$tramp`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $handler($($arg: $ty),*) -> $ret {
            unsafe {
                let tramp = $tramp.load(Ordering::Relaxed);
                let orig: unsafe extern "C" fn($($ty),*) -> $ret = core::mem::transmute(tramp);
                if !should_log($bit) {
                    return orig($($arg),*);
                }
                push_binary_event($fmt, [$($pack),*], $s1, $s2);
                orig($($arg),*)
            }
        }
    };
}

fastpath_hook!(TRAMP_CLOSE, my_close, 1, c_int, fmt_close,
    args = [fd as u64, 0, 0, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    fd: c_int);
fastpath_hook!(TRAMP_READ, my_read, 2, isize, fmt_read,
    args = [fd as u64, buf as u64, count as u64, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    fd: c_int, buf: *mut c_void, count: usize);
fastpath_hook!(TRAMP_SOCKET, my_socket, 4, c_int, fmt_socket,
    args = [domain as u64, ty as u64, protocol as u64, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    domain: c_int, ty: c_int, protocol: c_int);
fastpath_hook!(TRAMP_CONNECT, my_connect, 5, c_int, fmt_connect,
    args = [socket as u64, address as u64, len as u64, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    socket: c_int, address: *const libc::sockaddr, len: libc::socklen_t);
fastpath_hook!(TRAMP_SEND, my_send, 6, isize, fmt_send,
    args = [socket as u64, buf as u64, len as u64, flags as u64, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    socket: c_int, buf: *const c_void, len: usize, flags: c_int);
fastpath_hook!(TRAMP_RECV, my_recv, 7, isize, fmt_recv,
    args = [socket as u64, buf as u64, len as u64, flags as u64, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    socket: c_int, buf: *mut c_void, len: usize, flags: c_int);
fastpath_hook!(TRAMP_STAT, my_stat, 8, c_int, fmt_stat,
    args = [path as u64, buf as u64, 0, 0, 0, 0],
    strings = (path, core::ptr::null()),
    path: *const c_char, buf: *mut libc::stat);
fastpath_hook!(TRAMP_EXECVE, my_execve, 9, c_int, fmt_execve,
    args = [path as u64, argv as u64, envp as u64, 0, 0, 0],
    strings = (path, core::ptr::null()),
    path: *const c_char, argv: *const *const c_char, envp: *const *const c_char);
fastpath_hook!(TRAMP_MMAP, my_mmap, 12, *mut c_void, fmt_mmap,
    args = [addr as u64, len as u64, prot as u64, flags as u64, fd as u64, offset as u64],
    strings = (core::ptr::null(), core::ptr::null()),
    addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, offset: libc::off_t);
fastpath_hook!(TRAMP_MUNMAP, my_munmap, 13, c_int, fmt_munmap,
    args = [addr as u64, len as u64, 0, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    addr: *mut c_void, len: usize);
fastpath_hook!(TRAMP_UNLINK, my_unlink, 14, c_int, fmt_unlink,
    args = [path as u64, 0, 0, 0, 0, 0],
    strings = (path, core::ptr::null()),
    path: *const c_char);
fastpath_hook!(TRAMP_RENAME, my_rename, 15, c_int, fmt_rename,
    args = [old as u64, new as u64, 0, 0, 0, 0],
    strings = (old, new),
    old: *const c_char, new: *const c_char);
fastpath_hook!(TRAMP_LSTAT, my_lstat, 16, c_int, fmt_lstat,
    args = [path as u64, buf as u64, 0, 0, 0, 0],
    strings = (path, core::ptr::null()),
    path: *const c_char, buf: *mut libc::stat);
fastpath_hook!(TRAMP_FSTAT, my_fstat, 17, c_int, fmt_fstat,
    args = [fildes as u64, buf as u64, 0, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    fildes: c_int, buf: *mut libc::stat);
fastpath_hook!(TRAMP_BIND, my_bind, 18, c_int, fmt_bind,
    args = [socket as u64, address as u64, address_len as u64, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    socket: c_int, address: *const libc::sockaddr, address_len: libc::socklen_t);
fastpath_hook!(TRAMP_LISTEN, my_listen, 19, c_int, fmt_listen,
    args = [socket as u64, backlog as u64, 0, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    socket: c_int, backlog: c_int);
fastpath_hook!(TRAMP_ACCEPT, my_accept, 20, c_int, fmt_accept,
    args = [socket as u64, address as u64, address_len as u64, 0, 0, 0],
    strings = (core::ptr::null(), core::ptr::null()),
    socket: c_int, address: *mut libc::sockaddr, address_len: *mut libc::socklen_t);
fastpath_hook!(TRAMP_SENDTO, my_sendto, 21, isize, fmt_sendto,
    args = [socket as u64, buf as u64, len as u64, flags as u64, to as u64, tolen as u64],
    strings = (core::ptr::null(), core::ptr::null()),
    socket: c_int, buf: *const c_void, len: usize, flags: c_int, to: *const libc::sockaddr, tolen: libc::socklen_t);
fastpath_hook!(TRAMP_RECVFROM, my_recvfrom, 22, isize, fmt_recvfrom,
    args = [socket as u64, buf as u64, len as u64, flags as u64, from as u64, fromlen as u64],
    strings = (core::ptr::null(), core::ptr::null()),
    socket: c_int, buf: *mut c_void, len: usize, flags: c_int, from: *mut libc::sockaddr, fromlen: *mut libc::socklen_t);
fastpath_hook!(TRAMP_MKDIR, my_mkdir, 23, c_int, fmt_mkdir,
    args = [path as u64, mode as u64, 0, 0, 0, 0],
    strings = (path, core::ptr::null()),
    path: *const c_char, mode: libc::mode_t);
fastpath_hook!(TRAMP_RMDIR, my_rmdir, 24, c_int, fmt_rmdir,
    args = [path as u64, 0, 0, 0, 0, 0],
    strings = (path, core::ptr::null()),
    path: *const c_char);

static TRAMP_WRITE: AtomicUsize = AtomicUsize::new(0);

/// FastPath handler for `write(2)`.
///
/// # Safety
/// Installed as a detour target by the engine; forwards via `TRAMP_WRITE`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_write(fd: c_int, buf: *const c_void, count: usize) -> isize {
    unsafe {
        let tramp = TRAMP_WRITE.load(Ordering::Relaxed);
        let orig: unsafe extern "C" fn(c_int, *const c_void, usize) -> isize = core::mem::transmute(tramp);
        // Never trace the logger writing to itself (feedback loop).
        if !should_log(3) || READER_WRITING.load(Ordering::Relaxed) {
            return orig(fd, buf, count);
        }
        push_binary_event(fmt_write, [fd as u64, buf as u64, count as u64, 0, 0, 0], core::ptr::null(), core::ptr::null());
        orig(fd, buf, count)
    }
}

static TRAMP_FORK: AtomicUsize = AtomicUsize::new(0);

/// FastPath handler for `fork(2)`.
///
/// # Safety
/// Installed as a detour target by the engine; forwards via `TRAMP_FORK`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_fork() -> libc::pid_t {
    unsafe {
        let tramp = TRAMP_FORK.load(Ordering::Relaxed);
        let orig: unsafe extern "C" fn() -> libc::pid_t = core::mem::transmute(tramp);
        let pid = orig();
        // Log in the parent only: the child inherits a COW copy of the ring
        // buffers but no reader thread to drain them.
        if pid > 0 && should_log(10) {
            push_binary_event(fmt_fork, [pid as u64, 0, 0, 0, 0, 0], core::ptr::null(), core::ptr::null());
        }
        pid
    }
}

static TRAMP_EXIT: AtomicUsize = AtomicUsize::new(0);

/// FastPath handler for `exit(2)`.
///
/// # Safety
/// Installed as a detour target by the engine; forwards via `TRAMP_EXIT`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn my_exit(status: c_int) -> ! {
    unsafe {
        if should_log(11) {
            push_binary_event(fmt_exit, [status as u64, 0, 0, 0, 0, 0], core::ptr::null(), core::ptr::null());
        }
        let tramp = TRAMP_EXIT.load(Ordering::Relaxed);
        let orig: unsafe extern "C" fn(c_int) -> ! = core::mem::transmute(tramp);
        orig(status)
    }
}
pub mod hook;
