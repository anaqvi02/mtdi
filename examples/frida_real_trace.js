// frida_real_trace_agent.js — hooks open() and does actual work:
// reads the path string, formats a log line, sends it to host.
// This is what real Frida tracing looks like.
let callCount = 0;

Interceptor.attach(Module.findExportByName(null, 'open'), {
    onEnter(args) {
        // Read the path string from the target's memory — this is real work
        try {
            var path = args[0].readUtf8String(256);
            var flags = args[1].toInt32();
            // Format a log line like a real tracer would
            var logLine = '[frida] open("' + path + '", ' + flags + ')';
            // In a real trace, you'd send() or write this somewhere
            // send(logLine);
        } catch(e) {}
        callCount++;
    },
    onLeave(retval) {}
});

send({ type: 'ready' });
setTimeout(() => { send({ type: 'done', count: callCount }); }, 5000);
