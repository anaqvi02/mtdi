// probes/probe_open.rs
// A 100% Safe Rust probe executed inside the mtdis sandbox

pub fn on_open(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 256) {
        println!("[mtdis probe] Intercepted open() -> path: \"{}\", flags: {:#x}", path, ctx.arg(1));
    }
}

pub fn register(reg: &mut MtdiRegistry) {
    println!("[mtdis probe] Registering safe hooks from probe_open.rs...");
    reg.hook_symbol("open", on_open);
}
