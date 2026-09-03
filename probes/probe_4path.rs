// bisect 11: open+stat+lstat+fstat (4 firing path hooks)
// crash => nth-install issue; survives => need send/recv/fork/exit

pub fn on_open(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_stat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_lstat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_fstat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("open", on_open);
    reg.hook_symbol("stat", on_stat);
    reg.hook_symbol("lstat", on_lstat);
    reg.hook_symbol("fstat", on_fstat);
}
