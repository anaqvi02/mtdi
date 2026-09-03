#!/usr/bin/env python3
"""measure frida's per-call interceptor overhead vs mtdi on the same target."""
import subprocess, os, time

DIR = os.path.dirname(os.path.abspath(__file__))
BENCH_SRC = os.path.join(DIR, "frida_bench2.c")
BENCH_BIN = os.path.join(DIR, "frida_bench2")
AGENT_JS  = os.path.join(DIR, "frida_bench_agent.js")
RESULT_FILE = "/tmp/frida_bench_result.txt"

def compile():
    subprocess.run(["gcc", "-O2", "-o", BENCH_BIN, BENCH_SRC], check=True)

def run_native():
    r = subprocess.run([BENCH_BIN], capture_output=True, text=True)
    line = r.stdout.strip()
    ns = float(line.split()[0])
    return ns, line

def run_frida_bench():
    """spawn the bench directly under frida, hook target_func, read the result file.
    stdout of a frida-spawned child is not capturable, so the bench writes its
    timing to /tmp/frida_bench_result.txt."""
    import frida

    try:
        os.unlink(RESULT_FILE)
    except FileNotFoundError:
        pass

    pid = frida.spawn([BENCH_BIN])
    session = frida.attach(pid)

    with open(AGENT_JS) as f:
        agent = f.read()
    script = session.create_script(agent)
    script.load()
    frida.resume(pid)

    # wait for the bench to finish and exit
    for _ in range(300):
        try:
            frida.get_process(pid)
            time.sleep(0.1)
        except:
            break

    session.detach()
    time.sleep(0.3)
    try:
        with open(RESULT_FILE) as f:
            return float(f.read().strip())
    except Exception:
        return None

if __name__ == "__main__":
    compile()

    print("=" * 60)
    print("  Frida Interceptor Overhead Benchmark")
    print("  1M iterations of a 5-nop + ret function")
    print("=" * 60)

    # native baseline
    print("\n--- Native baseline (10 runs) ---")
    natives = []
    for i in range(10):
        ns, line = run_native()
        natives.append(ns)
    native_avg = sum(natives) / len(natives)
    native_min = min(natives)
    native_max = max(natives)
    print(f"  avg: {native_avg:.2f}  min: {native_min:.2f}  max: {native_max:.2f} ns/call")

    # frida, JS-callback Interceptor.attach (the default usage)
    print("\n--- Frida Interceptor (5 runs) ---")
    frida_vals = []
    for i in range(5):
        ns = run_frida_bench()
        if ns is not None:
            frida_vals.append(ns)
            print(f"  run {i+1}: {ns:.2f} ns/call")
        else:
            print(f"  run {i+1}: FAILED")

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

    if frida_vals and overhead > 0:
        print(f"\n{'='*60}")
        print(f"  mtdi FastPath ({1.6:.1f} ns) vs Frida ({overhead:.1f} ns)")
        print(f"  mtdi is {overhead/1.6:.0f}x FASTER than Frida on the hook path")
        print(f"{'='*60}")