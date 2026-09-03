// probes/probe_send_only.rs
// Bisect step 13: hook ONLY send (never fires during startup; tests its INSTALL).

pub fn on_send(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("send", on_send);
}
