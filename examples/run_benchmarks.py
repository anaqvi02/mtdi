import subprocess
import re
from collections import defaultdict

def run_cmd(cmd):
    result = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    return result.stdout

def parse_output(output):
    data = {}
    for line in output.split('\n'):
        if ':' in line and 'seconds' in line:
            parts = line.split(':')
            name = parts[0].strip().split(' ')[0] # e.g. "STAT" from "STAT      ( 5000)"
            time_str = parts[1].strip().split(' ')[0]
            try:
                data[name] = float(time_str)
            except ValueError:
                pass
    return data

def run_trials(cmd, num_trials=5):
    results = defaultdict(list)
    for i in range(num_trials):
        print(f"  Running trial {i+1}/{num_trials}...")
        out = run_cmd(cmd)
        parsed = parse_output(out)
        for k, v in parsed.items():
            results[k].append(v)
    return dict(results)

def calc_stats(results):
    stats = {}
    for k, v in results.items():
        avg = sum(v) / len(v)
        stats[k] = f"{avg:.3f}s"
    return stats

def main():
    print("Compiling C benchmark...")
    subprocess.run("gcc examples/heavy_bench.c -O3 -o heavy_bench", shell=True)
    subprocess.run("cargo build --release", shell=True)
    
    print("\n[1/3] Running Native benchmarks (5 trials)...")
    native = run_trials("./heavy_bench", 5)
    
    print("\n[2/3] Running Traced (Filtered) benchmarks (5 trials)...")
    filtered = run_trials("./target/release/mtrace -t asdf ./heavy_bench", 5)
    
    print("\n[3/3] Running Traced (Logged) benchmarks (5 trials)...")
    # Write to /dev/null to avoid blowing up the disk with 17 million lines of logs
    logged = run_trials("./target/release/mtrace -o /dev/null ./heavy_bench", 5)

    print("\n\n=== EXHAUSTIVE BENCHMARK RESULTS (0.5m iters, 5 trials avg) ===")
    print(f"{'Syscall Category':<20} | {'Native':<10} | {'Traced (Filtered)':<20} | {'Traced (Logged)':<20}")
    print("-" * 75)
    
    native_stats = calc_stats(native)
    filtered_stats = calc_stats(filtered)
    logged_stats = calc_stats(logged)
    
    keys = list(native_stats.keys())
    
    for k in keys:
        n = native_stats.get(k, "N/A")
        f = filtered_stats.get(k, "N/A")
        l = logged_stats.get(k, "N/A")
        print(f"{k:<20} | {n:<10} | {f:<20} | {l:<20}")

if __name__ == '__main__':
    main()
