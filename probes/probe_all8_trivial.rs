// bisect 9: all 8 writable hooks, trivial handlers
// crash => hook machinery at >=n hooks, not handler weight

pub fn on_open(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_stat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_lstat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_fstat(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_send(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_recv(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_fork(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }
pub fn on_exit(ctx: &mut MtdiSafeContext) { let _ = ctx.arg(0); }

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("open", on_open);
    reg.hook_symbol("stat", on_stat);
    reg.hook_symbol("lstat", on_lstat);
    reg.hook_symbol("fstat", on_fstat);
    reg.hook_symbol("send", on_send);
    reg.hook_symbol("recv", on_recv);
    reg.hook_symbol("fork", on_fork);
    reg.hook_symbol("exit", on_exit);
}
