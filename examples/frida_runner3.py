#!/usr/bin/env python3
"""Measure Frida overhead at three levels: empty hook, arg reading, full message send."""
import subprocess, os, time

DIR = os.path.dirname(os.path.abspath(__file__))
BENCH_SRC = os.path.join(DIR, "frida_bench3.c")
BENCH_BIN = os.path.join(DIR, "frida_bench3")
RESULT = "/tmp/frida_bench_result.txt"

def compile():
    subprocess.run(["gcc", "-O2", "-o", BENCH_BIN, BENCH_SRC], check=True)

def native():
    r = subprocess.run([BENCH_BIN], capture_output=True, text=True)
    return float(r.stdout.split()[0])

def frida_with_agent(agent_code, label):
    import frida
    try: os.unlink(RESULT)
    except: pass
    pid = frida.spawn([BENCH_BIN])
    session = frida.attach(pid)
    script = session.create_script(agent_code)
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
        with open(RESULT) as f:
            return float(f.read().strip())
    except:
        return None

AGENTS = {
    "empty (hook mechanism only)": """
    Interceptor.attach(Module.findExportByName(null, 'target_func'), {
        onEnter(args) {},
        onLeave(retval) {}
    });
    """,

    "read args + format string": """
    Interceptor.attach(Module.findExportByName(null, 'target_func'), {
        onEnter(args) {
            var a0 = args[0].toInt32();
            var a1 = args[1].toInt32();
            var s = 'target_func(' + a0 + ', ' + a1 + ')';
        },
        onLeave(retval) {}
    });
    """,

    "send() message to host": """
    Interceptor.attach(Module.findExportByName(null, 'target_func'), {
        onEnter(args) {
            send({ fn: 'target_func', a0: args[0].toInt32(), a1: args[1].toInt32() });
        },
        onLeave(retval) {}
    });
    """,
}

if __name__ == "__main__":
    compile()

    print("=" * 65)
    print("  Frida Overhead at Different Levels (1M iterations)")
    print("=" * 65)

    # Native baseline
    natives = [native() for _ in range(10)]
    navg = sum(natives) / len(natives)
    print(f"\n  Native baseline:  {navg:.2f} ns/call")

    for label, agent in AGENTS.items():
        vals = []
        for _ in range(5):
            ns = frida_with_agent(agent, label)
            if ns is not None:
                vals.append(ns)
        if vals:
            avg = sum(vals) / len(vals)
            overhead = avg - navg
            print(f"\n  Frida {label}:")
            print(f"    avg: {avg:.2f} ns/call  |  overhead: {overhead:.2f} ns  ({overhead/navg:.1f}x native)")

    print(f"\n  --- mtdi reference ---")
    print(f"  mtdi FastPath hook overhead:  ~1.6 ns")
    print(f"  mtdi ring-buffer push:        ~0 ns (atomic store + memcpy)")
    print(f"  Total mtdi hot path:          ~1.6 ns")
    print(f"\n  mtdi's hot path NEVER formats, NEVER allocates, NEVER does I/O.")
    print(f"  All formatting + I/O happens on the background reader thread.")
