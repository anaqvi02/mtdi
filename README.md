# mtdi

**Zero-privilege user-space dynamic instrumentation for macOS (Apple Silicon).**

`mtdi` (formerly `mtrace`) is a high-speed tracer + dynamic-instrumentation
engine that intercepts libc/API calls in an unmodified process — no root, no
SIP changes, no kernel extensions. It loads a dylib via `DYLD_INSERT_LIBRARIES`,
installs AArch64 inline hooks (detours with instruction relocation) on target
functions, and streams events out through a lock-free per-thread ring buffer.

A full-context hook costs **~15 ns per call** (all 64 registers saved and
restored); a FastPath hook costs **~1.6 ns**. That puts it in the same league as
Intel Pin / DynamoRIO and roughly two to three orders of magnitude faster than
`dtruss` (DTrace) or Frida-with-JavaScript on the same task.

---

## Why this exists

Apple's native `dtruss` requires disabling System Integrity Protection (SIP)
and running as root. `mtdi` does the job entirely in user space: `dyld` loads
the instrumentation dylib at process start, a constructor installs inline
detours on the functions you care about, and everything after that is plain
userspace execution — no kernel boundary crossings on the hot path.

> **Note on naming:** this is *not* a true system-call tracer. It traces libc /
> exported-symbol calls. Most applications use the libc API, so in practice it
> sees what you need. Raw `svc`-style syscalls bypass it entirely (see
> [Limitations](#limitations)).

## Features

- **Zero privileges** — runs as a standard user; no sudo, no SIP changes, no
  kexts, no developer mode.
- **Fast** — ~15 ns/call full-context hooks, ~1.6 ns/call FastPath (measured on
  Apple M3; see [Benchmarks](#benchmarks)).
- **Allocation-free hot path** — `mach_absolute_time()` + a memcpy into a
  per-thread SPSC ring; a background thread does all formatting and I/O.
- **Microsecond timestamps** — anchored once with `gettimeofday` at startup,
  computed as `mach_absolute_time` deltas; no syscalls while tracing.
- **Output formats** — human-readable, NDJSON (`-j`), or Elastic Common Schema
  (`-e`) for SIEM ingestion.
- **Active manipulation** — hooks run in-process, so you can mutate arguments,
  spoof return values, block calls, or implement TOCTOU-style behavior changes.
- **JIT-compiled Rust probes** — write a small Rust probe, `mtdi` verifies its
  AST against a zero-panic policy, compiles it with `rustc`, and injects it.
- **BYO dylib** — load any pre-built dylib with `-l`, or swap `open()` entirely
  via `MTDI_SWAP_DYLIB`.
- **MCP server** — AI-agent friendly: enumerate modules/exports, validate probe
  syntax, and run traces from any MCP-capable client.

## Requirements

- macOS on **Apple Silicon** (arm64). No Intel support.
- Rust toolchain (`cargo`/`rustc`) to build. Probes additionally require
  `rustc` at trace time (`-s` mode).
- SIP does **not** need to be disabled for normal third-party apps, but see
  [Compatibility](#compatibility).

## Build

```bash
cargo build --release
```

This produces:

- `target/release/mtdi` — the CLI (launcher + probe compiler)
- `target/release/libmtdi_lib.dylib` — the injected instrumentation dylib

## Quick Start

```bash
# Trace a command
./target/release/mtdi python3 -c "print('hello')"

# Only log open() calls
./target/release/mtdi -t open ./your_binary

# Write to a file instead of stderr
./target/release/mtdi -o trace.log ./your_binary

# NDJSON or Elastic Common Schema output
./target/release/mtdi -j -o trace.json ./your_binary
./target/release/mtdi -e -o ecs_trace.json ./your_binary

# Attach to a running PID (requires task_for_pid access, e.g. a root-owned target)
./target/release/mtdi -p 1234
```

CLI reference: `mtdi -h`.

| Flag | Meaning |
|------|---------|
| `-s, --script <file.rs>` | Compile, AST-verify, and inject a Rust probe |
| `-l, --load <dylib>` | Load a pre-built custom dylib |
| `-p, --pid <PID>` | Attach to a running process |
| `-t, --trace <calls>` | Comma-separated filter (currently: `open`) |
| `-o, --output <file>` | Log destination (default: stderr) |
| `-j, --json` | NDJSON output |
| `-e, --ecs` | Elastic Common Schema output |
| `-u, --legacy-unwind` | Bypass AST verification; permit panics (slower) |

## Dynamic Instrumentation (`-s` probes)

Write a Rust probe, register hooks on any exported symbol, and let `mtdi`
compile + inject it:

```rust
// probes/probe_open.rs — full working template
pub fn on_open(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 256) {
        println!("[mtdis probe] open() -> path: \"{}\", flags: {:#x}", path, ctx.arg(1));
    }
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("open", on_open);
}
```

```bash
./target/release/mtdi -s probes/probe_open.rs ./your_binary
```

The engine:

1. **Parses and AST-verifies** your code against a zero-panic policy (see below).
2. **Compiles** it as a self-contained `cdylib` with `-C overflow-checks=off`
   and `-C panic=abort`.
3. **Injects** it via dyld; its constructor resolves each registered symbol and
   installs an inline detour per hook.

### The zero-panic AST rules

When `-u` is not given, the verifier rejects:

- `.unwrap()` / `.expect()` (method *and* bare function calls)
- raw indexing `foo[i]` — use `.get_safe(i)` (clamps out of bounds) or `.get(i)`
- `panic!`, `assert!`, `assert_eq!`, `assert_ne!`, `todo!`, `unimplemented!`,
  `unreachable!`
- `panic_any`, `abort`, `exit`, `unreachable_unchecked` calls
- raw `/` and `%` — division by zero panics even with overflow checks off; use
  `SafeU64::checked_div()` / `checked_rem()`

Everything else is fair game: loops, allocation, recursion, wrapping integer
math. Your code runs inside a `#![forbid(unsafe_code)]` module. The harness
compiles with `-C panic=abort`, so if something *does* panic at runtime, the
traced process aborts — write code that can't panic.

The probe API (`MtdiSafeContext`):

- `arg(i)` / `set_arg(i, v)` — registers x0–x7
- `return_val()` / `set_return_val(v)` — mutate the return value
- `read_arg_str(i, max_len) -> Option<String>` — safely read a C string arg

### Swapping `open()` entirely

`probes/swap.rs` is a template for a dylib that replaces `open()` with your own
implementation. It forwards through the raw `syscall` instruction, so it cannot
recurse back into the hook:

```bash
rustc --edition=2021 --crate-type cdylib -O probes/swap.rs -o swap.dylib
MTDI_SWAP_DYLIB=$PWD/swap.dylib ./target/release/mtdi ./your_binary
```

## How it works

```
┌───────────────────────────┐        DYLD_INSERT_LIBRARIES
│  mtdi CLI                 │ ────────────────────────────────┐
│  spawns target + injects  │                                  ▼
└───────────────────────────┘               ┌──────────────────────────────┐
                                            │  libmtdi_lib.dylib            │
 ┌─────────────────────────┐               │  (loaded before main() runs)  │
 │  probe compiler (mtdis)  │  rustc  ──►  │  __mod_init_func constructor  │
 │  AST verify + JIT        │               │   ├─ installs detours         │
 └─────────────────────────┘               │   ├─ MTDI_SWAP_DYLIB support   │
                                            │   └─ spawns log-reader thread  │
                                            └──────────────┬───────────────┘
                                                           │
┌──────────────────────────────────────────────────────────▼──────────────────┐
│ Hook engine (src/hook)                                                      │
│  • FastPath: target prologue replaced with an absolute jump straight to a  │
│    C handler (e.g. my_open). No register save. ~1.6 ns/call.               │
│  • FullContext: jump to a thunk that spills all 32 GPRs + 32 SIMD regs     │
│    (~784 bytes), calls a Rust dispatcher, restores everything, then        │
│    continues through a trampoline. ~15 ns/call.                            │
│  • Trampolines: stolen prologue instructions are relocated (ADRP/ADR,      │
│    B/BL, B.cond, CBZ/CBNZ, TBZ/TBNZ, LDR-literal) into freshly allocated   │
│    executable memory, then branch back into the original function.         │
│  • Page protection toggled via mach_vm_protect (16 KB pages), icache       │
│    invalidated via sys_icache_invalidate.                                  │
└─────────────────────────────────────────────────────────────────────────────┘
│
▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Logging pipeline (src/lib.rs)                                               │
│  Producer (hook): mach_absolute_time() + memcpy args/strings into the       │
│  thread's SPSC ring slot (1024 slots/thread, up to 128 threads). No         │
│  allocation, no locks, no syscalls.                                         │
│  Consumer: background thread formats (plain / NDJSON / ECS) and writes.     │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Repository layout

```
src/
  main.rs              CLI entry point
  lib.rs               injected dylib: constructor, hooks, ring buffer, loggers
  cli/                 argument parsing + process spawning (SIP checks)
  injector/mach.rs     live-PID injection via task_for_pid + remote thread
  hook/                inline hook engine
    manager.rs         install_hook: FastPath / FullContext
    trampoline/        thunk asm, builder, relocator, allocator, disassembler
  script/compiler.rs   probe compiler: AST verifier + rustc harness generation
  mcp_server.py        MCP server (see below)
probes/                runtime-compiled probe templates (probe_open.rs, swap.rs)
examples/              C harnesses for manual testing and benchmarking
```

## Benchmarks

Measured on Apple M3, `cargo run --release --bin bench` (1M iterations, warm)
and `cargo run --release --bin bench_cold` (cache-thrashed worst case):

| Scenario | FullContext | FastPath |
|---|---|---|
| Warm loop (1M iters) | **15–16 ns** | **1.6 ns** |
| Cold (32 MB code + 64 MB data thrash, p50) | 42 ns | ~0 ns |
| Cold (p99) | 208 ns | 42 ns |
| Cold (max) | 542 ns | 125 ns |
| First call after install (one-time) | 1,750 ns | — |
| 8-thread contention, mean | 116 ns | 5.7 ns |

`bench` measures the cost of the **hook itself** — the detour, the dispatch,
and the trampoline hop — with a no-op handler, so no probe logic is in the
measurement and the hooked function's own body (the same 5 nops in both
baseline and hooked runs) cancels out. `bench_cold` adds worst-case
cache/BTB pressure and first-call effects. Real probe work (string decoding,
formatting, I/O) stacks on top, but stays off the hot path — the logging hot
path is an allocation-free ring-buffer write.

**Where that lands vs. other instrumentation engines** (order of magnitude,
from public benchmarks — setups vary):

| Engine | Approx. per-call overhead |
|---|---|
| **mtdi FastPath** | **~1.6 ns** |
| **mtdi FullContext** | **~15 ns** |
| Intel Pin / DynamoRIO (simple analysis) | ~1–20 ns |
| Frida, native (C) callback | ~0.1–1 µs |
| Linux eBPF uprobes | ~1–3 µs |
| dtruss / DTrace | ~1–10 µs |
| Frida, JavaScript callback | ~5–50 µs |

Notes on honesty: the ~15 ns figure is a *no-op* full-context handler — that's
the floor, and the speed comes partly from doing less than the alternatives
(targeted libc hooks, no kernel involvement, no cross-process marshaling).
FullContext contention at 116 ns is real: the dispatcher serializes on a global
`Mutex<HashMap>`; FastPath avoids it entirely. The historical README claimed
near-zero overhead across 25 syscalls — that was inaccurate (most were never
hooked, and one figure was measurement noise). The numbers above are what the
current code actually does.

## Compatibility

### Cannot be traced

- **Apple-signed system binaries** (`/bin/ls`, `/usr/bin/curl`, …) — blocked by
  SIP. `mtdi` warns you if you try with SIP enabled.
- **`arm64e` binaries** — Apple restricts PAC-enabled arm64e to their own
  components; dyld refuses to load a standard arm64 dylib into them.
- **Hardened Runtime with Library Validation** — App Store apps block injected
  dylibs. Unlike the first two categories, this one is removable:
  `codesign --remove-signature <app>`.

### Can be traced

Anything standard arm64 without strict Library Validation: Homebrew tools,
compiled C/Rust binaries, scripting interpreters, Electron apps, and most
third-party applications. Verified working: Steam, Blender, Unity, Firefox,
Chrome/Chromium, MS Word, Zoom, JetBrains IDEs, GarageBand, Minecraft (arm64
Java), Spotify, VLC, OBS, Photoshop, Ableton Live, Docker Desktop, iTerm2,
Sublime Text, TablePlus, Epic Games Launcher, Dropbox, Telegram, and general
Homebrew CLI tools.

> [!IMPORTANT]
> With SIP **enabled**, macOS blocks injection into any app enforcing the
> Hardened Runtime. Tracing those requires disabling SIP (`csrutil disable` in
> Recovery Mode). `mtdi` will warn you when it detects a protected binary.

## Limitations

- Not a true syscall tracer: only the explicit libc/exported symbols it hooks
  are visible (unlike `dtruss`, which catches everything at the kernel
  boundary).
- Bypassable by code issuing raw assembly syscalls (`svc 0x80`) instead of
  calling libc.
- Probes are limited to exported symbols; no arbitrary memory/register
  inspection like Frida or QBDI (the full-context `RegisterContext` API is
  available to *built-in* hooks, but probe scripts get the safe `MtdiSafeContext`
  surface).
- The built-in dylib currently hooks one function (`open`); the engine supports
  any function, and `-s` probes can hook any exported symbol.
- The dispatcher's global mutex shows up under heavy multi-threaded hook
  traffic (FullContext path; see benchmarks).

## MCP Server

`src/mcp_server.py` exposes mtdi to AI agents over MCP: process/module/export
enumeration, symbol demangling, probe-syntax validation, live traces, and
dyld-injection launches.

```bash
pip install fastmcp
python3 src/mcp_server.py
```

Or wire it into your agent's MCP config:

```json
{
  "mcpServers": {
    "mtdi": {
      "command": "/path/to/venv/bin/python3",
      "args": ["/path/to/mtdi/src/mcp_server.py"]
    }
  }
}
```

The server locates the `mtdi` binary via `$MTDI_BIN`, then
`<repo>/target/release/mtdi`, then `PATH`.

## Testing

```bash
cargo test --release          # AST-verifier unit tests
cargo clippy --all-targets --release -- -D warnings   # must stay clean
cargo run --release --bin bench       # warm overhead
cargo run --release --bin bench_cold  # worst-case overhead battery
```

## License

[Apache License 2.0](LICENSE).

## Disclaimer

`mtdi` is an instrumentation tool. Intercepting or modifying another
application's behavior may violate its license, your organization's policy, or
the law. Use it only on software you own or are authorized to analyze.
