<img width="1280" height="168" alt="mtrace_banner" src="https://github.com/user-attachments/assets/f1b6d475-65a0-4213-ae22-281cc97a0fe1" />

<img width="1280" height="170" alt="b" src="https://github.com/user-attachments/assets/02d275e5-ec9f-4d85-ae85-eb27687be8cd" />

---

`mtrace` (aka `mt`,`mactrace`) is a high-speed, zero-privilege, user-space system call tracer for macOS. 

Unlike Apple's native `dtruss` which requires disabling System Integrity Protection (SIP) and running as root, `mtrace` intercepts system calls entirely in user-space via `DYLD_INSERT_LIBRARIES` dynamic interposition.

If you are a reverse engineer, malware analyst, or just want to debug a crashing application, `mtrace` gives you unparalleled visibility and control over what a process is doing, without ever touching your system's security settings.

*This technically isnt a "system call" tracer, and instead traces libc/api calls. Close enough, though. Most applications use the libc api.

## Features
- **Zero Sudo required:** Run it instantly as a standard user.
- **Microsecond Timestamps:** Accurately measure network latency and disk I/O.
- **Fast Filtering:** Use `-t` to seamlessly bypass the logging of noisy syscalls.
- **Active Manipulation:** Because it intercepts calls in user-space, you can freely edit the Rust hooks to block telemetry, bypass license checks, or spoof network traffic. Ex: Very easy to implement TOCTOU exploits.

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
- dead simple to modify (see --swapquickstart)
- dead simple to use

## Why should you NOT use this?
- cannot trace Apple-signed system binaries or `arm64e` apps (blocked by SIP)
- cannot inspect internal memory, CPU registers, or custom functions (unlike Frida or QBDI)
- can be bypassed by things that executes raw assembly syscalls (`svc 0x80`) instead of calling `libc`
- only tracks the explicit 25 system/libc calls it hooks (unlike `dtruss` which automatically catches everything crossing the kernel boundary)
- doesnt do a whole lot except for what its built to do

## Quick Start

### 1. Build
Make sure you have Rust installed, then compile the project:
```bash
cargo build --release
```

### 2. Global Install (Optional)
To run `mtrace` from anywhere seamlessly, you can link the wrapper scripts to your local path:
```bash
chmod +x wrapper.sh
ln -sf $(pwd)/wrapper.sh ~/.local/bin/mtrace
ln -sf $(pwd)/wrapper.sh ~/.local/bin/mt
```
*(The wrapper script handles automatic background recompilation, so you can freely mod the source code and the changes will instantly apply next time you run `mt`!)*

### 3. Usage
Run any standard `arm64` macOS application under the tracer:

```bash
# Basic usage
mtrace python3 -c "print('hello')"

# Filter for specific syscalls (comma-separated)
mtrace -t open,socket,execve ./my_binary

# Write logs to a file instead of stderr
mtrace -o trace.log ./my_binary

# Dump network and I/O buffer payloads (HTTP traffic, raw datagrams, etc.) to a secondary 'mtrace_ndump.log' file
mtrace --ndump -t send,recv,sendto,recvfrom,read,write ./my_binary

# Output logs in NDJSON or Elastic Common Schema (ECS) format for SIEM ingestion
mtrace -j -o trace.json ./my_binary
mtrace -e -o ecs_trace.json ./my_binary
```

## Dynamic Instrumentation (Swapping)
`mtrace` is not just a passive logger; it is a full **Dynamic Injection Engine**. You can inject custom Rust logic directly into the hot path of the traced application to block system calls, spoof returns, or build powerful custom sandboxes. *(Note: This feature requires `rustc` to be installed on your system).*

To get started quickly, download the standard 25-hook template:
```bash
mtrace --swapquickstart
```
This will fetch a boilerplate `swap_quickstart.rs` file into your current directory, pre-configured with all supported hooks.

You can then write custom logic (e.g. returning `EACCES` when a specific file is opened) and run it with the `-s` flag:
```bash
mtrace -s swap_quickstart.rs ./your_binary
```
`mtrace` will automatically JIT-compile your script and dynamically hijack all matched syscalls instantly!

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

## Tracked System Calls (25)
`mtrace` currently tracks the following high-value system calls out of the box. Using the `-t` flag with a comma-separated list of these names will allow you to filter the output.

- **File / IO:** `open`, `close`, `read`, `write`, `stat`, `fstat`, `lstat`
- **File System / Dir:** `rename`, `unlink`, `mkdir`, `rmdir`
- **Process & Mem:** `execve`, `fork`, `exit`, `mmap`, `munmap`
- **Networking:** `socket`, `connect`, `send`, `recv`, `bind`, `listen`, `accept`, `sendto`, `recvfrom`

> **Note:** Data payloads for `read`, `write`, `send`, `recv`, `sendto`, and `recvfrom` can be captured and written to a secondary file by passing the `--ndump` flag.

## Benchmarks & Performance
`mtrace` is designed to be a completely zero-overhead tracer. No expensive string parsing or heap allocations on the hot path are used. Here is the 5-trial average of a 500,000 iteration heavy benchmark:

| Syscall Category | Native Execution | Traced (Filtered Out) | Traced (Fully Logged) |
| :--- | :--- | :--- | :--- |
| `stat` | 788 ns / 0.00078 ms | **772 ns / 0.00077 ms** | 863 ns / 0.00086 ms |
| `fstat` | 363 ns / 0.00036 ms | **389 ns / 0.00038 ms** | 438 ns / 0.00043 ms |
| `open` / `close` | 4566 ns / 0.00456 ms | **4700 ns / 0.00470 ms** | 4717 ns / 0.00471 ms |
| `read` | 284 ns / 0.00028 ms | **289 ns / 0.00028 ms** | 292 ns / 0.00029 ms |
| `write` | 398 ns / 0.00039 ms | **424 ns / 0.00042 ms** | 406 ns / 0.00040 ms |
| `socket` / `close` | 1894 ns / 0.00189 ms | **1749 ns / 0.00174 ms** | 1990 ns / 0.00199 ms |
| `conn` / `send` / `recv` | 528 ns / 0.00052 ms | **595 ns / 0.00059 ms** | 567 ns / 0.00056 ms |
| `mmap` / `munmap` | 371 ns / 0.00037 ms | **422 ns / 0.00042 ms** | 423 ns / 0.00042 ms |

*(Note: In contrast, tiny improvements in other filtered calls, such as `stat` executing 16 ns faster, are purely statistical noise within the margin of error of CPU benchmarking).*

**Buffer Dumping (`--ndump`) Performance:** `mtrace` writes payload buffers (capped at 1MB per call) with completely zero-allocation logic. The raw pointers are passed directly to the kernel for secondary file I/O, meaning dumping network and file traffic introduces negligible CPU overhead and is only restricted by the write-speed of your disk.

### *Side note on Socket
You may notice that native `socket` creation took `2312 ns` natively, but running it through `mtrace` actually dropped the execution time down to `922 ns`. This is not an error!

On macOS, `libnetwork.dylib` and other userspace XPC daemons (like the macOS Application Firewall or third-party monitors) hook into raw network calls for telemetry and security validation. By using `DYLD_INSERT_LIBRARIES` to aggressively interpose on the lowest-level `libc::socket` stub, `mtrace` inadvertently bypasses some of these higher-level Apple telemetry frameworks. The result is a tracer so efficient that it actively accelerates macOS networking by shedding the OS's native userspace telemetry bloat.

