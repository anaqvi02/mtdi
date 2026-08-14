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

        // Install our inline hooks
        if let Ok(tramp_addr) = hook::manager::install_hook("open", libc::open as usize, hook::manager::HookType::FastPath(my_open as usize)) {
            TRAMP_OPEN.store(tramp_addr, Ordering::SeqCst);
        } else {
            eprintln!("[mtdi] Failed to install hook for open!");
        }

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
                    if s.trim() == "open" { FILTER_MASK.fetch_or(1 << 0, Ordering::Relaxed); }
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
                        libc::write(LOG_FD.load(Ordering::Relaxed), buf.as_ptr() as *const libc::c_void, len);
                        
                        q.ready[slot_idx].store(0, Ordering::Release);
                        read_idx += 1;
                        }
                        *head = read_idx;
                    }
                    if idle {
                        std::thread::sleep(std::time::Duration::from_nanos(100));
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
pub mod hook;
