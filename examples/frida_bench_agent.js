// frida_bench_agent.js — hooks target_func, counts calls, reports overhead.
// Used by frida_runner.py

let callCount = 0;

Interceptor.attach(Module.findExportByName(null, 'target_func'), {
    onEnter(args) {
        callCount++;
    },
    onLeave(retval) {
    }
});

// Send a ready signal
send({ type: 'ready' });

// After 3 seconds, tell the Python side to stop
setTimeout(() => {
    send({ type: 'done', callCount: callCount });
}, 3000);
