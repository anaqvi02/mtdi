#!/usr/bin/env python3
"""Measure Frida's per-call interceptor overhead vs mtdi on the same target."""
import subprocess, os, time, sys

DIR = os.path.dirname(os.path.abspath(__file__))
BENCH_SRC = os.path.join(DIR, "frida_bench.c")
BENCH_BIN = os.path.join(DIR, "frida_bench")
AGENT_JS  = os.path.join(DIR, "frida_bench_agent.js")

def compile():
    subprocess.run(["gcc", "-O2", "-o", BENCH_BIN, BENCH_SRC], check=True)

def run_native():
    r = subprocess.run([BENCH_BIN], capture_output=True, text=True)
    line = r.stdout.strip()
    ns = float(line.split()[0])
    return ns, line

def run_with_frida():
    """Spawn the binary under Frida with the interceptor hook, let it run to completion,
    capture its stdout (the self-reported timing)."""
    import frida

    pid = frida.spawn([BENCH_BIN])
    session = frida.attach(pid)

    with open(AGENT_JS) as f:
        agent = f.read()

    script = session.create_script(agent)
    script.load()
    frida.resume(pid)

    # Wait for process to exit naturally (the binary runs 1M iterations then exits)
    # frida.get_process() will throw when the process is gone
    for _ in range(300):  # 30s max
        try:
            frida.get_process(pid)
            time.sleep(0.1)
        except:
            break

    session.detach()

    # Now re-run natively but intercept stdout via a wrapper
    # Actually, frida.spawn captures the child — we can't get its stdout.
    # So we'll use a different strategy: use frida-trace CLI which shows timing.

    return None

def run_frida_trace():
    """Use frida-trace CLI to hook target_func and let the binary self-time."""
    import subprocess, signal

    # Start the binary in background, attach frida-trace, let it run
    proc = subprocess.Popen(
        [BENCH_BIN],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )

    # Give it a moment to start
    time.sleep(0.05)

    # Attach frida-trace to the running process
    trace = subprocess.Popen(
        ["frida-trace", "-p", str(proc.pid), "-i", "target_func", "-q"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )

    # Wait for the binary to finish
    stdout, stderr = proc.communicate(timeout=30)
    trace.terminate()
    trace.wait()

    ns = float(stdout.strip().split()[0])
    return ns, stdout.strip()

def run_frida_python_api():
    """Use Frida Python API: spawn, hook, resume, wait for exit, get timing via pipe."""
    import frida

    # We need to capture the binary's stdout.
    # Strategy: redirect the binary's output to a file via a tiny wrapper.
    wrapper_src = os.path.join(DIR, "frida_bench_wrapper.c")
    with open(wrapper_src, "w") as f:
        f.write(f"""
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {{
    // Redirect stdout to a temp file
    char *tmp = "/tmp/frida_bench_out.txt";
    freopen(tmp, "w", stdout);
    return system("{BENCH_BIN}");
}}
""")
    wrapper_bin = os.path.join(DIR, "frida_bench_wrapper")
    subprocess.run(["gcc", "-O2", "-o", wrapper_bin, wrapper_src], check=True)

    pid = frida.spawn([wrapper_bin])
    session = frida.attach(pid)

    with open(AGENT_JS) as f:
        agent = f.read()

    script = session.create_script(agent)
    script.load()
    frida.resume(pid)

    # Wait for process to exit
    for _ in range(300):
        try:
            frida.get_process(pid)
            time.sleep(0.1)
        except:
            break

    session.detach()

    # Read the output
    time.sleep(0.2)
    try:
        with open("/tmp/frida_bench_out.txt") as f:
            output = f.read().strip()
        ns = float(output.split()[0])
        return ns, output
    except:
        return None, "could not read output"

if __name__ == "__main__":
    compile()

    print("=" * 60)
    print("  Frida Interceptor Overhead Benchmark")
    print("  1M iterations of a 5-nop + ret function")
    print("=" * 60)

    # Native baseline
    print("\n--- Native baseline (10 runs) ---")
    natives = []
    for i in range(10):
        ns, line = run_native()
        natives.append(ns)
    native_avg = sum(natives) / len(natives)
    native_min = min(natives)
    native_max = max(natives)
    print(f"  avg: {native_avg:.2f}  min: {native_min:.2f}  max: {native_max:.2f} ns/call")

    # Frida via Python API
    print("\n--- Frida Interceptor (5 runs) ---")
    frida_vals = []
    for i in range(5):
        ns, line = run_frida_python_api()
        if ns is not None:
            frida_vals.append(ns)
            print(f"  run {i+1}: {line}")
        else:
            print(f"  run {i+1}: FAILED ({line})")

    if frida_vals:
        frida_avg = sum(frida_vals) / len(frida_vals)
        frida_min = min(frida_vals)
        frida_max = max(frida_vals)
        overhead = frida_avg - native_avg
        print(f"\n  Frida avg: {frida_avg:.2f}  min: {frida_min:.2f}  max: {frida_max:.2f} ns/call")
        print(f"  Overhead:  {overhead:.2f} ns/call ({overhead/native_avg*100:.1f}% of baseline)")
        print(f"  Ratio:     {frida_avg/native_avg:.1f}x slower than native")

    print("\n--- mtdi reference (from README) ---")
    print("  FastPath:   ~1.6 ns/call overhead")
    print("  FullContext: ~15 ns/call overhead")

    if frida_vals:
        print(f"\n{'='*60}")
        print(f"  mtdi FastPath ({1.6:.1f} ns) vs Frida ({overhead:.1f} ns)")
        print(f"  mtdi is {overhead/1.6:.0f}x FASTER than Frida on the hook path")
        print(f"{'='*60}")
