#!/usr/bin/env python3
"""Direct Frida overhead measurement — spawns binary under Frida, reads timing from file."""
import subprocess, os, time

DIR = os.path.dirname(os.path.abspath(__file__))
BENCH_SRC = os.path.join(DIR, "frida_bench2.c")
BENCH_BIN = os.path.join(DIR, "frida_bench2")
RESULT_FILE = "/tmp/frida_bench_result.txt"

def compile():
    subprocess.run(["gcc", "-O2", "-o", BENCH_BIN, BENCH_SRC], check=True)

def native_baseline():
    r = subprocess.run([BENCH_BIN], capture_output=True, text=True)
    return float(r.stdout.split()[0])

def frida_hooked():
    import frida
    try: os.unlink(RESULT_FILE)
    except: pass

    pid = frida.spawn([BENCH_BIN])
    session = frida.attach(pid)

    # empty callbacks like a typical trace
    script = session.create_script("""
    Interceptor.attach(Module.findExportByName(null, 'target_func'), {
        onEnter(args) {},
        onLeave(retval) {}
    });
    """)
    script.load()
    frida.resume(pid)

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
    except:
        return None

if __name__ == "__main__":
    compile()

    print("=" * 60)
    print("  Frida vs mtdi: Per-Call Hook Overhead")
    print("  1M iterations, same target function")
    print("=" * 60)

    natives = []
    for _ in range(10):
        natives.append(native_baseline())
    native_avg = sum(natives) / len(natives)
    print(f"\n  Native baseline:     {native_avg:.2f} ns/call (avg of 10)")

    print(f"\n  Running under Frida (5 runs)...")
    frida_vals = []
    for i in range(5):
        ns = frida_hooked()
        if ns is not None:
            frida_vals.append(ns)
            print(f"    run {i+1}: {ns:.2f} ns/call")
        else:
            print(f"    run {i+1}: FAILED")

    if frida_vals:
        frida_avg = sum(frida_vals) / len(frida_vals)
        frida_overhead = frida_avg - native_avg

        print(f"\n  Frida avg:           {frida_avg:.2f} ns/call")
        print(f"  Frida overhead:      {frida_overhead:.2f} ns/call ({frida_overhead/native_avg*100:.0f}% of native)")

        print(f"\n  --- mtdi reference numbers ---")
        print(f"  mtdi FastPath:       ~1.6 ns overhead")
        print(f"  mtdi FullContext:    ~15 ns overhead")

        print(f"\n{'='*60}")
        if frida_overhead > 0:
            ratio = frida_overhead / 1.6
            print(f"  Frida hook overhead:  {frida_overhead:.1f} ns")
            print(f"  mtdi FastPath:        1.6 ns")
            print(f"  Frida is {ratio:.0f}x SLOWER than mtdi FastPath")
        print(f"{'='*60}")
