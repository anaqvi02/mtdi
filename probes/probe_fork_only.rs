// bisect 15: hook only fork; if its stub is a short wrapper,
// the 16-byte patch clobbers the next function

pub fn on_fork(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("fork", on_fork);
}
