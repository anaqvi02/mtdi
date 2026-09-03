// bisect 2: dylib with zero hooks
// if the target still crashes, it's the constructor, not a hook

pub fn register(_reg: &mut MtdiRegistry) {
}
