// bisect 13: hook only send (never fires at startup; tests install)

pub fn on_send(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("send", on_send);
}
