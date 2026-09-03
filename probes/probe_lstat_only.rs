// probes/probe_lstat_only.rs
// Bisect step 8: hook ONLY lstat.

pub fn on_lstat(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 256) {
        let _ = path;
    }
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("lstat", on_lstat);
}
