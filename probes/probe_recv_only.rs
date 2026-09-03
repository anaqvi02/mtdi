// probes/probe_recv_only.rs
// Bisect step 14: hook ONLY recv. Prediction: crash at 0x18509e680
// (16-byte patch overruns the 12-byte recv wrapper, clobbering the next fn).

pub fn on_recv(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("recv", on_recv);
}
