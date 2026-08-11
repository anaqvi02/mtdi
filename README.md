<img width="1280" height="168" alt="mtrace_banner" src="https://github.com/user-attachments/assets/f1b6d475-65a0-4213-ae22-281cc97a0fe1" />

<img width="1280" height="170" alt="b" src="https://github.com/user-attachments/assets/02d275e5-ec9f-4d85-ae85-eb27687be8cd" />

---

`mtrace` (aka `mt`,`mactrace`) is a high-speed, zero-privilege, user-space system call tracer for macOS. 

Unlike Apple's native `dtruss` which requires disabling System Integrity Protection (SIP) and running as root, `mtrace` intercepts libc calls entirely in user-space: `DYLD_INSERT_LIBRARIES` loads a dylib which installs inline hooks (AArch64 detours with instruction relocation) on the target functions.

If you are a reverse engineer, malware analyst, or just want to debug a crashing application, `mtrace` gives you unparalleled visibility and control over what a process is doing, without ever touching your system's security settings.

*This technically isnt a "system call" tracer, and instead traces libc/api calls. Close enough, though. Most applications use the libc api.

## Features
- **Zero Sudo required:** Run it instantly as a standard user.
- **Microsecond Timestamps:** Accurately measure network latency and disk I/O.
- **Fast Filtering:** Use `-t` to seamlessly bypass the logging of noisy syscalls.
- **Active Manipulation:** Because it intercepts calls in user-space, you can freely edit the hooks to block telemetry, bypass license checks, or spoof network traffic. Ex: Very easy to implement TOCTOU exploits.

## A small non-comprehensive list of notable cases that mt works on (verified by me)

> [!IMPORTANT]  
> **System Integrity Protection (SIP) Note:** If your Mac has SIP **enabled**, macOS will automatically block `mtrace` from injecting into any code-signed application that uses the Hardened Runtime (which includes almost all of the apps below). 
> 
> To trace these apps, **you must disable System Integrity Protection (SIP)** natively (`csrutil disable` in Recovery Mode). `mtrace` will warn you if it detects you are trying to trace a protected application with SIP enabled.

- Steam 
- Blender 
- Postgres databases 
- Unity 
- Firefox 
- Chrome / Chromium-based browsers 
- Microsoft Word 
- Zoom 
- JetBrains IDEs (IntelliJ, PyCharm, WebStorm, etc.)
- GarageBand (suprising, but this one makes sense, because it needs to be able to load extentions)
- Minecraft (using standard x86_64/arm64 Java, not default arm64e) (launcher and game itself)
- Spotify 
- VLC Media Player 
- OBS Studio 
- Adobe Photoshop / Adobe CC apps 
- Ableton Live 
- Docker Desktop 
- iTerm2 
- Sublime Text 
- TablePlus 
- Epic Games Launcher 
- Dropbox 
- Telegram
- Desktop Homebrew CLI tools (wget, ffmpeg, etc.)
- Locally compiled development binaries 
- Scripting interpreters (Python, Node.js, etc.)
- Programs written in anylanguage (except raw assembly/direct kernel syscalls)
- kind of deviously powerful. i didnt even build it for this purpose.


## Why should you use this?
- works w/o disabling sip and no sudo required (unlike dtrace/dtruss)
- its fast and purpose-built (see below) (unlike Frida) (this cant do 99% of what Frida does, but this is a lot faster for this one purpose)
- dead simple to modify (see examples/swap.rs)
- dead simple to use

## Why should you NOT use this?
- cannot trace Apple-signed system binaries or `arm64e` apps (blocked by SIP)
- cannot inspect internal memory or CPU registers (unlike Frida or QBDI); probe hooks are limited to exported symbols
- can be bypassed by things that executes raw assembly syscalls (`svc 0x80`) instead of calling `libc`
- only tracks the explicit libc calls it hooks (unlike `dtruss` which automatically catches everything crossing the kernel boundary)
- doesnt do a whole lot except for what its built to do

## Quick Start

### 1. Build
Make sure you have Rust installed, then compile the project:
```bash
cargo build --release
```

### 2. Usage
Run any standard `arm64` macOS application under the tracer:

```bash
# Basic usage
./target/release/mtrace python3 -c "print('hello')"

# Filter: only log open() calls
./target/release/mtrace -t open ./my_binary

# Write logs to a file instead of stderr
./target/release/mtrace -o trace.log ./my_binary

# Output logs in NDJSON or Elastic Common Schema (ECS) format for SIEM ingestion
./target/release/mtrace -j -o trace.json ./my_binary
./target/release/mtrace -e -o ecs_trace.json ./my_binary
```

## Dynamic Instrumentation
`mtrace` ships a **Dynamic Injection Engine** (`mtdis`). You can write a Rust probe that hooks functions in the traced process to log, mutate, or spoof their behavior. *(Note: This requires `rustc` to be installed on your system).*

The engine verifies your probe's AST (rejecting `unwrap()`/`panic!`/raw indexing for the Zero-Panic build), JIT-compiles it with `-C panic=abort`, and injects a self-contained dylib that installs its own inline hooks.

Probes use the `MtdiSafeContext` / `MtdiRegistry` API — see `examples/probe_open.rs` for a working template:
```bash
./target/release/mtrace -s examples/probe_open.rs ./your_binary
```

### Swapping open()
You can also replace `open()` entirely with your own implementation. Compile `examples/swap.rs` to a dylib and set `MTRACE_SWAP_DYLIB`; the engine calls its `on_open` instead of the real libc `open`. The template forwards via raw `syscall`, so it can't recurse back into the hook:
```bash
rustc --edition=2021 --crate-type cdylib -O examples/swap.rs -o swap.dylib
MTRACE_SWAP_DYLIB=$PWD/swap.dylib ./target/release/mtrace ./your_binary
```

## What Can (and Cannot) Be Traced
Apple's System Integrity Protection (SIP) creates a hard boundary around core OS components. Here is a quick cheat sheet on what you can and cannot trace:

### Cannot Be Traced
There are three main categories of executables that `mtrace` cannot touch:

1. **System Utilities (Blocked by SIP):** Any core Apple-signed tool in protected directories (`/bin/ls`, `/bin/cat`, `/usr/bin/curl`).
2. **`arm64e` Binaries:** Apple strictly restricts the `arm64e` architecture (which uses Pointer Authentication Codes) to their own first-party OS components. If you encounter a rare third-party app (like Spotify) that ships an `arm64e` binary, `dyld` will refuse to load our standard `arm64` tracer into it. 
*(Error signature: `terminating because inserted dylib ... incompatible architecture (have 'arm64', need 'arm64e')`)*
3. **Strict Hardened Runtime:** Apps from the Mac App Store with "Library Validation" strictly enforced will block the tracer. However, unlike the first two categories, you can bypass this by simply removing the signature (`codesign --remove-signature <app>`).

### Can Be Traced (Standard `arm64`)
Any third-party software, developer tool, or custom script that is standard `arm64` and lacks strict Library Validation will work perfectly.
- **Homebrew Packages:** `/opt/homebrew/bin/python3`, `/opt/homebrew/bin/curl`, `wget`, `ffmpeg`, `nmap`
- **Developer Runtimes:** Python (`python3 script.py`), Node.js (`node index.js`), compiled C/Rust binaries (`./victim`)
- **Third-Party Applications:** Steam, Discord, VS Code (many large Electron and game apps disable Library Validation out of the box).
- **Basically anything that you might want to run this on works.**

## Tracked Calls
The default dylib currently hooks one function: `open`. Filter with `-t open` (or `-t asdf` to disable logging entirely).

The hook engine — FastPath handlers, full-context thunks, instruction relocation — supports any function: the probe engine (`-s`) can hook any exported symbol, and a new built-in hook is one `install_hook` call plus a handler in `src/lib.rs`. (The previous 25-handler set was removed during cleanup: 24 of its handlers were never actually installed, and its `log_event` logger was a no-op.)

## Benchmarks & Performance
Run the microbenchmark with:
```bash
cargo run --release --bin bench
```
It measures the FullContext uprobe overhead (full register save/restore + dispatcher + trampoline) against a no-op baseline — typically ~15 ns per call on Apple Silicon.

The logging hot path is allocation-free: `mach_absolute_time()` + a memcpy into a per-thread SPSC ring buffer, with a background thread doing all formatting and I/O. Timestamps are anchored once with `gettimeofday` at startup and computed as `mach_absolute_time` deltas — no syscalls on the hot path.

*Note: the historical README table claiming near-zero overhead across 25 syscalls was inaccurate — most of those syscalls were never hooked in that build, and the "traced socket() is faster than native because it bypasses Apple telemetry" figure was measurement noise.*

