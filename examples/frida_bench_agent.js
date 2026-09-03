// used by frida_runner.py

let callCount = 0;

Interceptor.attach(Module.findExportByName(null, 'target_func'), {
    onEnter(args) {
        callCount++;
    },
    onLeave(retval) {
    }
});

send({ type: 'ready' });

// after 3s, signal done to the runner
setTimeout(() => {
    send({ type: 'done', callCount: callCount });
}, 3000);
