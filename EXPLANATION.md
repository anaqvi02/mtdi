# MTDI — Complete Engine Explanation

**How this dynamic-instrumentation engine actually works, subroutine by subroutine.**

Written by reading the source, not the README. Where a comment or doc disagrees with
the code, this document describes the code and flags the disagreement explicitly.

Source of truth: `src/lib.rs` (871 lines), `src/script/compiler.rs` (763), `src/injector/mach.rs` (340),
`src/hook/**`, `src/cli/**`, `src/main.rs`, `src/mcp_server.py`.

---

## 0. The 60-second version

MTDI is a user-space dynamic-instrumentation (DI) engine for macOS/Apple Silicon. It does three things:

1. **Gets code into a target process** — either at launch (`DYLD_INSERT_LIBRARIES`) or live (`task_for_pid` + a remote thread).
2. **Rewrites machine code in that process** — overwriting function prologues with a branch to a handler, after relocating the instructions it stole into a fresh trampoline.
3. **Collects the resulting events without slowing the traced code down** — a lock-free, allocation-free, per-thread ring buffer written on the hot path and drained by a background thread.

There are two hook types, two hook *installers*, and two injection paths. That duplication is
deliberate in places and accidental in others; both are documented below.

```
                       ┌──────────────────────────────────────────────┐
   mtdi CLI (main.rs)  │  parse args → resolve dylib → pick a mode    │
                       └───────┬──────────────────────┬───────────────┘
                               │ launch               │ attach (-p)
                               ▼                      ▼
                 DYLD_INSERT_LIBRARIES        task_for_pid + remote thread
                               │                      │
                               └──────────┬───────────┘
                                          ▼
                            libmtdi_lib.dylib ctor (__mod_init_func)
                                          │
              ┌───────────────────────────┼────────────────────────────┐
              ▼                           ▼                            ▼
    install 25 FastPath hooks    MTDI_SWAP_DYLIB dlopen      mmap 144 MiB of rings
    (src/hook/manager.rs)        (replace open())            + spawn reader thread
              │
              ▼
      traced process runs: call → detour → handler → ring slot → reader → log
```

---

## 1. The CLI (`src/main.rs`, `src/cli/args.rs`, `src/cli/spawn.rs`)

### 1.1 `main()` — `src/main.rs:24`

Ordered execution:

1. `cli::args::parse_args()` → `AppArgs`.
2. **Dylib resolution** (`main.rs:27-45`), in priority order:
   - `-s <probe.rs>` → calls `script::compiler::compile_script()`, prints `[mtdis] Verifying and compiling safe probe`, uses the returned `.dylib` path.
   - `-l <dylib>` → uses that path verbatim (BYOB).
   - otherwise → `env::current_exe()`, canonicalised, with the filename replaced by `libmtdi_lib.dylib`
     (so the CLI finds the engine dylib next to itself).
3. Existence check → `[mtdi] Error: Dylib not found at …` + hint to `cargo build`.
4. **Mode dispatch**:
   - `-p <PID>` → rejects `-o/-t/-j/-e` with an explicit error (`"a running target's environment can't be set by attach"`), then `injector::mach::inject_into_pid(pid, &dylib)`.
   - `--check-only` → requires `-s`; prints `[mtdis] Check-only: probe compiles and passes verification.` and **never injects** (this is what the MCP `check_probe_syntax` tool calls).
   - default → `cli::spawn::spawn_target(args, &dylib)`.

`CHILD_PID: AtomicU32` and `handle_signal()` (`main.rs:12`) exist so the CLI can forward a signal
to the traced child: on SIGINT/SIGTERM it sends **SIGTERM** (not SIGKILL) so the child's `atexit`
and the dylib's SIGTERM handler can drain the ring, then `_exit(1)`.

### 1.2 `parse_args()` — `src/cli/args.rs:17`

Hand-rolled sequential parser (`while !args.is_empty()`, `args.remove(0)`). Flags are consumed
by scanning left-to-right; **the first non-flag token ends flag parsing** and becomes the command,
with everything after it treated as the command's own arguments. (Consequence: `mtdi ./bin -o out.log`
passes `-o out.log` to `./bin`, not to mtdi. Put mtdi's flags first.)

| Flag | Field | Meaning |
|---|---|---|
| `-o/--output <file>` | `output_file` | log destination (default: stderr, or a TMPDIR file in attach mode) |
| `-t/--trace <list>` | `trace_filter` | comma-separated syscall filter |
| `-p/--pid <PID>` | `target_pid` | attach to running process |
| `-j/--json` | `json_output` | NDJSON |
| `-e/--ecs` | `ecs_output` | Elastic Common Schema |
| `-l/--load <dylib>` | `custom_dylib` | inject a prebuilt dylib |
| `-s/--script <file>` | `script_file` | compile+verify+inject a Rust probe |
| `-u/--legacy-unwind` | `legacy_unwind` | skip AST verification, allow panics (catch_unwind) |
| `-c/--check-only` | `check_only` | compile+verify only, no injection |
| `-h/--help` | — | help, exit 0 |

**Filter validation** (`args.rs:108-118`): every name in `-t` must be in the hardcoded `KNOWN: [&str; 25]`
list, else the CLI errors and prints the full known list. This is the *CLI-side* validation of the
25 syscalls; the dylib independently maps names to bits.

### 1.3 `spawn_target()` — `src/cli/spawn.rs:82`

1. `check_sip_and_codesign(&cmd_name)` — **hard-fails instead of silently tracing nothing**:
   - `file -b <bin>`: if it says `arm64e`, error out. Apple gates PAC-enabled arm64e to its own
     components; dyld refuses to load a standard arm64 dylib into them (the child would SIGABRT at load).
   - `csrutil status`: if SIP is **not** enabled, allow (dev machine).
   - If SIP is on and the binary lives in `/bin`, `/usr/bin`, `/sbin`, `/usr/sbin`, `/System` → error:
     `DYLD_INSERT_LIBRARIES` is stripped from Apple system binaries.
   - `codesign -dvv`: if `flags=0x10000(runtime)` (hardened runtime) **and** `codesign -d --entitlements :-`
     lacks both `com.apple.security.cs.allow-dyld-environment-variables` and
     `com.apple.security.cs.disable-library-validation` → error.
   - The stated reason is honest: silent dyld env-stripping would produce a trace with zero hooks
     and the user would never know.
2. Sets the environment contract and spawns:
   - `DYLD_INSERT_LIBRARIES=<dylib>` (**overwrites** any existing value — see §12.5 for the consequence)
   - `MTDI_OUTPUT=<file>` if `-o`
   - `MTDI_FILTER=<list>` if `-t`
   - `MTDI_JSON=1` / `MTDI_ECS=1` if `-j` / `-e`
   - `MTDI_OWN_SIGTERM=1` **always in launch mode** — the flag that tells the dylib "you own SIGTERM and
     may write to inherited stderr" (versus attach mode).
3. Stores the child pid in `CHILD_PID`, installs `handle_signal` for SIGINT/SIGTERM, waits, then prints
   `[mtdi] Command '<cmd>' finished successfully!` or the exit status.

---

## 2. Injection path A — at launch (dyld)

`DYLD_INSERT_LIBRARIES` makes dyld load the dylib before `main()`. The dylib's entry point is a
Mach-O constructor:

```rust
// src/lib.rs:88-90
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INITIALIZE: unsafe extern "C" fn() = { … init … };
```

Everything in §3 happens **before the target's `main()` runs** — which is why launch mode can hook
syscalls whose pages a live process can no longer modify (see §12.1).

---

## 3. The dylib constructor (`src/lib.rs:91`) — the whole engine's setup

Ordered steps, exactly as written:

1. **Time base**: `mach_timebase_info(&raw mut TIMEBASE)` (numer/denom) and one `gettimeofday`, stored as
   `INIT_TIMEOFDAY_USEC` (absolute epoch µs) plus `INIT_MACH_TIME = mach_absolute_time()`.
   ⇒ All later timestamps are `mach_absolute_time()` deltas anchored to one wall-clock reading taken at
   load. No syscall is ever made on the hot path to get a timestamp. `INIT_PID = getpid()`.
2. **Install the 25 built-in hooks** via a local `macro_rules! install!` (`lib.rs:103-135`). Each call is
   `hook::manager::install_hook(name, libc::<fn> as usize, HookType::FastPath(<my_fn> as usize))`.
   On success the returned trampoline address is stored into a per-syscall `AtomicUsize`
   (`TRAMP_OPEN`, `TRAMP_CLOSE`, …). On failure: `[mtdi] Hook skipped for <name>: <err>` — this is how
   PPL-sealed pages are reported, not by crashing.
3. **`MTDI_SWAP_DYLIB`** (`lib.rs:137-148`): if set, `dlopen(path, RTLD_LAZY|RTLD_LOCAL)`, `dlsym(handle, "on_open")`,
   store the pointer in `USER_ON_OPEN`. `my_open` prefers it over the trampoline (see §7.2).
4. **Output destination** (`lib.rs:150-182`):
   - `MTDI_OUTPUT` set → `open(path, O_CREAT|O_WRONLY|O_APPEND|O_CLOEXEC, 0666)`, store fd in `LOG_FD`.
   - else if `MTDI_OWN_SIGTERM` is **absent** (i.e. attach mode) **and** `isatty(2) == 0` → log to
     `$TMPDIR/mtdi_<pid>.log`. Rationale in the code: an attached GUI process's stderr is `/dev/null`,
     so events would vanish; launch mode keeps the inherited stderr.
5. **Attach-mode disclaimer** (`lib.rs:184-190`): two `mtdi_log()` lines stating that PID attach only covers
   the PPL-writable subset and that "things will break if you try to trace syscalls anyway with PID attaching".
6. **Filter mask** (`lib.rs:192-228`): `FILTER_MASK` starts as `0xFFFFFFFF` (all 25 bits set). If
   `MTDI_FILTER` exists it is **reset to 0** and each comma-separated name sets one bit:
   `open`=bit0, `close`=1, `read`=2, `write`=3, `socket`=4, `connect`=5, `send`=6, `recv`=7, `stat`=8,
   `execve`=9, `fork`=10, `exit`=11, `mmap`=12, `munmap`=13, `unlink`=14, `rename`=15, `lstat`=16,
   `fstat`=17, `bind`=18, `listen`=19, `accept`=20, `sendto`=21, `recvfrom`=22, `mkdir`=23, `rmdir`=24.
   Unknown names are ignored (`_ => {}`) — the CLI is what rejects typos.
7. **`MTDI_JSON` / `MTDI_ECS`** → `JSON_OUTPUT` / `ECS_OUTPUT` flags (ECS wins if both).
8. **Status banner**, written straight to `LOG_FD`: ECS → `{"@timestamp":"2000-01-01T00:00:00Z","event":{"action":"init"},"message":"mtdi active"}`,
   JSON → `{"event":"mtdi_active"}`, plain → `[mtdi] Active! Monitoring system calls...`.
   (Note the ECS banner carries a placeholder timestamp; it is not from the anchor.)
9. **Ring allocation**: `pthread_key_create(&key, None)` → `THREAD_KEY`; then
   `mmap(MAX_THREADS * size_of::<ThreadQueue>(), PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANON)` →
   `THREAD_QUEUES`. That is **128 × 1,180,800 = 151,142,400 bytes ≈ 144 MiB of virtual address space**,
   lazily backed by the kernel (`MAP_ANON` pages are zero-fill on first touch).
10. **Reader thread** (`thread::spawn`, `lib.rs:262-352`) — see §8.
11. `libc::atexit(flush_on_exit)` — drain the ring on normal exit.
12. **Launch mode only**: `signal(SIGTERM, handle_terminate)` so Ctrl-C drains the ring before dying.
    Attach mode leaves the target's own handlers alone.

---

## 4. Injection path B — live attach (`src/injector/mach.rs`)

`inject_into_pid(pid, dylib_path)` (`mach.rs:43`). This is the most intricate code in the project.

**The problem it solves:** to make dyld4's `dlopen` work in a target process, you must call it on a
*real* pthread (dyld4 reads pthread TLS and crashes with a null TSD base). `task_for_pid` gives you a
Mach task, and `thread_create_running` gives you a raw Mach thread with no TLS. So the injector
boots the raw thread into a pthread.

1. **Prints a consent-prompt warning** (`mach.rs:45-48`): macOS may show "…wants to control this process"
   and block until a human clicks Allow — explicitly documented as not-a-hang.
2. `task_for_pid(mach_task_self(), pid, &mut target_task)`; failure → suggests sudo/SIP and exits 1.
3. `mach_vm_allocate(target_task, &mut remote_address, 1 MiB, 1)` — one 1 MiB region for stack + payload.
4. **Remote memory layout** (all offsets from `remote_address`):

   | Offset | Content |
   |---|---|
   | `+0x000` | dylib path (NUL-terminated) |
   | `+0x100` | 2 diagnostic marker bytes (`'R'` then `'D'`), written by the stub as it progresses |
   | `+0x110` | `timespec {1, 0}` used by the park loop |
   | `+0x160` | load-ack: `dlopen`'s return value (0 = not yet / failed) |
   | `+0x170` | `dlerror()` string pointer captured on failure |
   | `+0x4000` | bootstrap stub, its own 16 KB page, made **RX** |
   | `+0x8000` | `pthread_t` slot for `pthread_create_from_mach_thread` |
   | top of 1 MiB | initial SP (16-byte aligned) |

5. **`mov_addr(reg, addr)`** (`mach.rs:33`) — emits `movz` + three `movk` to materialise a 64-bit absolute
   address (shared-cache addresses are valid in the target).
6. **The bootstrap stub** (`mach.rs:127-170`, hand-assembled `Vec<u32>`):
   - Entry: `x0 = &slot`, `x1 = NULL` (attr), `x2 = start_routine`, `x3 = path`, then
     `blr pthread_create_from_mach_thread` — the raw thread's job is only to *create* a real pthread.
   - Then it **parks forever**: a `nanosleep(1s)` loop in a `b` back-edge. The code comments the reason:
     the raw thread must never terminate (`pthread_exit` crashes with null `tpidrro_el0`).
   - `start_routine(path)` (`x0` = path): writes marker `'r'`; `dlopen(path, RTLD_NOW)`; stores the handle
     to `+0x160`; calls `dlerror()` and stores its result to `+0x170`; writes marker `'d'`; parks in the
     same `nanosleep` loop. (Note `RTLD_NOW = 2` on Darwin, not 1.)
7. **Diagnostics**: the markers distinguish "stub never started" from "dlopen ran and failed".
8. Stub page is `mach_vm_protect`ed to **R|X** ("or the first fetch faults (w^x)").
9. **Thread state**: `__pc = stub`, `__lr = pthread_exit`, `__sp = base + 1MiB - 16` (aligned).
   Fallback if `pthread_create_from_mach_thread` is unavailable (pre-dyld4): `x0 = path`, `x1 = RTLD_NOW`,
   `__pc = dlopen` — a raw thread straight into `dlopen`.
10. `thread_create_running(target_task, ARM_THREAD_STATE64 /*6*/, &state, 68 /*272/4*/, &mut child_thread)`.
11. **Load-ack polling**: up to 50 × 100 ms reads of `+0x160`. Non-zero → `[mtdi] Dylib loaded into PID <pid> (handle 0x…)`.
    Still zero → reads markers, the ack bytes, and `dlerror`'s string, then prints a detailed
    `[mtdi] WARNING: dylib did NOT load into PID …` — the injection either succeeds verifiably or fails loudly.

---

## 5. The hook engine, part 1 — page access (`src/hook/mod.rs`)

### 5.1 Why a fork probe exists (`hook/mod.rs:16-60`)

Some pages on Apple Silicon (notably PPL-sealed regions of `libsystem_kernel`) **cannot be written even
after `mach_vm_protect` succeeds**. Touching them faults and kills the process. Since the engine writes
into arbitrary libc text pages, it tests each page first — **in a forked child**, so a fault kills only
the child:

- `probe_page(page_start, addr)`: `fork()`; the child calls
  `mach_vm_protect(self, page_start, 16384, 0, READ|WRITE|COPY)` then performs a
  `write_volatile(p, read_volatile(p))` (a genuine store, which is what materialises a COW/fault),
  then `_exit(0)` on success or `_exit(2)` on `mach_vm_protect` failure. The parent `waitpid`s and
  returns `WIFEXITED && WEXITSTATUS == 0`.
- `page_accepts_writes(addr)`: 32-entry cache of `page_start | writable_bit`, so each unique page is
  forked **once**, not once per symbol.
- Consequence documented in the README: on sealed pages the probe child dies by design, which macOS
  records as `mtdi_lib-*.ips` crash reports. Expected noise, and the fix for it is the
  `probe_bughunt.rs`-style awareness, not a code change.

### 5.2 `unprotect_page` / `protect_page` (`hook/mod.rs:64-90`)

Both operate on **16 KB pages** (`addr & !(16384-1)`):
- unprotect → `mach_vm_protect(…, READ|WRITE|COPY)` (COPY is what allows writing a MAP_PRIVATE/COW text page);
- protect → `mach_vm_protect(…, READ|EXECUTE)`.

### 5.3 `overwrite_with_jump` (`hook/mod.rs:98`)

Writes a 16-byte absolute detour at `target_addr`:

```
 0x58000050  ldr x16, #8        ; load the 8-byte literal that follows into x16
 0xD61F0200  br  x16            ; branch to it (no link register touched)
 <u64>       absolute handler address
```

Order matters and is exactly as written: **probe the pristine page first** (comment: "the child must do
the writes, or cow materialization faults even on writable pages"), then `unprotect_page` and check its
`kern_return` (non-success ⇒ return Err rather than write), then copy 16 bytes,
then `sys_icache_invalidate(target_ptr, 16)`, then `protect_page`.

`sys_icache_invalidate` is the correct macOS cache-maintenance primitive here: **D-cache is coherent on
Apple Silicon, I-cache is not**, so freshly written code must be invalidated before execution.

---

## 6. Trampolines (`src/hook/trampoline/**`)

Overwriting a prologue destroys the instructions there, so they are copied elsewhere, *relocated*
(any PC-relative ones fixed up), and executed before jumping back.

### 6.1 `disasm::is_pc_relative` (`disasm.rs:1`)

Returns true for exactly six families:
`B/BL` (`>>26 == 0b000101|0b100101`), `B.cond` (`>>24 == 0b01010100`), `CBZ/CBNZ` and `TBZ/TBNZ`
(`(>>25)&0b111111 == 0b011010|0b011011`), `ADR/ADRP` (`&0x9F000000 == 0x10000000|0x90000000`), and
`LDR` literal (`&0x3B000000 == 0x18000000`).

The `0x9F000000` mask is deliberate: it captures both `ADR` (`0x10000000`) and `ADRP` (`0x90000000`)
in one test. An earlier version tested only one of them, which is a real latent crash for any function
whose prologue starts with the other form.

### 6.2 `relocator::relocate_instruction` (`relocator.rs:3`)

Non-PC-relative → copied verbatim. Otherwise, six rewrites:

1. **ADR/ADRP** → compute the absolute target (ADRP: `(pc & !0xFFF) + (imm << 12)`; ADR: `pc + imm`,
   sign-extended from a 21-bit field), then emit `ldr xd, #8 ; b #12 ; .dword target`.
2. **B** (not BL) → `ldr x16,#8 ; br x16 ; .dword target`.
3. **BL** → `ldr x30,#12 ; ldr x16,#16 ; br x16 ; .dword (pc+4) ; .dword target` — preserves the return
   address in `x30` while still branching absolutely.
4. **B.cond** → invert the condition and skip: `b.<inverted> #20 ; ldr x16,#8 ; br x16 ; .dword target`.
5. **CBZ/CBNZ** → invert the opcode (`^ (1<<24)`), retarget the immediate to +5 instructions, then the
   absolute jump pair.
6. **TBZ/TBNZ** → same inversion trick with the narrower 14-bit immediate.
7. Anything else PC-relative → **`panic!("Unhandled PC-relative instruction at …")`** (`relocator.rs:150`).
   This is the relocator's known fault line: for the libc functions currently hooked, the stolen 16 bytes
   never contain an unhandled form (e.g. `open`'s prologue is `pacibsp; sub sp; stp; stp`). See §12.3.

### 6.3 `builder::build_trampoline` (`builder.rs:10`)

Takes the 16 stolen bytes, relocates each of the 4 instructions with its **original** PC
(`original_addr + i*4`), appends a 16-byte absolute branch back to `original_addr + 16`, allocates,
writes, `sys_icache_invalidate`s the whole thing, and re-protects R|X.
Returns the trampoline address.

### 6.4 `allocator::allocate_trampoline` (`allocator.rs:12`)

One `mach_vm_allocate` of **a single 16 KB page**, lazily on first use (CAS into `TRAMPOLINE_BASE`),
then a bump pointer (`TRAMPOLINE_OFFSET.fetch_add(size)`). Overflow → `panic!("Out of trampoline memory!")`.
There is no second page and no proximity search here — the detour uses a 16-byte absolute jump, so
proximity is unnecessary.

---

## 7. Installing a hook (`src/hook/manager.rs:28`)

State: `HOOKS: OnceLock<Mutex<HashMap<usize, HookInfo>>>`, keyed by **trampoline address**;
`HookInfo { original_addr, trampoline_addr, handler: Option<fn(&mut RegisterContext)> }`.

`install_hook(name, target_addr, hook_type)`:

1. Lock `HOOKS`; refuse duplicates on the same `original_addr`.
2. `page_accepts_writes(target_addr)` — refuse early (a write would fault the whole process).
3. Copy the 16 bytes at `target_addr`; `build_trampoline(target_addr, &stolen)`.
4. Depending on `HookType`:
   - **`FastPath(handler_addr)`** → the detour jumps **straight to the C handler**; no register save, no
     dispatcher, no map entry needed at call time.
   - **`FullContext(func)`** → allocate a 32-byte stub on the trampoline page:
     ```
     ldr x16, #12        ; hook id
     ldr x17, #16        ; thunk address
     br  x17
     <u64 trampoline_addr>   ; the "hook id" is the trampoline address
     <u64 hook_thunk>        ; the asm thunk
     ```
     The stub is written with unprotect → copy → `sys_icache_invalidate` → protect.
5. `overwrite_with_jump(target_addr, jump_target)` (handler or stub).
6. Insert `HOOKS[trampoline_addr] = HookInfo{..}`; return the trampoline address.

### 7.1 The FastPath shape (`src/lib.rs:111`)

`my_open` is the canonical example (`lib.rs:471`): load the trampoline pointer, transmute it to the real
function signature, check the filter bit, push one event, and forward the call. Cost measured at ~1.6 ns
— it is a branch swap, a pointer load, a bit test, and (usually) another branch.

### 7.2 `my_open`'s two special cases (`lib.rs:471-486`)

1. `USER_ON_OPEN` (from `MTDI_SWAP_DYLIB`) is checked **first**; if set, the swap dylib's `on_open` fully
   replaces the call (it is expected to forward via the raw `syscall` instruction, so it cannot recurse
   into the hook).
2. There is a candid comment about the ABI: Darwin arm64 passes `open`'s variadic `mode` on the **caller's
   stack**, not in `x2`, so the recorded `mode` is only meaningful for non-variadic callers. The trace
   shows junk like `mode=1869621824` for `gcc`-compiled callers. Documented, not "fixed".

---

## 8. The logging pipeline (`src/lib.rs`) — the part that makes it fast

### 8.1 Layout

```rust
#[repr(align(128))]
pub struct Slot {                       // 1152 bytes
    timestamp: u64,                     //  8   mach_absolute_time() at capture
    formatter: Option<SlotFormatterFn>, //  8   fn(&Slot, json, ecs, &mut Formatter) -> Result
    args: [u64; 6],                     // 48   packed syscall args
    str1_len: usize, str2_len: usize,   // 16   lengths of the two inline strings
    str_data: [u8; 1024],               // 1024 inline string storage (path, old/new, …)
    _pad: [u8; 48],                     // 48   padding to 1152
}
```

The `formatter` pointer lives **in the slot**: the reader never dispatches on a tag, it just calls the
pointer. That removed a branch from the consumer.

```rust
#[repr(align(128))]
pub struct ThreadQueue {                              // 1,180,800 bytes
    ready: [AtomicU8; 1024],                          // 1024  publish flags
    slots: [Slot; 1024],                              // 1,179,648
    write_head: usize,                                // 8     producer cursor (thread-private)
    _pad: [u8; 120],                                  // 120
}
```

128 queues × 1,180,800 = **144 MiB of VA** per traced process, sparse.

### 8.2 Producer — `push_binary_event` (`lib.rs:388`)

Called on the hot path by every FastPath handler:

1. `timestamp = mach_absolute_time()` — a commpage read, no syscall.
2. `pthread_getspecific(THREAD_KEY)` → this thread's `ThreadQueue*`.
3. **First call from a thread**: CAS-claim the next index from `ACTIVE_QUEUES` (bounded: `>= MAX_THREADS`
   → drop the event and return), set `THREAD_QUEUES.add(idx)` into TLS. The comment notes this bound is
   what keeps the reader's `take(active)` in range, and that it is fork-safe.
4. `slot = slots[write_head & 1023]`.
5. **If `ready[slot_idx] != 0` → the queue is full → drop the event and return.** (Loss is the load-shedding
   mechanism; it is also silent — see §12.4.)
6. Fill `timestamp`, `formatter`, `args`.
7. Copy string 1 with `strnlen(ptr, 512)` + `copy_nonoverlapping`; then string 2 (cap
   `min(512, 1024 - offset)` — the two strings share the 1024-byte buffer).
8. `write_head += 1`; `ready[slot_idx].store(1, Release)` — the release store is the publish.

No allocation, no locks, no syscalls, no formatting. That is the entire reason a hook costs nanoseconds.

### 8.3 Consumer — the reader thread (`lib.rs:262`)

- `read_heads: [usize; 128]`, one cursor per queue.
- Loop: `active = ACTIVE_QUEUES`; for each queue `0..active`, drain **up to 64 slots** (comment: "to ensure
  fairness" between busy and quiet threads).
- Per slot: if `ready[slot_idx] == 0`, stop this queue; else `idle = false` and:
  - **Timestamp reconstruction**: `delta_mach = slot.timestamp - INIT_MACH_TIME`;
    `delta_ns = delta_mach * TIMEBASE.numer / TIMEBASE.denom`;
    `current_usec = INIT_TIMEOFDAY_USEC + delta_ns/1000`; then `sec`/`usec`.
  - **Formatting** into a stack buffer (`buf = [0u8; 8192]`, `slice = &mut buf[..]`):
    - ECS: `gmtime_r` → `{"@timestamp":"YYYY-MM-DDTHH:MM:SS.mmmZ",<formatter>}` (millisecond precision).
    - JSON: `{"timestamp":"HH:MM:SS.ffffff",<formatter>}`.
    - plain: `[HH:MM:SS.ffffff] [mtdi] Caught <formatter>`.
    Each `writeln!` advances `slice`; the outer JSON braces are closed by the reader while the per-syscall
    `fmt_*` strings stay internally balanced — this split is what keeps NDJSON valid.
  - `len = buf_len - slice.len()` — the **written** length, derived from the buffer, never a constant.
    (It used to be `4096 - slice.len()` with an 8192-byte buffer, which underflowed: in debug it panicked
    the reader thread, in release it wrapped and every `write` returned EFAULT, silently discarding all
    events while the banner still appeared. Fixed by deriving from `buf_len`.)
  - `READER_WRITING = true`, `write(LOG_FD, buf, len)`, `READER_WRITING = false` — the guard that stops
    `my_write` from tracing the logger writing to itself.
  - `ready[slot_idx] = 0` (Release) — hand the slot back to the producer.
- When a full pass finds nothing (`idle`): if `SHUTDOWN`, set `READER_DONE` and break; else
  **sleep 1 ms**. (The code comment records the measurement: a 100 ns poll cost 1.5–4× wall-clock on
  unrelated work; 1 ms is invisible. An older README claim of a 100 ns sleep is stale.)

### 8.4 Shutdown and flush (`lib.rs:530-553`)

- `flush_on_exit()` (registered via `atexit`): if `getpid() != INIT_PID` → **return immediately** (a forked
  child has a COW copy of the ring and no reader thread; waiting would stall every child exit for the full
  budget). Otherwise set `SHUTDOWN` and wait up to `100 × 1 ms` for `READER_DONE`.
- `handle_terminate(_sig)` (launch mode only): `flush_on_exit()`, restore `SIG_DFL`, re-`raise` — so the
  process still dies with SIGTERM's normal semantics after its tail is drained.
- Consequence: a process that exits in under ~1 ms can still lose its tail if the reader is mid-sleep,
  which is why the `atexit` drain exists at all.

### 8.5 `mtdi_log` (`lib.rs:500`)

The sanctioned logging entry point for probe code and the dylib itself: null-check the pointer; if the
ring exists, `push_binary_event(fmt_raw, …)` (goes through the same pipeline, formatted by `fmt_raw`);
otherwise write directly to `LOG_FD` (used before the ring is set up), normalising a trailing newline.

### 8.6 JSON escaping — `JsonEscape` (`lib.rs:370`)

`Display` impl that escapes `"`, `\`, `\n`, `\r`, `\t`, and any control char `< 0x20` as `\uXXXX`.
Applied to every string that reaches JSON/ECS output (`fmt_open`, `fmt_stat`, `fmt_rename`, …).

---

## 9. The 25 built-in hooks (`src/lib.rs:455-871`)

Each syscall has: a filter **bit**, a `TRAMP_*: AtomicUsize`, a `my_*` handler, and a `fmt_*` formatter.

Most handlers are generated by `macro_rules! fastpath_hook!` (`lib.rs:701`), whose expansion is:

```rust
static $tramp: AtomicUsize = AtomicUsize::new(0);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn $handler($($arg: $ty),*) -> $ret {
    let tramp = $tramp.load(Relaxed);
    let orig: unsafe extern "C" fn(...) -> _ = transmute(tramp);
    if !should_log($bit) { return orig($($arg),*); }     // filter short-circuit
    push_binary_event($fmt, [$($pack),*], $s1, $s2);     // one ring write
    orig($($arg),*)                                      // forward through trampoline
}
```

| # | Syscall | Bit | Handler | Formatter | Notes |
|---|---|---|---|---|---|
| 1 | open | 0 | `my_open` | `fmt_open` | hand-written: swap-dylib override + mode-ABI caveat |
| 2 | close | 1 | `my_close` | `fmt_close` | macro |
| 3 | read | 2 | `my_read` | `fmt_read` | macro |
| 4 | write | 3 | `my_write` | `fmt_write` | hand-written: `READER_WRITING` self-trace guard |
| 5 | socket | 4 | `my_socket` | `fmt_socket` | macro |
| 6 | connect | 5 | `my_connect` | `fmt_connect` | macro |
| 7 | send | 6 | `my_send` | `fmt_send` | macro |
| 8 | recv | 7 | `my_recv` | `fmt_recv` | macro |
| 9 | stat | 8 | `my_stat` | `fmt_stat` | macro, path string |
| 10 | execve | 9 | `my_execve` | `fmt_execve` | macro, path string |
| 11 | fork | 10 | `my_fork` | `fmt_fork` | hand-written: logs parent only (`pid > 0`) |
| 12 | exit | 11 | `my_exit` | `fmt_exit` | hand-written: logs *before* forwarding (never returns) |
| 13 | mmap | 12 | `my_mmap` | `fmt_mmap` | macro, 6 args |
| 14 | munmap | 13 | `my_munmap` | `fmt_munmap` | macro |
| 15 | unlink | 14 | `my_unlink` | `fmt_unlink` | macro, path string |
| 16 | rename | 15 | `my_rename` | `fmt_rename` | macro, **two** strings |
| 17 | lstat | 16 | `my_lstat` | `fmt_lstat` | macro, path string |
| 18 | fstat | 17 | `my_fstat` | `fmt_fstat` | macro |
| 19 | bind | 18 | `my_bind` | `fmt_bind` | macro |
| 20 | listen | 19 | `my_listen` | `fmt_listen` | macro |
| 21 | accept | 20 | `my_accept` | `fmt_accept` | macro |
| 22 | sendto | 21 | `my_sendto` | `fmt_sendto` | macro, 6 args |
| 23 | recvfrom | 22 | `my_recvfrom` | `fmt_recvfrom` | macro, 6 args |
| 24 | mkdir | 23 | `my_mkdir` | `fmt_mkdir` | macro, octal mode |
| 25 | rmdir | 24 | `my_rmdir` | `fmt_rmdir` | macro, path string |

Why the four hand-written ones:
- **open** — needs the `MTDI_SWAP_DYLIB` override, which must run *before* the trampoline.
- **write** — must not trace itself when the reader writes to `LOG_FD` (infinite recursion otherwise).
- **fork** — the child inherits the ring via COW but has no reader thread; logging in the child would
  write into a buffer nobody drains.
- **exit** — the event has to be pushed *before* the real `exit` runs, because it never returns.

`should_log(bit)` is `(FILTER_MASK & (1 << bit)) != 0` — one relaxed load and an AND.

---

## 10. The FullContext path (thunk + dispatcher)

Used by probe code and available to any `HookType::FullContext` caller. `RegisterContext` (`thunk.rs:6`)
is the C-layout view of the saved stack frame:

```rust
#[repr(C)]
pub struct RegisterContext {
    pub x: [u64; 29],  // x0–x28    offset   0 (232 bytes)
    pub fp: u64,       // x29       offset 232
    pub lr: u64,       // x30       offset 240
    pub sp: u64,       // original SP
    pub cpsr: u64,     // NZCV
    pub hook_id: u64,  // trampoline address, delivered in x16
    pub q: [u128; 32], // v0–v31    offset 272 (512 bytes)
}
```

`_hook_thunk` (`thunk.rs:16`) — hand-written `global_asm!`:

1. `sub sp, sp, #512` then 16 × `stp qN, qN+1` → all 32 SIMD registers.
2. `sub sp, sp, #272` then 14 × `stp xN, xN+1` + `str x28` → x0–x28.
3. `stp x29, x30, [sp, #232]` → fp/lr.
4. `add x0, sp, #784 ; str x0, [sp, #248]` → the **original** SP (784 = 272 + 512, i.e. the value of SP
   before either `sub`).
5. `mrs x0, nzcv ; str x0, [sp, #256]` → flags.
6. `str x16, [sp, #264]` → the hook id that the stub loaded into x16.
7. `mov x0, sp ; bl _hook_dispatcher` — SP *is* the `&mut RegisterContext`.
8. Restore in reverse (nzcv via `msr`, fp/lr, x0–x28, `add sp, sp, #272`, q regs, `add sp, sp, #512`),
   then `br x16`.

Total: **784 bytes saved/restored per call** — ~49 cache lines of traffic. That is the dominant cost in
the measured ~15 ns. It is *not* strictly mandatory, and this document previously over-claimed by calling it
"physics": from the caller's point of view only `x19`–`x28`, `fp`, `sp`, and the low halves of `v8`–`v15` are
the callee's responsibility, so a minimal-save mode (`x0`–`x8` + `lr`) is a genuine optimization frontier —
and `FastPath` saves nothing at all. What makes the full save necessary *here* is the generality of
FullContext: handlers are arbitrary Rust code, which may clobber any caller-saved GPR and any SIMD register,
and the API lets them inspect and rewrite that state.

`hook_dispatcher(&mut RegisterContext)` (`thunk.rs:134`): locks `HOOKS`, looks up `ctx.hook_id`, calls the
handler, then sets `ctx.x[16] = hook_info.trampoline_addr` — which is how the thunk's final `br x16` ends up
in the trampoline (relocated prologue → branch back to `target + 16`). The `Mutex` acquire/release is on
every FullContext call, which is why 8-thread contention raises the mean to ~116 ns.

---

## 11. The probe engine — `mtdis` (`src/script/compiler.rs`)

This is the "write a small Rust probe and let mtdi inject it" path. It is a **second, independent
implementation of the hook engine**, generated as source text and compiled by `rustc` at trace time.

### 11.1 `compile_script(script_path, legacy_unwind)` (`compiler.rs:48`)

1. File must exist; read it as `user_code`.
2. Unless `-u`: `syn::parse_file(user_code)` → `SafetyVerifier.visit_file()` → on any error, return a
   formatted `[mtdis] Probe Verification Failed:` message listing every violation.
3. `DefaultHasher` over the user code → `code_hash` → deterministic paths
   `/tmp/mtdis_wrap_<hash>.rs` and `/tmp/mtdis_lib_<hash>.dylib` (content-addressed ⇒ identical probes
   reuse their dylib).
4. `generate_harness(user_code, legacy_unwind)` → written to the wrapper path.
5. `rustc --edition=2021 --crate-type cdylib -O` plus, when not legacy:
   `-C overflow-checks=off -C panic=abort`. Non-zero exit → return the compiler's stderr.
6. Return the dylib path (which `main.rs` then injects).

### 11.2 The AST verifier (`compiler.rs:13-60`) — what it actually bans

A `syn::visit::Visit` impl with five visitors. Verbatim rules:

| Visitor | Bans | Rationale in the code |
|---|---|---|
| `visit_expr_index` | **all** `foo[i]` | use `.get_safe(i)` (clamping) or `.get(i)` |
| `visit_expr_method_call` | `.unwrap()`, `.expect()` | "can panic" — use `.unwrap_or()`, `match`, `if let` |
| `visit_macro` | `panic!`, `assert!`, `assert_eq!`, `assert_ne!`, `todo!`, `unimplemented!`, `unreachable!` | "unconditional panic" |
| `visit_expr_call` | function calls named `unwrap`, `expect`, `panic_any`, `abort`, `exit`, `unreachable_unchecked` | "can panic or kill the process" |
| `visit_expr_binary` | `Div`, `Rem`, **`DivAssign`, `RemAssign`** | "dividing by zero panics even with overflow-checks off" |

The last two are the newest and the most important. They exist because of a demonstrated hole: probes using
`std::panic::panic_any`, `std::process::abort()`, or plain `a / b` all passed verification, and the
division case **panicked at runtime** (`attempt to divide by zero`) — because `-C overflow-checks=off`
removes overflow checks but **not** the divide-by-zero check. `-C panic=abort` then turned that panic into
a SIGABRT of the traced process. The fix bans the call names and (all four forms of) raw division; the
safe arithmetic route is `SafeU64::checked_div` / `checked_rem`.

`#[cfg(test)] mod tests` (`compiler.rs:661`) pins this behaviour with six tests:
`accepts_clean_probe`, `rejects_unwrap_and_expect`, `rejects_raw_indexing`, `rejects_panicking_macros`,
`rejects_raw_division_and_modulo` (expects 4 violations), `rejects_process_killing_calls`.
`cargo test` is the canonical suite.

### 11.3 The generated harness (`generate_harness`, `compiler.rs:127`)

The wrapper source contains, in order:

1. A **private copy of `RegisterContext`** and a **private copy of `_hook_thunk`** — the same 784-byte
   save/restore asm, duplicated as text.
2. Externs for `mach_task_self`, `mach_vm_protect`, `mach_vm_allocate`, `mach_vm_read_overwrite`,
   `sys_icache_invalidate`, `dlsym`; plus `RTLD_DEFAULT`, `VM_*` constants.
3. `unprotect_page` / `protect_page` (16 KB granularity, `VM_PROT_COPY` included).
4. **`allocate_near(target_addr, size)`** — tries a probe list of offsets from the target
   (±0x10000, ±0x20000, ±0x40000, ±0x80000, ±0x100000, ±0x400000, ±0x1000000) and returns the first
   `mach_vm_allocate` that lands; falls back to `VM_FLAGS_ANYWHERE`. Proximity matters here because of
   the next item.
5. `can_branch_26` / `make_branch_26` — a ±128 MB reachability test and a 26-bit `B` encoder.
6. `is_pc_relative` + `relocate_instruction` + `build_trampoline` — a **second copy** of the relocator
   (same six classes). Differences from `src/hook/trampoline/relocator.rs`: the fallback for an
   unrecognised PC-relative instruction is to **copy it verbatim** instead of panicking, and
   `build_trampoline` scans the stolen instructions for `ret` (0xD65F03C0) and **omits the branch-back**
   if the stolen window already returns.
7. **`raw_install_hook(target_addr, handler)`** — the interesting one:
   - allocate a 32-byte stub **near** the target;
   - `is_near = can_branch_26(...)` → if yes the detour steals only **4 bytes** (one instruction) and
     writes a single 4-byte `B` to the stub, else it steals 16 bytes and writes the 16-byte
     `ldr x17,#8 ; br x17 ; .dword stub` sequence;
   - the stub is `ldr x16,#12 ; ldr x17,#16 ; br x17` followed by `<trampoline>`, `<hook_thunk>`;
   - then `get_hooks().lock()…insert(trampoline_addr, HookInfo{ trampoline_addr, handler })`.
   So the probe path can cost as little as one 4-byte branch per hook when the allocation lands close.
8. `MtdiSafeContext`: `arg(i)`/`set_arg(i,v)` (x0–x7, bounds-checked, returns 0 out of range),
   `return_val()`/`set_return_val(v)` (x0), and `read_arg_str(i, max_len) -> Option<String>` which
   `mach_vm_read_overwrite`s the pointer, rejects read failures/empty reads, truncates at the first NUL,
   and validates UTF-8.
9. `MtdiRegistry::hook_symbol(symbol, handler)` — the registration API.
10. `mod user_sandbox { #![forbid(unsafe_code)] … }` containing `SafeU64` (`add`/`sub`/`mul` are
    `wrapping_*`; `checked_div`/`checked_rem` return 0 on a zero divisor), `SafeSliceExt::get_safe`
    for slices and arrays, and then **the user's code**, inlined.
11. `hook_dispatcher` — copies `(trampoline, handler)` out under the lock, sets `ctx.x[16]`, then runs the
    handler behind a **thread-local `HOOK_DEPTH` re-entrancy guard** (`depth > 0` ⇒ return immediately),
    so a handler that triggers the same syscall cannot recurse forever. In `-u` mode the handler body is
    wrapped in `catch_unwind(AssertUnwindSafe(...))`; in safe mode it is not (panics are supposed to be
    impossible, and `panic=abort` makes them fatal).
12. `mtdis_init`, installed as the dylib's `__mod_init_func`: builds an `MtdiRegistry`, calls the user's
    `register()`, and for each `(symbol, handler)` does `dlsym(RTLD_DEFAULT, symbol)` — retrying with a
    leading `_` — then `raw_install_hook`. Missing symbols print
    `[mtdis] Warning: Symbol '<name>' not found in process.` and are skipped; failures print
    `[mtdis] Failed to hook <name>: <err>`.

### 11.4 `-u` / `--legacy-unwind`

Skips step 2 (no AST pass) and drops `-C overflow-checks=off -C panic=abort`, wrapping dispatch in
`catch_unwind` instead. Slower, but it is the documented escape hatch for probes the verifier rejects.

### 11.5 `-c` / `--check-only`

Compiles and verifies only (never injects) — used by the MCP server so an agent can validate probe syntax
without touching a process.

---

## 12. Behaviour worth knowing (hazards, limits, and honest sharp edges)

### 12.1 The PPL subset

On recent macOS, the kernel seals a subset of `libsystem_kernel`'s text pages. The fork probe returns
`false` for those pages, `install_hook` fails, and the ctor prints `[mtdi] Hook skipped for <name>:
page at 0x… is PPL-protected (write probe faulted)`. Launch-time injection **can** hook more than attach,
because dyld loads the dylib before the target's `main()` (and the pages are writable in that window).
The README's "8 of 25" figure is a snapshot of one macOS build; the code imposes no fixed number, it just
skips what the probe rejects. The fork-probe children that die on sealed pages are recorded by macOS as
`mtdi_lib-*.ips` reports — expected, not a crash of the traced process.

### 12.2 Two hook installers, one engine

`src/hook/manager.rs` (built-in 25, FastPath only) and the generated harness's `raw_install_hook`
(probe path, FullContext only, plus `allocate_near` and the 4-byte branch short-cut) implement the same
concept independently, including **duplicated relocators**. They have already diverged once in
behaviour: the probe copy copies unhandled instructions verbatim while the core copy panics. Any fix to
one must be applied to the other.

### 12.3 Relocator fault lines

- `relocator.rs:150` panics on any PC-relative form outside the six classes. It has never fired on the
  current 25 hooks (their prologues are `pacibsp` + non-PC-relative instructions), but it is a landmine
  for new targets.
- The ADR/ADRP mask (`0x9F000000`) must keep covering **both** `0x10000000` and `0x90000000`.
  The probe copy of `is_pc_relative` uses a *different* test (`& 0x1F000000 == 0x10000000`), which is
  worth auditing if the two ever need to agree.

### 12.4 Silent loss

`push_binary_event` drops events when a queue is full and there is **no drop counter**. Under a syscall
storm, the trace is incomplete with no indication. (The same holds for threads beyond `MAX_THREADS = 128`:
the event is dropped.) A single atomic counter would make the loss visible.

### 12.5 Environment inheritance

`spawn.rs` sets `DYLD_INSERT_LIBRARIES` with `cmd.env(...)`, which **overwrites** any value the user had
set. Consequence: you cannot stack a second injected dylib that way — the target gets only the engine.
(Verified: injecting a test dylib by pre-setting the variable works for the CLI process itself, but the
child receives only `libmtdi_lib.dylib`.)

### 12.6 The trampoline allocator ignores its own allocation result

`allocator.rs:18` calls `mach_vm_allocate(...)` **without checking the return value**. If the allocation
fails, `addr` stays `0`, the `compare_exchange(0, 0)` "succeeds", and `TRAMPOLINE_BASE` is latched to `0`
forever — after which `allocate_trampoline` hands out `base + offset`, i.e. tiny addresses near NULL, and
`build_trampoline` writes the relocated prologue there. The only guard is
`offset + size > PAGE_SIZE → panic!("Out of trampoline memory!")`, which is a *capacity* check, not a
*validity* check. `inject_into_pid` checks every one of its six Mach returns; this one does not.
Cheap fix: check `kr == KERN_SUCCESS` and, on failure, either retry or panic with the kernel code.

### 12.7 Fork and the ring

A forked child gets a COW copy of the ring and **no reader thread**; `flush_on_exit` returns immediately
for non-`INIT_PID` processes so children never stall. `my_fork` therefore only logs in the parent.

### 12.8 FullContext costs a mutex

`hook_dispatcher` takes the global `HOOKS` lock on every FullContext call. Measured: ~15–16 ns warm
(uncontended), ~116–121 ns mean under 8-thread contention. FastPath has no lock at all and stays at
~3.5–5.7 ns under the same load. If the FullContext path ever matters for multi-threaded targets, an
immutable-map + atomic-pointer read is the obvious fix.

### 12.9 Virtual memory

144 MiB of VA per traced process (128 queues × 1024 slots × 1152 B), zero-filled on demand. Physical
use tracks actual event flow.

### 12.10 Timer granularity

`mach_absolute_time()` on Apple Silicon ticks at 24 MHz (41.67 ns). Per-event cold measurements are
therefore quantised to ~42 ns; sub-tick results read as 0. Timestamps are reconstructed from mach deltas
anchored to one `gettimeofday`, so no syscall is made per event. The README's microsecond-precision
output is cosmetic relative to that 42 ns source.

### 12.11 The ring buffer's actual bargain

The entire pipeline in §8 exists to keep **one** thing off the traced thread: format-then-`write`. Five
reasons, in order of importance:

1. **Cost** — the detour is ~1.6 ns; a `write(2)` is ~1–2 µs. Inline logging would make the tracer ~1000×
   the thing it is measuring.
2. **Blocking** — a pipe or file write can block, which would make the traced thread's latency a function
   of the log consumer. A tracer that perturbs timing is measuring itself.
3. **Self-tracing** — `write` is one of the 25 hooked calls, so a write issued from inside a hook is itself
   an event to exclude. Hence `READER_WRITING` and `my_write`'s guard.
4. **Formatting** — `gmtime_r` + JSON escaping + `writeln!` costs hundreds of ns of CPU per event, which for
   small records exceeds the syscall. The ring offloads the formatting, not just the trap.
5. **Contention** — a shared fd serializes all threads on one file lock; per-thread queues never contend.

**But the ring does not eliminate `write(2)` — it relocates it.** `libc::write` still appears six times in
the dylib: three in the init banner (`lib.rs:244/247/250`), one **per event** in the reader (`lib.rs:331`),
and two in `mtdi_log`'s pre-ring fallback (`lib.rs:510/513`). The reader performs **no batching** — there is
no accumulation buffer and no flush helper anywhere in the file; the only "flush" is the `atexit` drain. So
the process's syscall count is not reduced, it strictly *increases* by one write per logged event. What the
ring actually buys is isolation, ordering, and formatting offload.

The consequence is a quantified design bargain: the reader costs ~1.5–2.5 µs per event (format + write),
while the producer costs ~20–50 ns. That is roughly a 50× mismatch. A 1024-slot queue fills in ~50–100 µs
and needs ~1 ms to drain, so any sustained rate beyond roughly 400k events/s is **by construction** dropping
events — silently, with no counter (§12.4). Completeness was traded for "the traced thread never pays," and
the 144 MiB reservation is the price of never blocking regardless of how bursty or wide the target is.

---

## 13. Measured performance

From `src/bin/bench.rs` (warm, 1 M iterations, no-op handler) and `src/bin/bench_cold.rs`
(32 MB of random-code thrash + 64 MB data thrash between timed calls):

| Scenario | FullContext | FastPath |
|---|---|---|
| Warm loop | 15–16 ns | 1.6 ns |
| Cold, p50 | 42–83 ns | < 1 tick (≤ 42 ns) |
| Cold, p99 | 208–792 ns | 42–125 ns |
| Cold, max | 542 ns–7 µs (scheduler noise) | 125–333 ns |
| First call after install | ~1,167–1,750 ns | n/a |
| 8-thread contention, mean | 116–125 ns | 3.5–5.7 ns |

On a real syscall the tracer adds ~175 ns (filtered) to ~360 ns (logged) on top of a ~4.1 µs
`open("/dev/null")` — single-digit percent. The FastPath's 1.6 ns is at the floor of what an inline
detour can cost on this silicon: one branch plus a few L1-resident loads.

---

## 14. MCP server (`src/mcp_server.py`)

A `fastmcp` server exposing the engine to agents.

**Binary discovery** (`_mtdi_binary()`): `$MTDI_BIN` → `<repo>/target/release/mtdi` → `PATH`. It also
records the binary's mtime and **raises** if it changes mid-session ("The mtdi binary was rebuilt since
this MCP server started (stale engine). Restart the MCP server"), which prevents an agent from driving a
stale engine.

| Tool | What it does |
|---|---|
| `list_processes(query="")` | `ps -eo pid,comm`, filtered |
| `check_probe_syntax(script_code, legacy_unwind=False)` | writes the probe to a temp file, runs `mtdi [-u] --check-only -s <file>`, returns stdout/stderr |
| `trace_process(target, script_code, duration_seconds=5, legacy_unwind=False)` | `mtdi [-u] -s <probe> <target>` or `-p <pid>` if the target is numeric; terminates after the duration; in attach mode also reads back `$TMPDIR/mtdi_<pid>.log` and appends it |
| `enumerate_modules(pid, filter_query="")` | `vmmap -p <pid>` (chosen over `lsof`: it sees the main binary and every mapped image), extracts `.dylib`/`.framework` paths |
| `enumerate_exports(binary_path, filter_query="")` | `nm -gU` |
| `demangle_symbol(symbol)` | `swift demangle --compact` for `_$s`/`$s`, else `c++filt` |
| `launch_with_dyld(target_executable, dylib_path, args=[], duration_seconds=5)` | `Popen` with `DYLD_INSERT_LIBRARIES` |

**Resources**: `mtdi://docs/workflow` (the recommended agent loop), `mtdi://docs/ast_rules` (the five ban
classes, the allowed surface, and the probe API), `mtdi://examples/syscall`, `mtdi://examples/uprobe`.

---

## 15. File map

```
src/
  main.rs                     entry point: dylib resolution + mode dispatch (77)
  lib.rs                      the injected dylib: ctor, 25 hooks, ring buffer, reader, formatters (871)
  cli/args.rs                 hand-rolled arg parser + 25-name filter validation + help (149)
  cli/spawn.rs                SIP / codesign / arm64e pre-flight, env contract, spawn + signals (126)
  injector/mach.rs            live attach: task_for_pid, remote alloc, bootstrap stub, ack polling (340)
  hook/mod.rs                 page write-probe (fork), unprotect/protect, overwrite_with_jump (131)
  hook/manager.rs             install_hook, HookType, HOOKS registry (85)
  hook/trampoline/thunk.rs    784-byte register save + dispatcher (143)
  hook/trampoline/builder.rs  relocate stolen prologue + branch back (45)
  hook/trampoline/relocator.rs six PC-relative instruction classes (151)
  hook/trampoline/disasm.rs   is_pc_relative (23)
  hook/trampoline/allocator.rs single 16 KB bump page for trampolines (37)
  script/compiler.rs          probe compiler: AST verifier, harness generation, probe-side hooking (763)
  mcp_server.py               MCP tools + resources (368)
  bin/bench.rs                warm overhead microbenchmark (105)
  bin/bench_cold.rs           worst-case battery: thrash, first-call, contention (286)
probes/                       probe templates + the historical hook-count bisect series + bug-hunt probe
examples/                     C harnesses: victim/victim2, heavy_bench, h_open, syscall_bench, and
                              Frida/Dobby comparison harnesses (frida_*.js/py, dobby_*.c)
```

Environment contract (set by the CLI, read by the dylib): `DYLD_INSERT_LIBRARIES`, `MTDI_OUTPUT`,
`MTDI_FILTER`, `MTDI_JSON`, `MTDI_ECS`, `MTDI_OWN_SIGTERM`, `MTDI_SWAP_DYLIB`.

## 16. Glossary

- **FastPath** — detour straight to a C handler; no register save; ~1.6 ns.
- **FullContext** — detour to the asm thunk, which spills all GPRs/SIMD flags (784 B), calls a Rust
  dispatcher, restores, and continues through the trampoline; ~15 ns.
- **Trampoline** — executable memory holding relocated prologue instructions plus a branch back.
- **Thunk** — the hand-written asm save/restore/dispatch routine.
- **Ring / queue** — the per-thread SPSC `ThreadQueue` written by handlers and drained by the reader.
- **Slot** — one 1152-byte event record, including its own formatter pointer and inline strings.
- **PPL** — Apple's Page Protection Layer; seals some kernel-cache text pages so they cannot be patched.
- **mtdis** — the probe sandbox/compiler subsystem (`-s` mode).
- **SafeU64 / get_safe** — the sandbox's panic-free arithmetic and bounds-clamping helpers.
