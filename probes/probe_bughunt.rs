// flags anomalous syscall args in a target app
// pre-call hooks: return values not visible, so all signatures are arg-side:
//   - repeated opens (fd leak / redundant io)
//   - writes into /Applications
//   - O_TRUNC on config paths, O_CREAT with mode 0
//   - stat storms (enoent hunting)
//   - send/recv on fd < 3, zero-length sends
//   - exit status != 0
// logs to /tmp/mtdi_bughunt_<tag>.log; open outside the lock,
// write inside, re-entrancy guard (our own open() re-fires the hook)

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

const EVENT_CAP: usize = 20000;

static LOG: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static CREATING: AtomicBool = AtomicBool::new(false);
static EVENTS: AtomicUsize = AtomicUsize::new(0);
static OPEN_COUNTS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
static STAT_COUNTS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn log_line(line: String) {
    if EVENTS.fetch_add(1, Ordering::Relaxed) >= EVENT_CAP {
        return;
    }
    // guard: our own open() below re-fires on_open
    if CREATING.swap(true, Ordering::SeqCst) {
        return;
    }
    let f = LOG.get_or_init(|| Mutex::new(None));
    // open outside the lock
    let needs_open = {
        let g = f.lock();
        match g {
            Ok(g2) => g2.is_none(),
            Err(_) => false,
        }
    };
    if needs_open {
        let res = OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/mtdi_bughunt_vlc.log");
        if let Ok(fh) = res {
            let g = f.lock();
            if let Ok(mut g2) = g {
                if g2.is_none() {
                    *g2 = Some(fh);
                }
            }
        }
    }
    // write inside the lock
    let g = f.lock();
    if let Ok(mut g2) = g {
        if let Some(fh) = g2.as_mut() {
            let _ = writeln!(fh, "{}", line);
            let _ = fh.flush();
        }
    }
    CREATING.store(false, Ordering::SeqCst);
}

fn bump_count(m: &OnceLock<Mutex<HashMap<String, u64>>>, key: String) -> u64 {
    let map = m.get_or_init(|| Mutex::new(HashMap::new()));
    let g = map.lock();
    match g {
        Ok(mut g2) => {
            let e = g2.entry(key).or_insert(0u64);
            *e = *e + 1;
            *e
        }
        Err(_) => 0,
    }
}

pub fn on_open(ctx: &mut MtdiSafeContext) {
    let flags = ctx.arg(1);
    let mode = ctx.arg(2);
    if let Some(path) = ctx.read_arg_str(0, 512) {
        let access = flags & 3;
        let is_write = access != 0;
        let is_trunc = (flags & 0x400) != 0;
        let is_create = (flags & 0x200) != 0;
        let mut note = String::new();
        if is_write && path.contains("/Applications/") {
            note.push_str(" WRITE-TO-BUNDLE");
        }
        if is_trunc {
            note.push_str(" O_TRUNC");
        }
        if is_create && mode == 0 {
            note.push_str(" CREATE-MODE-0");
        }
        if path.contains("//") {
            note.push_str(" DOUBLE-SLASH");
        }
        let n = bump_count(&OPEN_COUNTS, path.clone());
        let line = if n == 2 {
            format!(
                "[open] flags={:#x} mode={:#o} \"{}\"{}  <-- REPEAT-OPEN (2nd time)",
                flags, mode, path, note
            )
        } else {
            format!(
                "[open] flags={:#x} mode={:#o} \"{}\"{}",
                flags, mode, path, note
            )
        };
        log_line(line);
    }
}

pub fn on_stat(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 512) {
        let n = bump_count(&STAT_COUNTS, path.clone());
        if n == 1 || n == 10 || n == 100 || n == 1000 || n == 10000 {
            log_line(format!("[stat] \"{}\" (seen {} times)", path, n));
        }
    }
}

pub fn on_lstat(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 512) {
        let n = bump_count(&STAT_COUNTS, path.clone());
        if n == 1 || n == 10 || n == 100 || n == 1000 || n == 10000 {
            log_line(format!("[lstat] \"{}\" (seen {} times)", path, n));
        }
    }
}

pub fn on_fstat(ctx: &mut MtdiSafeContext) {
    let fd = ctx.arg(0);
    if fd < 3 {
        log_line(format!("[fstat] fd={} (stdio fd)", fd));
    }
}

pub fn on_send(ctx: &mut MtdiSafeContext) {
    let fd = ctx.arg(0);
    let len = ctx.arg(2);
    let flags = ctx.arg(3);
    let mut note = String::new();
    if fd < 3 {
        note.push_str(" STDIO-FD");
    }
    if len == 0 {
        note.push_str(" ZERO-LEN");
    }
    log_line(format!(
        "[send] fd={} len={} flags={:#x}{}",
        fd, len, flags, note
    ));
}

pub fn on_recv(ctx: &mut MtdiSafeContext) {
    let fd = ctx.arg(0);
    let len = ctx.arg(2);
    let flags = ctx.arg(3);
    let mut note = String::new();
    if fd < 3 {
        note.push_str(" STDIO-FD");
    }
    if len == 0 {
        note.push_str(" ZERO-LEN");
    }
    log_line(format!(
        "[recv] fd={} len={} flags={:#x}{}",
        fd, len, flags, note
    ));
}

pub fn on_fork(ctx: &mut MtdiSafeContext) {
    log_line("[fork]".to_string());
}

pub fn on_exit(ctx: &mut MtdiSafeContext) {
    let status = ctx.arg(0);
    log_line(format!("[exit] status={}", status));
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("open", on_open);
    reg.hook_symbol("stat", on_stat);
    reg.hook_symbol("lstat", on_lstat);
    reg.hook_symbol("fstat", on_fstat);
    reg.hook_symbol("send", on_send);
    reg.hook_symbol("recv", on_recv);
    reg.hook_symbol("fork", on_fork);
    reg.hook_symbol("exit", on_exit);
}
