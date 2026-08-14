import subprocess
import tempfile
import os
import time
import shutil
from fastmcp import FastMCP

mcp = FastMCP("MTDI")


def _mtdi_binary() -> str:
    """Locate the mtdi CLI: $MTDI_BIN, then <repo>/target/release/mtdi, then PATH."""
    env = os.environ.get("MTDI_BIN")
    if env and os.path.exists(env):
        return env
    repo_relative = os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "target",
        "release",
        "mtdi",
    )
    if os.path.exists(repo_relative):
        return repo_relative
    found = shutil.which("mtdi")
    if found:
        return found
    raise FileNotFoundError(
        "mtdi binary not found. Build it with `cargo build --release` in the repo "
        "root, or point $MTDI_BIN at the built binary."
    )

@mcp.tool()
def list_processes(query: str = "") -> str:
    """
    Lists active macOS processes to find a target PID.
    Args:
        query: Optional string to filter the process list (e.g. 'Safari', 'Spotify').
    """
    try:
        result = subprocess.run(["ps", "-eo", "pid,comm"], capture_output=True, text=True)
        lines = result.stdout.splitlines()
        if not query:
            return "\n".join(lines[:50]) + "\n... (use a query to filter)"
        
        matches = [lines[0]]
        query_lower = query.lower()
        for line in lines[1:]:
            if query_lower in line.lower():
                matches.append(line)
        return "\n".join(matches)
    except Exception as e:
        return f"Error listing processes: {str(e)}"

@mcp.tool()
def check_probe_syntax(script_code: str, legacy_unwind: bool = False) -> str:
    """
    Validates the Rust probe script syntax and AST constraints without running a live trace.
    Args:
        script_code: The Rust probe code to compile.
        legacy_unwind: If True, uses the -u flag to bypass AST verification.
    """
    return trace_process(target="/bin/ls", script_code=script_code, duration_seconds=0, legacy_unwind=legacy_unwind)

@mcp.tool()
def trace_process(target: str, script_code: str, duration_seconds: int = 5, legacy_unwind: bool = False) -> str:
    """
    Traces a macOS process by injecting a Rust probe script using the MTDI engine.
    
    Args:
        target: The executable path or PID to trace.
        script_code: The Rust probe code to compile and inject.
        duration_seconds: How many seconds to trace before gracefully stopping. (0 = check compilation only)
        legacy_unwind: If True, uses the -u flag to bypass AST verification and enable panicking.
    """
    with tempfile.NamedTemporaryFile(mode='w', suffix='.rs', delete=False) as f:
        f.write(script_code)
        script_path = f.name
        
    try:
        cmd = [_mtdi_binary()]
        if legacy_unwind:
            cmd.append("-u")
        cmd.extend(["-s", script_path, str(target)])
        
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )
        
        if duration_seconds > 0:
            time.sleep(duration_seconds)
        else:
            time.sleep(2) # Give it 2 seconds to compile for check_probe_syntax
        
        proc.terminate()
        stdout, stderr = proc.communicate(timeout=2)
        
        return f"STDOUT:\n{stdout}\n\nSTDERR:\n{stderr}"
    except Exception as e:
        return f"Error tracing process: {str(e)}"
    finally:
        if os.path.exists(script_path):
            os.unlink(script_path)

@mcp.tool()
def enumerate_modules(pid: int, filter_query: str = "") -> str:
    """
    Lists loaded dynamic libraries (modules) for a running process, similar to Frida's Process.enumerateModules().
    Args:
        pid: The process ID to inspect.
        filter_query: Optional string to filter the modules (e.g. 'libSystem').
    """
    try:
        result = subprocess.run(["lsof", "-p", str(pid)], capture_output=True, text=True)
        lines = [line.split()[-1] for line in result.stdout.splitlines() if '.dylib' in line or '.framework' in line]
        unique_lines = sorted(list(set(lines)))
        
        if filter_query:
            unique_lines = [l for l in unique_lines if filter_query.lower() in l.lower()]
            
        if not unique_lines:
            return f"No matching modules found for PID {pid}."
            
        return "Loaded Modules:\n" + "\n".join(unique_lines[:100])
    except Exception as e:
        return f"Error enumerating modules: {str(e)}"

@mcp.tool()
def enumerate_exports(binary_path: str, filter_query: str = "") -> str:
    """
    Lists exported symbols from a binary or dylib, similar to Frida's Module.enumerateExports().
    Args:
        binary_path: Absolute path to the binary or .dylib.
        filter_query: Optional string to filter the exports (e.g. 'open').
    """
    if not os.path.exists(binary_path):
        return f"Error: File not found: {binary_path}"
    try:
        result = subprocess.run(["nm", "-gU", binary_path], capture_output=True, text=True)
        lines = result.stdout.splitlines()
        
        exports = []
        for line in lines:
            parts = line.split()
            if len(parts) >= 3:
                symbol = parts[-1]
                exports.append(symbol)
        
        if filter_query:
            exports = [s for s in exports if filter_query.lower() in s.lower()]
            
        if not exports:
            return "No matching exports found."
            
        return "Exported Symbols:\n" + "\n".join(exports[:200])
    except Exception as e:
        return f"Error enumerating exports: {str(e)}"

@mcp.tool()
def demangle_symbol(symbol: str) -> str:
    """
    Demangles a Swift or C++ symbol into human-readable format.
    """
    try:
        if symbol.startswith("_$s") or symbol.startswith("$s"):
            result = subprocess.run(["swift", "demangle", "--compact", symbol], capture_output=True, text=True)
            return result.stdout.strip()
        else:
            result = subprocess.run(["c++filt", symbol], capture_output=True, text=True)
            return result.stdout.strip()
    except Exception as e:
        return f"Error demangling: {str(e)}"

@mcp.tool()
def launch_with_dyld(target_executable: str, dylib_path: str, args: list[str] = [], duration_seconds: int = 5) -> str:
    """
    Launches an application with a dynamic library injected via DYLD_INSERT_LIBRARIES.
    
    Args:
        target_executable: The absolute path to the executable to launch.
        dylib_path: The absolute path to the .dylib to inject.
        args: Optional list of command-line arguments to pass to the executable.
        duration_seconds: How many seconds to let the process run before gracefully terminating (0 means wait indefinitely).
    """
    if not os.path.exists(target_executable):
        return f"Error: Target executable not found: {target_executable}"
    if not os.path.exists(dylib_path):
        return f"Error: Dylib not found: {dylib_path}"
        
    env = os.environ.copy()
    env["DYLD_INSERT_LIBRARIES"] = dylib_path
    
    cmd = [target_executable] + args
    
    try:
        proc = subprocess.Popen(
            cmd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )
        
        if duration_seconds > 0:
            time.sleep(duration_seconds)
            proc.terminate()
            try:
                stdout, stderr = proc.communicate(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                stdout, stderr = proc.communicate()
        else:
            stdout, stderr = proc.communicate()
            
        return f"STDOUT:\n{stdout}\n\nSTDERR:\n{stderr}"
    except Exception as e:
        return f"Error launching process with DYLD_INSERT_LIBRARIES: {str(e)}"

@mcp.resource("mtdi://docs/workflow")
def ai_workflow() -> str:
    """Instructions on how an AI should interact with the MTDI MCP server."""
    return '''# MTDI AI Interaction Workflow
As an AI agent, follow this precise workflow to use MTDI successfully:

1. **Find the Target**: Use `list_processes(query)` to find the PID.
2. **Reconnaissance (Optional)**: Use `enumerate_modules(pid)` to find loaded libraries, and `enumerate_exports(binary_path)` to find hookable functions. Use `demangle_symbol` if the function is a Swift/C++ symbol.
3. **Understand the Rules**: Read `mtdi://docs/ast_rules` to ensure your Rust code won't trigger the Zero-Panic AST verifier.
4. **Read the Templates**: Read `mtdi://examples/syscall` or `mtdi://examples/uprobe` to see how to use the `MtdiSafeContext` / `MtdiRegistry` API.
5. **Validate**: Call `check_probe_syntax(script_code)` with your generated Rust code. Fix any compilation or AST errors it returns!
6. **Execute**: Call `trace_process(target, script_code)` or `launch_with_dyld(target_executable, dylib_path)`.
'''

@mcp.resource("mtdi://docs/ast_rules")
def ast_rules() -> str:
    """Rules for writing zero-panic MTDI probes."""
    return '''# MTDI Zero-Panic AST Rules
When `legacy_unwind` is False, the engine enforces zero-panic code by walking your probe's AST before compiling it. What the verifier actually bans:
1. `.unwrap()` / `.expect()` (method calls AND bare function calls). Use `match`, `if let`, or `.unwrap_or()`.
2. Array indexing `foo[i]`. Use `.get_safe(i)` (harness trait, clamps out-of-bounds) or `.get(i)`.
3. The macros `panic!`, `assert!`, `assert_eq!`, `assert_ne!`, `todo!`, `unimplemented!`, `unreachable!`.
4. The functions `panic_any`, `abort`, `exit`, `unreachable_unchecked` (e.g. `std::process::abort()`).
5. Raw integer division `/` and modulo `%` — dividing by zero panics even with `overflow-checks=off`. Use `SafeU64::checked_div()` / `SafeU64::checked_rem()`.

What is allowed (the verifier does NOT check these): `for`/`while`/`loop`, Vec/String/HashMap allocation, recursion, and integer arithmetic (`+`, `-`, `*`, `<<`, `>>` — wrapping under `overflow-checks=off`, like C on arm64). The harness compiles your code with `-C overflow-checks=off` and `-C panic=abort` — if anything panics anyway, the whole traced process aborts. Don't write code that panics.

Your probe code runs inside a module with `#![forbid(unsafe_code)]` — no `unsafe` blocks.

The real probe API (see `mtdi://examples/syscall`):
- Define `pub fn register(reg: &mut MtdiRegistry)` and register handlers with `reg.hook_symbol("open", on_open)`.
- Handlers have the signature `fn(&mut MtdiSafeContext)`.
- Context API: `arg(i)` / `set_arg(i, v)` (registers x0-x7), `return_val()` / `set_return_val(v)`, `read_arg_str(i, max_len) -> Option<String>`.
'''

@mcp.resource("mtdi://examples/syscall")
def syscall_example() -> str:
    """Boilerplate for a syscall hook."""
    return '''// MTDI probe: hook open() and log the path.
// The engine wraps this in a #![forbid(unsafe_code)] module and verifies
// the AST (see mtdi://docs/ast_rules) before compiling with -C panic=abort.

pub fn on_open(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 256) {
        println!("[mtdis probe] Intercepted open() -> path: \\"{}\\", flags: {:#x}", path, ctx.arg(1));
    }
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("open", on_open);
}
'''

@mcp.resource("mtdi://examples/uprobe")
def uprobe_example() -> str:
    """Boilerplate for an uprobe function hook."""
    return '''// MTDI probe: hook an arbitrary function by symbol name.

pub fn on_target(ctx: &mut MtdiSafeContext) {
    // ctx.arg(0) is the first argument of the hooked function
    println!("[mtdis probe] target_function called, arg0 = {:#x}", ctx.arg(0));
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("strlen", on_target);
}
'''

@mcp.prompt()
def write_probe(target_function: str, goal: str) -> str:
    """Creates a prompt to write a new MTDI probe."""
    return f'''Please write a MTDI probe script to hook `{target_function}`.
Your goal is: {goal}

Constraints:
- Return ONLY valid Rust code.
- Read `mtdi://docs/ast_rules` first!
- Follow the Zero-Panic AST Rules (no unwrap/expect/panic/assert, no `[]` indexing, no unsafe).
- Define `pub fn register(reg: &mut MtdiRegistry)` and handlers with signature `fn(&mut MtdiSafeContext)`, registered via `reg.hook_symbol("<symbol>", handler)`.
- Use the context API: `ctx.arg(i)`, `ctx.set_arg(i, v)`, `ctx.return_val()`, `ctx.set_return_val(v)`, `ctx.read_arg_str(i, max_len)`.
- See `mtdi://examples/syscall` for a template.
'''

if __name__ == "__main__":
    mcp.run()
