// probes/probe_3path.rs
// Bisect step 12: hook open+stat+lstat (3 FIRING path hooks only).
// Tests the 3-hook threshold.

pub fn on_open(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_stat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_lstat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("open", on_open);
    reg.hook_symbol("stat", on_stat);
    reg.hook_symbol("lstat", on_lstat);
}
