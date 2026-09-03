// probes/probe_noop.rs
// Bisect step 2: probe dylib that registers ZERO hooks.
// If Firefox still crashes with this injected, the crash is in the probe
// dylib's constructor/harness, not in any hook.

pub fn register(_reg: &mut MtdiRegistry) {
    // intentionally empty
}
