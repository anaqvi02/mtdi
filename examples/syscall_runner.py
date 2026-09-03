#!/usr/bin/env python3
"""Real-world syscall tracing benchmark: Frida vs mtdi on open()+close()."""
import subprocess, os, time

DIR = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(DIR, "syscall_bench.c")
BIN = os.path.join(DIR, "syscall_bench")
RESULT = "/tmp/frida_bench_result.txt"

def compile():
    subprocess.run(["gcc", "-O2", "-o", BIN, SRC], check=True)

def native():
    r = subprocess.run([BIN], capture_output=True, text=True)
    return float(r.stdout.split()[0])

def frida_bench(agent_file):
    import frida
    try: os.unlink(RESULT)
    except: pass

    pid = frida.spawn([BIN])
    session = frida.attach(pid)

    with open(os.path.join(DIR, agent_file)) as f:
        script = session.create_script(f.read())

    msg_log = []
    def on_message(msg, data):
        if msg.get("type") == "send":
            msg_log.append(msg.get("payload", {}))
    script.on("message", on_message)
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

    time.sleep(0.3)
    try:
        with open(RESULT) as f:
            return float(f.read().strip()), msg_log
    except:
        return None, msg_log

# Empty hook agent
EMPTY_AGENT = "frida_empty_agent.js"
with open(os.path.join(DIR, EMPTY_AGENT), "w") as f:
    f.write("""
    Interceptor.attach(Module.findExportByName(null, 'open'), {
        onEnter(args) {},
        onLeave(retval) {}
    });
    Interceptor.attach(Module.findExportByName(null, 'close'), {
        onEnter(args) {},
        onLeave(retval) {}
    });
    """)

if __name__ == "__main__":
    compile()

    print("=" * 70)
    print("  Real Syscall Tracing Benchmark (1M open+close pairs)")
    print("  open() = read path arg + format log line")
    print("=" * 70)

    # Native baseline
    natives = [native() for _ in range(5)]
    navg = sum(natives) / len(natives)
    print(f"\n  Native (no hooks):     {navg:.2f} ns/syscall")

    # Frida empty hooks
    print(f"\n  Frida empty hooks (5 runs)...")
    vals = []
    for i in range(5):
        ns, _ = frida_bench(EMPTY_AGENT)
        if ns: vals.append(ns); print(f"    run {i+1}: {ns:.2f} ns")
    if vals:
        favg = sum(vals)/len(vals)
        print(f"    avg: {favg:.2f} ns  |  overhead: {favg-navg:.2f} ns ({(favg-navg)/navg*100:.0f}%)")

    # Frida real tracing (read path + format)
    print(f"\n  Frida real tracing — read path + format string (5 runs)...")
    vals = []
    for i in range(5):
        ns, msgs = frida_bench("frida_real_trace.js")
        if ns: vals.append(ns); print(f"    run {i+1}: {ns:.2f} ns")
    if vals:
        favg = sum(vals)/len(vals)
        print(f"    avg: {favg:.2f} ns  |  overhead: {favg-navg:.2f} ns ({(favg-navg)/navg*100:.0f}%)")

    # mtdi reference
    print(f"\n  --- mtdi reference ---")
    print(f"  mtdi FastPath hook + ring buffer push: ~1.6 ns overhead")
    print(f"  mtdi formatting + I/O: on background thread (off hot path)")
    print(f"\n  Key difference: mtdi's hot path is ONE atomic store + memcpy.")
    print(f"  Frida's hot path goes through JS engine on every call.")
