// probes/probe_fork_only.rs
// Bisect step 15: hook ONLY fork. Prediction: if fork's stub is also a short
// wrapper, its 16-byte patch clobbers the next function.

pub fn on_fork(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("fork", on_fork);
}
