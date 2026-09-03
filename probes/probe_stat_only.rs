// bisect 6: hook only stat; if the target survives, it's innocent

pub fn on_stat(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 256) {
        let _ = path;
    }
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("stat", on_stat);
}
