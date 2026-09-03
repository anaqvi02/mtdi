// probes/probe_stat_only.rs
// Bisect step 6: hook ONLY stat. If Firefox survives, stat's hook is innocent.

pub fn on_stat(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 256) {
        let _ = path;
    }
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("stat", on_stat);
}
