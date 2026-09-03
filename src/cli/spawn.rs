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
    // disable-library-validation also lifts dyld env stripping for
    // hardened bins (e.g. spotify); both count
    stdout.contains("com.apple.security.cs.allow-dyld-environment-variables")
        || stdout.contains("com.apple.security.cs.disable-library-validation")
}

/// hard-fail when launch injection is impossible (sip on + apple binary,
/// or hardened without dyld entitlement): silent dyld env stripping
/// would trace with zero hooks and the user would never know
fn check_sip_and_codesign(binary_path: &str) -> Result<(), String> {
    // arm64e binaries refuse any arm64 dylib at dyld load time (SIGABRT),
    // independent of SIP: flag it before spawning so the user gets a reason
    let file_out = Command::new("file")
        .arg("-b")
        .arg(binary_path)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    if file_out.contains("arm64e") {
        return Err(format!(
            "'{}' is an arm64e binary: dyld refuses to load a standard arm64 dylib into it (Apple restricts PAC-enabled arm64e to their own components).",
            binary_path
        ));
    }

    if !is_sip_enabled() {
        return Ok(());
    }

    if binary_path.starts_with("/bin/")
        || binary_path.starts_with("/usr/bin/")
        || binary_path.starts_with("/sbin/")
        || binary_path.starts_with("/usr/sbin/")
        || binary_path.starts_with("/System/")
    {
        return Err(format!(
            "SIP is enabled and '{}' is an Apple system binary: DYLD_INSERT_LIBRARIES is stripped, launch-time injection cannot work.",
            binary_path
        ));
    }

    if has_hardened_runtime(binary_path) && !has_dyld_entitlement(binary_path) {
        return Err(format!(
            "'{}' enforces the Hardened Runtime without allow-dyld-environment-variables or disable-library-validation: DYLD_INSERT_LIBRARIES will be stripped.",
            binary_path
        ));
    }
    Ok(())
}

pub fn spawn_target(args: AppArgs, dylib_path: &Path) -> std::io::Result<()> {
    let cmd_name = args.cmd_name.unwrap();
    if let Err(msg) = check_sip_and_codesign(&cmd_name) {
        eprintln!("[mtdi] {}", msg);
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, msg));
    }

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
    // launch mode: sigterm drains the ring instead of truncating the trace
    cmd.env("MTDI_OWN_SIGTERM", "1");

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
