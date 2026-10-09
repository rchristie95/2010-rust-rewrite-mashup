use bevy::prelude::*;

#[derive(Default, Clone, Copy)]
pub struct SkateKeyboardInput {
    pub buttons: u16,
    pub triggers: [u8; 2],
    pub left: [i16; 2],
    pub right: [i16; 2],
    pub engaged: bool,
}

/// Local skating presentation. Physics remains owned by the optional Skate host.
#[derive(Resource, Default)]
pub struct SkateMode {
    pub keyboard: SkateKeyboardInput,
    pub active: bool,
    pub entering: bool,
    pub preloaded: bool,
    pub preload_pending: bool,
    pub controller: Option<usize>,
    pub toggle_requested: bool,
    pub input_blocked: bool,
    pub client: u32,
    pub root: Mat4,
    pub bones: Vec<Mat4>,
    pub names: Vec<String>,
    pub camera: Option<(Transform, f32)>,
    pub velocity: Vec3,
    pub tick: u64,
    pub status: String,
}
