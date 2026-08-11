pub mod cli;
pub mod injector;
pub mod script;

use std::env;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

pub static CHILD_PID: AtomicU32 = AtomicU32::new(0);

pub extern "C" fn handle_signal(_sig: libc::c_int) {
    let pid = CHILD_PID.load(Ordering::Relaxed);
    if pid > 0 {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    unsafe { libc::_exit(1) }
}

fn main() -> io::Result<()> {
    let parsed_args = cli::args::parse_args();

    let dylib_path = if let Some(ref script_path) = parsed_args.script_file {
        println!("[mtdis] Verifying and compiling safe probe: {}...", script_path);
        match script::compiler::compile_script(Path::new(script_path), parsed_args.legacy_unwind) {
            Ok(path) => {
                println!("[mtdis] Successfully compiled sandboxed probe to {}", path.display());
                path
            }
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    } else if let Some(ref custom_path) = parsed_args.custom_dylib {
        std::path::PathBuf::from(custom_path)
    } else {
        let mut p = env::current_exe()?;
        p.set_file_name("libmactrace_lib.dylib");
        p
    };

    if !dylib_path.exists() {
        eprintln!("[mt] Error: Dylib not found at {}", dylib_path.display());
        if parsed_args.custom_dylib.is_none() {
            eprintln!("[mt] Make sure you built the project with `cargo build`");
        }
        std::process::exit(1);
    }

    if let Some(pid) = parsed_args.target_pid {
        injector::mach::inject_into_pid(pid, &dylib_path);
    } else {
        cli::spawn::spawn_target(parsed_args, &dylib_path)?;
    }

    Ok(())
}
