// bisect 5: hook only open; if the target survives, it's innocent

pub fn on_open(ctx: &mut MtdiSafeContext) {
    if let Some(path) = ctx.read_arg_str(0, 256) {
        let _ = path;
    }
}

pub fn register(reg: &mut MtdiRegistry) {
    reg.hook_symbol("open", on_open);
}
