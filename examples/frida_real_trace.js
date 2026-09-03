// like a real trace: read path, format line, send
let callCount = 0;

Interceptor.attach(Module.findExportByName(null, 'open'), {
    onEnter(args) {
        // read path from the target's memory (real work)
        try {
            var path = args[0].readUtf8String(256);
            var flags = args[1].toInt32();
            // format a log line
            var logLine = '[frida] open("' + path + '", ' + flags + ')';
        } catch(e) {}
        callCount++;
    },
    onLeave(retval) {}
});

send({ type: 'ready' });
setTimeout(() => { send({ type: 'done', count: callCount }); }, 5000);
