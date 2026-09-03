// probes/probe_fstat_only.rs
// Bisect step 7: hook ONLY fstat (fd in arg0 — never read_arg_str(0) on fstat).

pub fn on_fstat(ctx: &mut MtdiSafeContext) {
    let fd = ctx.arg(0);
    let _ = fd;
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("fstat", on_fstat);
}
