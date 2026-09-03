// bisect 14: hook only recv. prediction: crash at 0x18509e680
// (16-byte patch overruns the 12-byte wrapper, clobbering the next fn)

pub fn on_recv(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("recv", on_recv);
}
