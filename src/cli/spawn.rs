use std::process::Command;
use std::path::Path;
use std::sync::atomic::Ordering;

use crate::cli::args::AppArgs;
use crate::CHILD_PID;
use crate::handle_signal;

fn is_sip_enabled() -> bool {
    let output = Command::new("csrutil").arg("status").output().unwrap_or_else(|_| {
        Command::new("true").output().unwrap()
    });
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains("enabled")
}

fn has_hardened_runtime(binary_path: &str) -> bool {
    let output = Command::new("codesign")
        .args(["-dvv", binary_path])
        .output()
        .unwrap_or_else(|_| Command::new("true").output().unwrap());
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("flags=0x10000(runtime)")
}

fn has_dyld_entitlement(binary_path: &str) -> bool {
    let output = Command::new("codesign")
        .args(["-d", "--entitlements", ":-", binary_path])
        .output()
        .unwrap_or_else(|_| Command::new("true").output().unwrap());
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains("com.apple.security.cs.allow-dyld-environment-variables")
}

fn check_sip_and_codesign(binary_path: &str) {
    if !is_sip_enabled() { return; }
    
    if binary_path.starts_with("/bin/") || binary_path.starts_with("/usr/bin/") || binary_path.starts_with("/sbin/") || binary_path.starts_with("/usr/sbin/") || binary_path.starts_with("/System/") {
        eprintln!("[mtdi] Oh look, SIP is enabled and you're trying to inject into an Apple system binary. Good luck with that.");
        return;
    }
    
    if has_hardened_runtime(binary_path) && !has_dyld_entitlement(binary_path) {
        eprintln!("[mtdi] Oh look, SIP is enabled and this binary enforces the Hardened Runtime. Good luck with that.");
    }
}

pub fn spawn_target(args: AppArgs, dylib_path: &Path) -> std::io::Result<()> {
    let cmd_name = args.cmd_name.unwrap();
    check_sip_and_codesign(&cmd_name);

    let mut cmd = Command::new(&cmd_name);
    cmd.args(&args.cmd_args);
    cmd.env("DYLD_INSERT_LIBRARIES", dylib_path);

    if let Some(out) = args.output_file {
        cmd.env("MTDI_OUTPUT", out);
    }
    if let Some(filter) = args.trace_filter {
        cmd.env("MTDI_FILTER", filter);
    }
    if args.json_output {
        cmd.env("MTDI_JSON", "1");
    }
    if args.ecs_output {
        cmd.env("MTDI_ECS", "1");
    }

    let mut child = cmd.spawn()?;
    CHILD_PID.store(child.id(), Ordering::Relaxed);

    unsafe {
        libc::signal(libc::SIGINT, handle_signal as usize);
        libc::signal(libc::SIGTERM, handle_signal as usize);
    }

    let status = child.wait()?;
    CHILD_PID.store(0, Ordering::Relaxed);

    if status.success() {
        println!("[mtdi] Command '{}' finished successfully!", cmd_name);
    } else {
        println!("[mtdi] Command '{}' exited with status: {}", cmd_name, status);
    }

    Ok(())
}
