// probes/probe_open_fstat.rs
// Bisect step 10: hook open + fstat (the hottest pair at the crash site), trivial.

pub fn on_open(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_fstat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("open", on_open);
    reg.hook_symbol("fstat", on_fstat);
}
