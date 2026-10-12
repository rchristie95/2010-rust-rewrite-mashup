//! The Minecraft map at run time: MinecraftOSS generates and streams a seeded
//! world around the local player, whose chunks become block collision and
//! whose section meshes go to the renderer. The world sits with its player
//! spawn at map origin, a block to `sim::voxel::BLOCK` map units.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, mpsc};

use bevy::input::gamepad::GamepadButton;
use bevy::prelude::*;
use minecraft_terrain::clouds::CloudMask;
use minecraft_terrain::day_cycle::{DayCycle, Skybox};
use minecraft_terrain::environment::{DimensionEnvironment, View};
use minecraft_terrain::lighting::SkyLight;
use minecraft_terrain::mesh::{Atlas, SectionMesh, Vertex};
use minecraft_terrain::pack::PackStack;
use minecraft_terrain::scene::HandcraftedScene;
use minecraft_terrain::sections::{CullCamera, SectionPos};
use minecraft_terrain::terrain::{Dimension, TerrainStream};
use minecraftoss_core::BlockStateId;
use minecraftoss_core::registries::{DataPaths, Registries};

const VIEW_DISTANCE: i32 = 8;
const TICK_SECONDS: f64 = 1.0 / 20.0;
/// Blocks on a side of the light volume MW2 models are lit from.
pub const LIGHT_VOLUME: i32 = 64;
/// Chunk sections fade in over this long, as the viewer's default option.
const FADE_MILLIS: u64 = 750;

/// What the renderer takes from the world each frame.
#[derive(Resource, Default)]
pub struct MinecraftWorldView {
    /// A Minecraft map is loaded: the stand-in map's world is not drawn.
    pub active: bool,
    /// Block point at map origin.
    pub origin: [f64; 3],
    pub atlas: Option<Arc<Atlas>>,
    pub uploads: Vec<(SectionPos, SectionMesh)>,
    pub removed: Vec<SectionPos>,
    pub visible: Vec<(SectionPos, f32)>,
    /// Bumped when the world is replaced, so stale sections are dropped.
    pub generation: u64,
    /// The environment uniform of MinecraftOSS for this frame, in block space.
    pub environment: [[f32; 4]; 16],
    /// Sun and the eight moon phases, 32 pixels each, side by side.
    pub celestial: Option<Arc<image::RgbaImage>>,
    pub clouds: Option<Arc<(Vec<Vertex>, Vec<u32>)>>,
    /// Sky and block light around the player: origin block, then
    /// `LIGHT_VOLUME` cubed pairs, x fastest then z then y.
    pub light_volume: Option<Arc<([i32; 3], Vec<u8>)>>,
    /// Sky and block light at the eye, for the view model.
    pub eye_light: [f32; 2],
    /// Break particles as section vertices and indices, rebuilt each frame.
    pub particles: (Vec<u8>, Vec<u32>),
    /// Destroy stage cubes over blocks being mined: position, strip uv.
    pub cracks: (Vec<[f32; 5]>, Vec<u32>),
    /// The ten destroy stages side by side.
    pub crack_texture: Option<Arc<image::RgbaImage>>,
    /// Mob models (cut out, back-face culled, translucent) and entity
    /// shadows, as `mesh::Vertex` bytes and indices.
    pub entity_meshes: [(Vec<u8>, Vec<u32>); 4],
    /// The black card behind the inventory's character, in blocks.
    pub backdrop: Option<[[f32; 3]; 4]>,
    /// The first-person hand or held item: section vertex bytes in view
    /// space (x right, y up, z back) and indices, with the projection that
    /// draws them (vanilla's fixed 70 degree hand field of view).
    pub hand: (Vec<u8>, Vec<u32>),
    pub hand_clip: [f32; 16],
    pub status: Option<MinecraftWorldStatus>,
}

#[derive(Clone, Copy, Debug)]
pub struct MinecraftWorldStatus {
    pub seed: i64,
    pub origin: [f64; 3],
    pub alive: bool,
    pub health: i32,
    pub stream_ready: bool,
    pub stream_center: (i32, i32, i32),
    pub edit_revision: u64,
    pub edit_count: usize,
    pub edit_fingerprint: u64,
    pub mob_sequence: u32,
    pub mob_count: usize,
}

fn edit_fingerprint(pos: (i32, i32, i32), state: u32) -> u64 {
    [pos.0 as u32, pos.1 as u32, pos.2 as u32, state]
        .into_iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, value| {
            (hash ^ u64::from(value)).wrapping_mul(0x100_0000_01b3)
        })
}

struct Loaded {
    stream: TerrainStream,
    scene: HandcraftedScene,
    packs: PackStack,
    atlas: Arc<Atlas>,
    registries: Arc<Registries>,
    seed: i64,
    environment: DimensionEnvironment,
    celestial: Arc<image::RgbaImage>,
    cloud_mask: Option<CloudMask>,
    crack_texture: Arc<image::RgbaImage>,
}

/// The hand's swing and the timers of mining and placing by hand.
#[derive(Default)]
struct HandState {
    /// Ticks into a swing, while one runs.
    swing: Option<f32>,
    /// Seconds towards the next hand-mining and placing tick.
    clock: f64,
    /// Ticks until another placement while the button is held
    /// (`rightClickDelay`).
    place_delay: u32,
}

/// The player's walk, for vanilla's step and fall sounds.
#[derive(Default)]
struct StepState {
    last: Option<[f64; 3]>,
    /// `Entity.moveDist` and `nextStep`.
    move_dist: f32,
    next_step: f32,
    /// The highest point since leaving the ground.
    air_peak: Option<f64>,
}

#[derive(Default)]
struct Runtime {
    loading: Option<mpsc::Receiver<Result<Loaded, String>>>,
    world: Option<Loaded>,
    network_origin: Option<[f64; 3]>,
    network_seed: Option<i64>,
    network_edits_expected: Option<u32>,
    network_edits: BTreeMap<(i32, i32, i32), u32>,
    changed_blocks: HashSet<(i32, i32, i32)>,
    edit_revision: u64,
    edit_fingerprint: u64,
    network_log: Vec<(u64, Vec<([i32; 3], u32)>)>,
    network_initial_revision: u64,
    network_initial_positions: HashSet<(i32, i32, i32)>,
    remote_stock: HashMap<sim::ClientId, HashMap<u16, u16>>,
    stock_dirty: HashSet<sim::ClientId>,
    pending_stock: Option<Vec<(u16, u16)>>,
    remote_mining_owner: HashMap<(i32, i32, i32), sim::ClientId>,
    live_progress: HashMap<sim::ClientId, (usize, usize)>,
    sent_init: HashSet<sim::ClientId>,
    init_progress: HashMap<sim::ClientId, PeerInit>,
    mob_sequence: u32,
    mob_count: usize,
    mob_parts: Option<(u32, u8, Vec<Option<Vec<net::MinecraftMobVisual>>>)>,
    last_mobs_sent: f64,
    day: DayCycle,
    environment_accumulator: f64,
    environment_primed: bool,
    light: Option<SkyLight>,
    light_volume_at: Option<[i32; 3]>,
    light_volume_age: u32,
    cloud_center: Option<(i32, i32)>,
    mining: crate::minecraft_mining::Mining,
    minimap: crate::minecraft_minimap::Minimap,
    hand: HandState,
    steps: StepState,
    sounds: Option<crate::minecraft_sounds::Sounds>,
    inventory_ui: crate::minecraft_inventory::InventoryUi,
    entities: Option<crate::minecraft_entities::Entities>,
    /// Shape id of each block state already seen.
    shapes: HashMap<BlockStateId, u16>,
    /// Boxes of each shape id, to reuse an id for a repeated shape.
    shape_ids: HashMap<Vec<[u32; 6]>, u16>,
    was_alive: bool,
    retained_collision: HashSet<(i32, i32)>,
}

/// The player's MW2 body stands in the inventory's character window, on a
/// black card: placed from this frame's camera and projection where the
/// window shows, facing the camera, turned and aiming towards the mouse,
/// and drawn with the card in the view model's depth band so no wall comes
/// between.
pub(crate) fn place_inventory_puppet(
    ui: Res<frame::MinecraftUi>,
    mut puppet: ResMut<frame::InventoryPuppet>,
    mut view: ResMut<MinecraftWorldView>,
    local: Res<net::LocalPresentClient>,
    presented: Res<net::PresentedSnapshot>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cameras: Query<&Transform, With<render_scene::FlyCamera>>,
    lenses: Query<&Projection, With<render_scene::FpvLens>>,
) {
    use std::sync::atomic::Ordering::Relaxed;
    puppet.active = false;
    view.backdrop = None;
    render_frame::DEPTH_HACK_SCENE_ENTNUM.store(u32::MAX, Relaxed);
    let alive = presented.player(local.0).is_some_and(|ps| ps.pm_type == 0);
    if !(ui.active && ui.inventory_open && alive) {
        return;
    }
    let (Some([cx, cy, box_w, box_h]), Ok(window), Ok(camera), Ok(Projection::Perspective(lens))) = (
        ui.character_box,
        windows.single(),
        cameras.single(),
        lenses.single(),
    ) else {
        return;
    };
    let (w, h) = (window.width().max(1.0), window.height().max(1.0));
    let tan_v = (lens.fov * 0.5).tan();
    let tan_h = tan_v
        * if lens.aspect_ratio > 1e-3 {
            lens.aspect_ratio
        } else {
            w / h
        };
    let (eye, fwd, right, up) = (
        camera.translation,
        *camera.forward(),
        *camera.right(),
        *camera.up(),
    );
    // A point `distance` along the view at a window pixel.
    let at = |px: f32, py: f32, distance: f32| {
        let (nx, ny) = (px / w * 2.0 - 1.0, 1.0 - py / h * 2.0);
        eye + (fwd + right * (nx * tan_h) + up * (ny * tan_v)) * distance
    };
    let distance = 10.0;
    let world_h = box_h / h * 2.0 * tan_v * distance;
    // A standing MW2 player is about 72 units: most of the window.
    let scale = world_h * 0.82 / 72.0;
    let feet = at(cx, cy + box_h * 0.5, distance) + up * (world_h * 0.07);
    // Turned by the mouse as vanilla's
    // `InventoryScreen.renderEntityInInventoryFollowsMouse` turns its body.
    let turn = (ui.gaze[0] * 1.2).atan() * 0.7;
    let x_axis = (-fwd * turn.cos() + right * turn.sin()).normalize_or(-fwd);
    let y_axis = up.cross(x_axis);
    puppet.root = Mat4::from_cols(
        (x_axis * scale).extend(0.0),
        (y_axis * scale).extend(0.0),
        (up * scale).extend(0.0),
        feet.extend(1.0),
    );
    puppet.pitch = (ui.gaze[1] * 1.2).atan().to_degrees() * 0.6;
    puppet.client = local.0.0;
    puppet.active = true;
    render_frame::DEPTH_HACK_SCENE_ENTNUM.store(local.0.0, Relaxed);
    // The card: the window and a margin the panel covers, behind it.
    let corner = |dx: f32, dy: f32| {
        let p = at(cx + dx * box_w * 0.6, cy + dy * box_h * 0.6, distance * 1.4);
        let b = sim::voxel::to_block(view.origin, p.to_array());
        [b[0] as f32, b[1] as f32, b[2] as f32]
    };
    view.backdrop = Some([
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ]);
}

pub(crate) fn register(app: &mut App) {
    app.add_systems(
        Update,
        place_inventory_puppet
            .after(crate::sync_camera_from_presented)
            .after(frame::PresentedPublished)
            .in_set(frame::ClientSet::Present),
    );
    app.init_resource::<MinecraftWorldView>()
        .init_resource::<frame::MinecraftUi>()
        .init_resource::<frame::InventoryPuppet>()
        .insert_non_send(Runtime::default())
        .add_systems(
            Update,
            update
                .after(crate::sync_camera_from_presented)
                .after(frame::PresentedPublished)
                .in_set(frame::ClientSet::Present),
        );
}

fn load(seed: i64) -> Result<Loaded, String> {
    let root = assets::minecraft_map::root().ok_or_else(assets::minecraft_setup::status)?;
    let paths = DataPaths::under(&root);
    let registries = Arc::new(Registries::load(&paths)?);
    let packs = PackStack::open(vec![root.join("resourcepacks/local/minecraft-26.3")])
        .map_err(|e| e.to_string())?;
    let stream = TerrainStream::for_dimension(
        registries.clone(),
        seed,
        VIEW_DISTANCE,
        Dimension::Overworld,
        None,
    )
    .map_err(|e| e.to_string())?;
    let build = minecraft_terrain::mesh::build(&HandcraftedScene::default(), &packs)
        .map_err(|e| e.to_string())?;
    let scene = HandcraftedScene::streamed(stream.states.clone());
    let environment =
        DimensionEnvironment::load(&registries, Dimension::Overworld.dimension_type())?;
    let celestial = Arc::new(celestial_image(&packs).map_err(|e| e.to_string())?);
    let cloud_mask = CloudMask::from_pack(&packs).ok();
    let crack_texture =
        Arc::new(crate::minecraft_mining::crack_strip(&packs).map_err(|e| e.to_string())?);
    Ok(Loaded {
        stream,
        scene,
        packs,
        atlas: build.atlas,
        registries,
        seed,
        environment,
        celestial,
        cloud_mask,
        crack_texture,
    })
}

/// The sky's sun and moon phases, laid out as MinecraftOSS lays them out.
fn celestial_image(packs: &PackStack) -> anyhow::Result<image::RgbaImage> {
    let mut celestial = image::RgbaImage::new(32 * 9, 32);
    let names = [
        "environment/celestial/sun",
        "environment/celestial/moon/full_moon",
        "environment/celestial/moon/waning_gibbous",
        "environment/celestial/moon/third_quarter",
        "environment/celestial/moon/waning_crescent",
        "environment/celestial/moon/new_moon",
        "environment/celestial/moon/waxing_crescent",
        "environment/celestial/moon/first_quarter",
        "environment/celestial/moon/waxing_gibbous",
    ];
    for (index, path) in names.iter().enumerate() {
        let id = minecraft_terrain::pack::ResourceId::parse(&format!("minecraft:{path}"))?;
        if let Some(bytes) = packs.texture(&id)? {
            let img =
                image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)?.to_rgba8();
            let tile = image::imageops::resize(&img, 32, 32, image::imageops::FilterType::Nearest);
            image::imageops::replace(&mut celestial, &tile, (index as i64) * 32, 0);
        }
    }
    Ok(celestial)
}

/// A controller trigger, held or just pressed: with a block or an empty hand
/// the right trigger mines and the left places, as the mouse buttons do.
fn pad_trigger(
    pad: Option<&bevy::input::gamepad::Gamepad>,
    button: GamepadButton,
    just: bool,
) -> bool {
    pad.is_some_and(|pad| {
        if just {
            pad.just_pressed(button)
        } else {
            pad.pressed(button)
        }
    })
}

fn seed() -> i64 {
    if let Some(seed) = std::env::var("IW4L_MINECRAFT_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return seed;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (nanos as i64) ^ 0x5DEE_CE66_D1CE_4E5B
}

struct PeerInit {
    edits: Vec<([i32; 3], u32)>,
    next: usize,
    revision: u64,
    began: bool,
    log_index: usize,
}

fn block_shape(
    blocks: &minecraftoss_core::block::BlockRegistry,
    state: BlockStateId,
    shapes: &mut HashMap<BlockStateId, u16>,
    shape_ids: &mut HashMap<Vec<[u32; 6]>, u16>,
) -> u16 {
    if state == BlockStateId::AIR {
        return 0;
    }
    *shapes.entry(state).or_insert_with(|| {
        let boxes = blocks.collision_boxes(state);
        if let Some(fluid) = blocks.state(state).fluid {
            let boxes = boxes.iter().map(|b| b.map(|v| v as f32)).collect();
            return match fluid.kind {
                minecraftoss_core::block::FluidKind::Water => {
                    sim::voxel::add_water_shape(boxes, fluid.amount)
                }
                minecraftoss_core::block::FluidKind::Lava => {
                    sim::voxel::add_lava_shape(boxes, fluid.amount)
                }
            }
            .unwrap_or(0);
        }
        if boxes.is_empty() {
            return 0;
        }
        let key: Vec<[u32; 6]> = boxes
            .iter()
            .map(|b| b.map(|v| (v as f32).to_bits()))
            .collect();
        if let Some(&id) = shape_ids.get(&key) {
            return id;
        }
        let boxes32 = boxes.iter().map(|b| b.map(|v| v as f32)).collect();
        let id = sim::voxel::add_shapes(vec![boxes32]).unwrap_or(0);
        shape_ids.insert(key, id);
        id
    })
}

fn start_load(seed: i64) -> mpsc::Receiver<Result<Loaded, String>> {
    let (send, receive) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("minecraft-world-load".into())
        .spawn(move || {
            let _ = send.send(load(seed));
        });
    receive
}

#[allow(clippy::too_many_arguments)]
fn update(
    time: Res<Time>,
    mut installed: MessageReader<frame::MatchInstalled>,
    mut torn_down: MessageReader<frame::MatchTornDown>,
    local: Res<net::LocalPresentClient>,
    presented: Res<net::PresentedSnapshot>,
    authority: Option<ResMut<net::AuthorityWorld>>,
    prediction: Option<Res<net::ClientPredictionState>>,
    role: Res<frame::RuntimeRole>,
    (mut minecraft_sync, mut action_inbox, mut request_ids): (
        ResMut<net::MinecraftSync>,
        ResMut<net::ClientActionInbox>,
        ResMut<net::ActionRequestIds>,
    ),
    hub: Option<Res<net::UdpAuthorityHub>>,
    mut reliable: ResMut<net::ReliableEventHub>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    (mut ui, mut puppet, mut images, mut sound_queue, buttons): (
        ResMut<frame::MinecraftUi>,
        ResMut<frame::InventoryPuppet>,
        ResMut<Assets<Image>>,
        ResMut<audio::McSoundQueue>,
        Res<ButtonInput<MouseButton>>,
    ),
    mut view: ResMut<MinecraftWorldView>,
    mut runtime: NonSendMut<Runtime>,
    (skate, cameras, lenses, gamepads, active_pad): (
        Res<frame::SkateMode>,
        Query<&Transform, With<render_scene::FlyCamera>>,
        Query<&Projection, With<render_scene::FpvLens>>,
        Query<&bevy::input::gamepad::Gamepad>,
        Option<Res<frame::ActivePad>>,
    ),
) {
    let pad = active_pad
        .and_then(|active| active.0)
        .and_then(|entity| gamepads.get(entity).ok());
    for _ in torn_down.read() {
        stop(&mut runtime, &mut view);
        ui.minimap = None;
        minecraft_sync.accept_intents = false;
        minecraft_sync.incoming_intents.clear();
    }
    ui.loading_world = runtime.loading.is_some()
        || (*role == frame::RuntimeRole::Client && view.active && runtime.world.is_none());
    for match_ in installed.read() {
        stop(&mut runtime, &mut view);
        ui.workbench_open = false;
        ui.inventory_open = false;
        ui.clicks.clear();
        ui.minimap = None;
        minecraft_sync.accept_intents = false;
        minecraft_sync.incoming_intents.clear();
        if assets::minecraft_map::is_minecraft(&match_.zone) {
            if *role != frame::RuntimeRole::Client {
                let seed = seed();
                runtime.network_seed = Some(seed);
                diag::info!(World, "Minecraft world: seed {seed}");
                runtime.loading = Some(start_load(seed));
            }
            ui.loading_world = true;
            view.active = true;
        }
    }
    for message in std::mem::take(&mut minecraft_sync.incoming) {
        match message {
            net::MinecraftMessage::Begin {
                seed,
                origin,
                revision,
                edits,
            } if *role == frame::RuntimeRole::Client && view.active => {
                runtime.network_seed = Some(seed);
                runtime.network_origin = Some(origin);
                runtime.network_edits_expected = Some(edits);
                runtime.network_initial_revision = revision;
                runtime.network_initial_positions.clear();
                runtime.network_edits.clear();
                runtime.edit_revision = revision;
                runtime.edit_fingerprint = 0;
            }
            net::MinecraftMessage::Blocks { revision, changes }
                if *role == frame::RuntimeRole::Client && view.active =>
            {
                for (pos, block) in changes {
                    let point = (pos[0], pos[1], pos[2]);
                    if revision == runtime.network_initial_revision {
                        runtime.network_initial_positions.insert(point);
                    }
                    if let Some(old) = runtime.network_edits.insert(point, block) {
                        runtime.edit_fingerprint ^= edit_fingerprint(point, old);
                    }
                    runtime.edit_fingerprint ^= edit_fingerprint(point, block);
                    runtime.changed_blocks.insert(point);
                }
                runtime.edit_revision = runtime.edit_revision.max(revision);
            }
            net::MinecraftMessage::Mobs {
                sequence,
                part,
                total,
                mobs,
            } if *role == frame::RuntimeRole::Client && view.active => {
                if sequence > runtime.mob_sequence
                    && runtime
                        .mob_parts
                        .as_ref()
                        .is_none_or(|(current, _, _)| sequence > *current)
                {
                    runtime.mob_parts = Some((sequence, total, vec![None; usize::from(total)]));
                }
                let mut complete = None;
                if let Some((current, count, parts)) = runtime.mob_parts.as_mut()
                    && *current == sequence
                    && *count == total
                {
                    parts[usize::from(part)] = Some(mobs);
                    if parts.iter().all(Option::is_some) {
                        complete = Some(
                            parts
                                .iter()
                                .flat_map(|part| part.as_ref().unwrap().iter().copied())
                                .collect::<Vec<_>>(),
                        );
                    }
                }
                if let Some(snapshot) = complete {
                    runtime.mob_count = snapshot.len();
                    if let Some(entities) = runtime.entities.as_mut() {
                        entities.apply_network_snapshot(&snapshot);
                    }
                    runtime.mob_parts = None;
                    runtime.mob_sequence = sequence;
                }
            }
            net::MinecraftMessage::Stock { entries }
                if *role == frame::RuntimeRole::Client && view.active =>
            {
                runtime.pending_stock = Some(entries);
            }
            _ => {}
        }
    }
    if *role == frame::RuntimeRole::Client
        && runtime.world.is_none()
        && runtime.loading.is_none()
        && runtime
            .network_edits_expected
            .is_some_and(|count| runtime.network_initial_positions.len() >= count as usize)
        && let Some(seed) = runtime.network_seed
    {
        runtime.loading = Some(start_load(seed));
    }
    ui.map_active = view.active;
    let mut authority = authority;

    if let Some(receive) = &runtime.loading
        && let Ok(result) = receive.try_recv()
    {
        runtime.loading = None;
        match result {
            Ok(world) => {
                let (x, y, z) = world.stream.player_spawn;
                view.origin = runtime.network_origin.unwrap_or([x, y, z]);
                view.atlas = Some(world.atlas.clone());
                view.celestial = Some(world.celestial.clone());
                view.crack_texture = Some(world.crack_texture.clone());
                runtime.mining = Default::default();
                runtime.sounds = Some(crate::minecraft_sounds::Sounds::load(&world.packs));
                runtime.entities = Some(if *role == frame::RuntimeRole::Client {
                    crate::minecraft_entities::Entities::new_remote(world.seed)
                } else {
                    crate::minecraft_entities::Entities::new(&world.stream, world.seed)
                });
                runtime.day = DayCycle::default();
                // Game ticks since sunrise to start at: 6000 noon, 13000
                // dusk, 18000 midnight.
                if let Some(ticks) = std::env::var("IW4L_MINECRAFT_TIME")
                    .ok()
                    .and_then(|t| t.trim().parse::<f64>().ok())
                {
                    runtime.day.set(ticks);
                }
                runtime.environment_accumulator = 0.0;
                runtime.environment_primed = false;
                runtime.light = Some(SkyLight::streamed());
                runtime.light_volume_at = None;
                runtime.cloud_center = None;
                view.generation += 1;
                if let Some(authority) = authority.as_ref() {
                    sim::voxel::activate(
                        authority.0.content().clip_brushes(),
                        view.origin,
                        vec![Vec::new()],
                    );
                } else if let Some(prediction) = prediction.as_ref() {
                    sim::voxel::activate(
                        prediction.0.world().content().clip_brushes(),
                        view.origin,
                        vec![Vec::new()],
                    );
                }
                diag::info!(
                    World,
                    "Minecraft world ready: seed {} spawn {:?}",
                    world.seed,
                    world.stream.player_spawn
                );
                runtime.world = Some(world);
                runtime.shapes.clear();
                runtime.shape_ids.clear();
                runtime.was_alive = false;
                runtime.retained_collision.clear();
            }
            Err(error) => {
                diag::warn!(World, "Minecraft world failed to load: {error}");
                view.active = false;
                ui.map_active = false;
            }
        }
    }

    let origin = view.origin;
    let Runtime {
        world,
        network_edits,
        changed_blocks,
        edit_revision,
        edit_fingerprint: edits_hash,
        network_log,
        remote_stock,
        stock_dirty,
        pending_stock,
        remote_mining_owner,
        live_progress,
        sent_init,
        init_progress,
        mob_sequence,
        mob_count,
        last_mobs_sent,
        shapes,
        shape_ids,
        was_alive,
        retained_collision,
        day,
        environment_accumulator,
        environment_primed,
        light,
        light_volume_at,
        light_volume_age,
        cloud_center,
        mining,
        entities,
        inventory_ui,
        sounds,
        hand,
        minimap,
        steps,
        ..
    } = &mut *runtime;
    let Some(world) = world.as_mut() else {
        minecraft_sync.incoming_intents.clear();
        ui.active = false;
        ui.inventory_open = false;
        puppet.active = false;
        return;
    };
    minecraft_sync.accept_intents = authority.is_some();
    if let Some(entities) = entities.as_mut() {
        if let Some(entries) = pending_stock.take() {
            for slot in &mut entities.inventory.slots {
                if slot
                    .as_ref()
                    .is_some_and(|item| crate::minecraft_inventory::weapon_of(item).is_none())
                {
                    *slot = None;
                }
            }
            for (state, count) in entries {
                if let Some(block) = world.stream.states.block(BlockStateId(state)) {
                    let mut left = count;
                    while left > 0 {
                        let chunk = left.min(64) as u8;
                        let item =
                            minecraftoss_player::inventory::ItemStack::new(block.id.key(), chunk);
                        let _ = entities.inventory.add_item(item, entities.selected);
                        left -= u16::from(chunk);
                    }
                }
            }
        }
    }
    if *role == frame::RuntimeRole::Client && !changed_blocks.is_empty() {
        let positions: Vec<_> = changed_blocks.drain().collect();
        for pos in &positions {
            let state = network_edits[pos];
            if state as usize >= world.registries.blocks.state_count() {
                diag::warn!(
                    World,
                    "Minecraft sync: rejected unknown block state {state}"
                );
                continue;
            }
            let state = BlockStateId(state as u16);
            world
                .scene
                .set(*pos, world.stream.states.block(state).cloned());
            let shape = block_shape(&world.registries.blocks, state, shapes, shape_ids);
            sim::voxel::set_block_shape(pos.0, pos.1, pos.2, shape);
        }
        world.stream.record_edits(&world.scene, &positions);
        world.stream.mark_edited(&world.scene, &positions);
    }
    let Some(ps) = presented.player(local.0) else {
        ui.active = false;
        puppet.active = false;
        return;
    };

    let remote_view = presented
        .snapshot()
        .and_then(|snapshot| snapshot.meta.for_client(local.0))
        .is_some_and(|meta| {
            meta.linked_weapon_view.is_some()
                || meta
                    .remote_missile
                    .is_some_and(|link| link.unlink_at_ms.is_none())
        });

    let alive = ps.pm_type == 0;
    if !alive {
        *was_alive = false;
    }

    let feet = sim::voxel::to_block(origin, ps.origin);
    let block = (
        feet[0].floor() as i32,
        feet[1].floor() as i32,
        feet[2].floor() as i32,
    );
    let player_chunk = (block.0 >> 4, block.2 >> 4);
    retained_collision.retain(|&(x, z)| {
        if (x - player_chunk.0).abs() > 1 || (z - player_chunk.1).abs() > 1 {
            sim::voxel::remove_chunk(x, z);
            false
        } else {
            true
        }
    });
    // The narrow aerial view looks hundreds of blocks down and ahead. Track
    // the terrain under that ray, while retaining the real eye for rendering.
    let remote_terrain_center = cameras.iter().next().filter(|_| remote_view).map(|camera| {
        let at = sim::voxel::to_block(origin, camera.translation.to_array());
        let at = glam::DVec3::from_array(at);
        let ahead = camera.rotation * Vec3::NEG_Z;
        let direction =
            glam::DVec3::new(f64::from(ahead.x), f64::from(ahead.z), -f64::from(ahead.y));
        let ground_y = feet[1] + 1.62;
        // The frustum's far plane is 1024 blocks. A near-horizontal ray
        // cannot require terrain beyond it or an unbounded view area.
        let distance = if direction.y < -0.001 && at.y > ground_y {
            ((ground_y - at.y) / direction.y).clamp(0.0, 1024.0)
        } else {
            0.0
        };
        let target = at + direction * distance;
        glam::DVec3::new(target.x, ground_y, target.z)
    });
    // Admission and respawn need the nominal spawn's physical floor first,
    // even if the previous life died beyond that area's streamed chunks.
    let stream_block = if !*was_alive {
        (
            origin[0].floor() as i32,
            origin[1].floor() as i32,
            origin[2].floor() as i32,
        )
    } else {
        remote_terrain_center
            .map(|at| {
                (
                    at.x.floor() as i32,
                    at.y.floor() as i32,
                    at.z.floor() as i32,
                )
            })
            .unwrap_or(block)
    };
    let (loaded, forgotten) = world.stream.server_tick(stream_block, &mut world.scene);
    let spawn_center = if !*was_alive {
        [0.0; 2]
    } else {
        [ps.origin[0], ps.origin[1]]
    };
    sim::voxel::set_spawn_center(spawn_center);
    if let Some(authority) = authority.as_mut() {
        for (id, _) in authority.0.clients_scoreboard() {
            if id == local.0 {
                continue;
            }
            let Some(remote) = authority.0.player(id) else {
                continue;
            };
            if remote.health <= 0 || remote.pm_type != 0 {
                continue;
            }
            let remote_feet = sim::voxel::to_block(origin, remote.origin);
            let peer_chunk = (
                remote_feet[0].floor() as i32 >> 4,
                remote_feet[2].floor() as i32 >> 4,
            );
            // A killstreak camera can stream terrain far from the player's
            // body. It must not repeatedly relocate combatants in the arena.
            let center_chunk = player_chunk;
            if (peer_chunk.0 - center_chunk.0).abs() > 5
                || (peer_chunk.1 - center_chunk.1).abs() > 5
            {
                // Recover each player against the current authoritative positions.
                // A fixed offset piled all returning bots into the same body box.
                let avoid: Vec<_> = authority
                    .0
                    .clients_scoreboard()
                    .into_iter()
                    .filter_map(|(other, _)| {
                        (other != id)
                            .then(|| authority.0.player(other))
                            .flatten()
                            .filter(|player| player.pm_type == 0 && player.health > 0)
                            .map(|player| player.origin)
                    })
                    .collect();
                if let Some(safe) = sim::voxel::spawn_clear(&avoid)
                    && authority.0.teleport(id, safe)
                {
                    diag::warn!(
                        World,
                        "Minecraft player {} returned to the shared loaded area",
                        id.0
                    );
                }
            }
        }
    }
    for chunk in loaded {
        let blocks = &world.registries.blocks;
        let (min_y, height) = (chunk.min_y(), chunk.height());
        let mut ids = vec![0u16; (height * 256) as usize];
        // Runs of one state (air, stone) skip the map.
        let mut last = None;
        for y in 0..height {
            for z in 0..16usize {
                for x in 0..16usize {
                    let state = chunk.block(x, min_y + y, z);
                    if let Some((last_state, id)) = last
                        && last_state == state
                    {
                        ids[((y as usize * 16) + z) * 16 + x] = id;
                        continue;
                    }
                    let id = block_shape(blocks, state, shapes, shape_ids);
                    last = Some((state, id));
                    ids[((y as usize * 16) + z) * 16 + x] = id;
                }
            }
        }
        if let Some(entities) = entities.as_mut() {
            entities.load_chunk(&chunk);
        }
        sim::voxel::set_chunk(
            chunk.pos.x,
            chunk.pos.z,
            sim::voxel::VoxelChunk {
                min_y,
                height,
                shapes: ids,
            },
        );
        retained_collision.remove(&(chunk.pos.x, chunk.pos.z));
    }
    for pos in forgotten {
        let key = (pos.x, pos.z);
        let supports_player = (remote_view || retained_collision.contains(&key))
            && (pos.x - player_chunk.0).abs() <= 1
            && (pos.z - player_chunk.1).abs() <= 1;
        if supports_player {
            retained_collision.insert(key);
        } else {
            retained_collision.remove(&key);
            sim::voxel::remove_chunk(pos.x, pos.z);
        }
        if let Some(entities) = entities.as_mut() {
            entities.unload_chunk(pos);
        }
    }

    // Generated spawn height is only a hint. Uploaded collision must prove
    // a clear standing box and a walkable floor before admitting a class.
    let spawn_at =
        if !*was_alive && sim::voxel::spawn_area_ready(world.stream.view_distance().min(2)) {
            if let Some(authority) = authority.as_ref() {
                let avoid: Vec<_> = authority
                    .0
                    .clients_scoreboard()
                    .into_iter()
                    .filter_map(|(id, _)| {
                        (id != local.0)
                            .then(|| authority.0.player(id))
                            .flatten()
                            .filter(|player| player.pm_type == 0 && player.health > 0)
                            .map(|player| player.origin)
                    })
                    .collect();
                sim::voxel::spawn_existing(ps.origin, &avoid)
                    .or_else(|| sim::voxel::spawn_clear(&avoid))
            } else {
                sim::voxel::spawn_origin()
            }
        } else {
            None
        };
    ui.loading_world = !*was_alive && spawn_at.is_none();
    if alive
        && !*was_alive
        && let Some(at) = spawn_at
    {
        if let Some(authority) = authority.as_mut() {
            if authority.0.teleport(local.0, at) {
                diag::info!(
                    World,
                    "Minecraft spawn: moved to clear world spawn {:?}",
                    at
                );
                *was_alive = true;
            }
        } else if *role == frame::RuntimeRole::Client {
            *was_alive = true;
        }
    }

    // Minecraft's yaw: 0 facing +Z (map -Y), turning towards -X.
    let yaw_rad = ps.viewangles[1].to_radians();
    let mc_yaw = (-yaw_rad.cos()).atan2(-yaw_rad.sin()).to_degrees();
    let mut all_events = sim::voxel::take_events();
    if let Some(authority) = authority.as_ref() {
        let mut handled = HashMap::<sim::ClientId, u8>::new();
        for (client, kind, position, state) in std::mem::take(&mut minecraft_sync.incoming_intents)
        {
            let count = handled.entry(client).or_default();
            if *count >= 4 {
                continue;
            }
            *count += 1;
            let Some(remote) = authority.0.player(client).filter(|p| p.pm_type == 0) else {
                continue;
            };
            let remote_feet = sim::voxel::to_block(origin, remote.origin);
            let delta = glam::DVec3::new(
                f64::from(position[0]) + 0.5 - remote_feet[0],
                f64::from(position[1]) + 0.5 - remote_feet[1],
                f64::from(position[2]) + 0.5 - remote_feet[2],
            );
            if kind != 2 && delta.length() > 6.0 {
                continue;
            }
            let yaw_rad = remote.viewangles[1].to_radians();
            let remote_yaw = (-yaw_rad.cos()).atan2(-yaw_rad.sin()).to_degrees();
            let mut actor = minecraftoss_player::Player::new(glam::DVec3::from_array(remote_feet));
            actor.yaw = f64::from(remote_yaw);
            actor.pitch = f64::from(remote.viewangles[0]);
            let pos = (position[0], position[1], position[2]);
            match kind {
                3 if actor
                    .target(&world.scene, 5.0)
                    .is_some_and(|hit| hit.pos == pos) =>
                {
                    if let Some(entities) = entities.as_mut() {
                        entities.use_block(&world.scene, pos, remote_yaw);
                    }
                }
                2 => {
                    if let (Some(entities), Some(meta)) =
                        (entities.as_mut(), authority.0.client_meta(client))
                    {
                        let yaw = f64::from(remote_yaw).to_radians();
                        let pitch = f64::from(remote.viewangles[0]).to_radians();
                        let eye = glam::DVec3::from_array(remote_feet) + glam::DVec3::Y * 1.62;
                        let look = glam::DVec3::new(
                            -yaw.sin() * pitch.cos(),
                            -pitch.sin(),
                            yaw.cos() * pitch.cos(),
                        );
                        entities.remote_punch(
                            u64::from(client.0) + 1,
                            eye,
                            look,
                            remote_yaw,
                            sim::voxel::MobAttackCredit {
                                client,
                                life: meta.life_sequence,
                            },
                        );
                    }
                }
                0 if actor
                    .target(&world.scene, 4.5)
                    .is_some_and(|hit| hit.pos == pos) =>
                {
                    remote_mining_owner.insert(pos, client);
                    all_events.push(sim::voxel::VoxelEvent::Shot {
                        block: position,
                        damage: 160.0 / 30.0,
                        drop_items: true,
                    });
                }
                1 if state != BlockStateId::AIR.0
                    && (state as usize) < world.registries.blocks.state_count() =>
                {
                    let Some(stock) = remote_stock.get_mut(&client) else {
                        continue;
                    };
                    let Some(available) = stock.get_mut(&state).filter(|count| **count > 0) else {
                        continue;
                    };
                    let Some(block) = world.stream.states.block(BlockStateId(state)).cloned()
                    else {
                        continue;
                    };
                    let old = minecraft_terrain::scene::Scene::block(&world.scene, pos).cloned();
                    let block = minecraftoss_player::Block {
                        id: block.id.key(),
                        properties: block.properties,
                    };
                    if let Some(placed) = actor.place_target_with(&mut world.scene, block) {
                        if placed != pos {
                            world.scene.set(placed, None);
                            continue;
                        }
                        let [fx, fy, fz] = remote_feet;
                        let inside = (fx - 0.3) < f64::from(pos.0 + 1)
                            && (fx + 0.3) > f64::from(pos.0)
                            && fy < f64::from(pos.1 + 1)
                            && (fy + 1.8) > f64::from(pos.1)
                            && (fz - 0.3) < f64::from(pos.2 + 1)
                            && (fz + 0.3) > f64::from(pos.2);
                        if inside {
                            world.scene.set(pos, old);
                            continue;
                        }
                        let placed_block =
                            minecraft_terrain::scene::Scene::block(&world.scene, pos);
                        let placed_state =
                            placed_block.and_then(|b| world.stream.states.state_of(b));
                        let shape = placed_state.map_or(0, |id| {
                            block_shape(&world.registries.blocks, id, shapes, shape_ids)
                        });
                        sim::voxel::set_block_shape(pos.0, pos.1, pos.2, shape);
                        world.stream.record_edits(&world.scene, &[pos]);
                        world.stream.mark_edited(&world.scene, &[pos]);
                        changed_blocks.insert(pos);
                        *available -= 1;
                        if *available == 0 {
                            stock.remove(&state);
                        }
                        stock_dirty.insert(client);
                    }
                }
                _ => {}
            }
        }
    } else {
        minecraft_sync.incoming_intents.clear();
    }

    // The hand, when no gun is selected: vanilla's left click mines by hand
    // (with the hand's break speed) or punches, its right click places the
    // held block (`Player.place_selected`, vanilla's placement states).
    // Footsteps (`Entity.applyMovementEmissionAndPlaySound`): the walked
    // distance grows by 0.6 of each horizontal move on the ground, and past
    // the next step the block under the feet sounds its step at 0.15 of its
    // volume. A fall of more than three blocks lands with the block's fall
    // sound and the player's (`LivingEntity.causeFallDamage`).
    let on_ground = alive && ps.ground_entity_num != playerstate_iw4::ENTITYNUM_NONE;
    if let Some(last) = steps.last.filter(|_| alive) {
        let horizontal = ((feet[0] - last[0]).hypot(feet[2] - last[2]) * 0.6) as f32;
        let block_at = |dy: f64| {
            let pos = (
                feet[0].floor() as i32,
                (feet[1] - dy).floor() as i32,
                feet[2].floor() as i32,
            );
            minecraft_terrain::scene::Scene::block(&world.scene, pos)
                .cloned()
                .map(|b| (pos, b))
        };
        let under = block_at(0.2);
        let centre = |pos: (i32, i32, i32)| {
            Vec3::from_array(sim::voxel::to_map(
                origin,
                [pos.0 as f64 + 0.5, pos.1 as f64 + 1.0, pos.2 as f64 + 0.5],
            ))
        };
        if on_ground && horizontal < 2.0 {
            steps.move_dist += horizontal;
            if steps.move_dist > steps.next_step
                && let Some((pos, block)) = under.as_ref()
            {
                steps.next_step = steps.move_dist as i32 as f32 + 1.0;
                // Snow layers and carpets sound instead of what they lie on.
                let inside = block_at(-0.01).filter(|(_, b)| {
                    let p = b.id.path.as_str();
                    p == "snow" || p.ends_with("_carpet") || p == "moss_carpet"
                });
                let (pos, block) = inside.as_ref().map_or((*pos, block), |(p, b)| (*p, b));
                if let (Some(sounds), Some(kind)) = (sounds.as_mut(), world.scene.sound_type(block))
                {
                    sounds.play(
                        &world.packs,
                        &kind.step,
                        Some(centre(pos)),
                        kind.volume * 0.15,
                        kind.pitch,
                    );
                }
            }
        }
        if on_ground {
            if let Some(peak) = steps.air_peak.take() {
                let fall = peak - feet[1];
                if fall > 3.0
                    && let Some(sounds) = sounds.as_mut()
                {
                    let event = if fall > 7.0 {
                        "minecraft:entity.player.big_fall"
                    } else {
                        "minecraft:entity.player.small_fall"
                    };
                    sounds.play(&world.packs, event, None, 1.0, 1.0);
                    if let Some((pos, block)) = under.as_ref()
                        && let Some(kind) = world.scene.sound_type(block)
                    {
                        sounds.play(
                            &world.packs,
                            &kind.fall,
                            Some(centre(*pos)),
                            kind.volume * 0.5,
                            kind.pitch * 0.75,
                        );
                    }
                }
            }
        } else {
            steps.air_peak = Some(steps.air_peak.map_or(feet[1], |p| p.max(feet[1])));
        }
    } else {
        steps.air_peak = None;
    }
    steps.last = alive.then_some(feet);

    let dt_hand = time.delta_secs_f64();
    if let Some(ticks) = hand.swing.as_mut() {
        *ticks += (dt_hand * 20.0) as f32;
        if *ticks >= crate::minecraft_hand::SWING_TICKS {
            hand.swing = None;
        }
    }
    let holding = entities.as_ref().is_some_and(|e| {
        e.inventory.slots[e.selected]
            .as_ref()
            .is_none_or(|s| crate::minecraft_inventory::weapon_of(s).is_none())
    });
    ui.holding_item = alive && holding;
    ui.empty_hand = ui.holding_item
        && entities
            .as_ref()
            .is_some_and(|e| e.inventory.slots[e.selected].is_none());
    hand.clock += dt_hand;
    let hand_ticks = (hand.clock / TICK_SECONDS) as u32;
    hand.clock -= f64::from(hand_ticks) * TICK_SECONDS;
    // Minecraft use is separate from MW2 ADS and crate pickup, and works
    // with a gun selected as well as with an item or an empty hand.
    let use_block = buttons.just_pressed(MouseButton::Middle) || std::mem::take(&mut ui.interaction_requested);
    if alive
        && !ui.inventory_open
        && use_block
        && !presented
            .snapshot()
            .and_then(|s| s.meta.for_client(local.0))
            .is_some_and(|meta| {
                meta.controls.usability_disabled
                    || meta.controls.linked
                    || meta.remote_missile.is_some()
            })
        && let Some(entities) = entities.as_mut()
    {
        let mut player = minecraftoss_player::Player::new(glam::DVec3::from_array(feet));
        player.yaw = f64::from(mc_yaw);
        player.pitch = f64::from(ps.viewangles[0]);
        if let Some(hit) = player.target(&world.scene, 5.0) {
            let crafting = minecraft_terrain::scene::Scene::block(&world.scene, hit.pos)
                .is_some_and(|block| block.id.path == "crafting_table");
            if crafting {
                ui.workbench_open = true;
                ui.inventory_open = true;
            } else if authority.is_some() {
                entities.use_block(&world.scene, hit.pos, mc_yaw);
            } else {
                let _ = action_inbox.push(
                    local.0,
                    sim::ClientAction::MinecraftInteract {
                        request_id: request_ids.allocate(),
                        kind: 3,
                        position: [hit.pos.0, hit.pos.1, hit.pos.2],
                        state: 0,
                    },
                );
            }
        }
    }
    if let Some(entities) = entities.as_mut()
        && ui.holding_item
        && !ui.inventory_open
    {
        let mut player = minecraftoss_player::Player::new(glam::DVec3::from_array(feet));
        player.yaw = f64::from(mc_yaw);
        player.pitch = f64::from(ps.viewangles[0]);
        player.selected = entities.selected;
        let eye_block = glam::DVec3::from_array(feet) + glam::DVec3::Y * 1.62;
        let look = {
            let (yaw, pitch) = (
                f64::from(mc_yaw).to_radians(),
                f64::from(ps.viewangles[0]).to_radians(),
            );
            glam::DVec3::new(
                -yaw.sin() * pitch.cos(),
                -pitch.sin(),
                yaw.cos() * pitch.cos(),
            )
        };
        if buttons.just_pressed(MouseButton::Left)
            || pad_trigger(pad, GamepadButton::RightTrigger2, true)
        {
            hand.swing = Some(0.0);
            if let Some(meta) = authority
                .as_ref()
                .and_then(|authority| authority.0.client_meta(local.0))
            {
                entities.punch(
                    eye_block,
                    look,
                    mc_yaw,
                    sim::voxel::MobAttackCredit {
                        client: local.0,
                        life: meta.life_sequence,
                    },
                );
            } else if authority.is_none() {
                let _ = action_inbox.push(
                    local.0,
                    sim::ClientAction::MinecraftInteract {
                        request_id: request_ids.allocate(),
                        kind: 2,
                        position: [0; 3],
                        state: 0,
                    },
                );
            }
        }
        if buttons.pressed(MouseButton::Left)
            || pad_trigger(pad, GamepadButton::RightTrigger2, false)
        {
            for _ in 0..hand_ticks {
                if let Some(hit) = player.target(&world.scene, 4.5) {
                    // A hand mines as vanilla's `getDestroyProgress`: a
                    // block's hardness times thirty ticks.
                    if authority.is_some() {
                        all_events.push(sim::voxel::VoxelEvent::Shot {
                            block: [hit.pos.0, hit.pos.1, hit.pos.2],
                            damage: 160.0 / 30.0,
                            drop_items: true,
                        });
                    } else {
                        let _ = action_inbox.push(
                            local.0,
                            sim::ClientAction::MinecraftInteract {
                                request_id: request_ids.allocate(),
                                kind: 0,
                                position: [hit.pos.0, hit.pos.1, hit.pos.2],
                                state: 0,
                            },
                        );
                    }
                    if hand
                        .swing
                        .is_none_or(|t| t >= crate::minecraft_hand::SWING_TICKS * 0.5)
                    {
                        hand.swing = Some(0.0);
                    }
                }
            }
        }
        hand.place_delay = hand.place_delay.saturating_sub(hand_ticks);
        let place = (buttons.just_pressed(MouseButton::Right)
            || pad_trigger(pad, GamepadButton::LeftTrigger2, true))
            || ((buttons.pressed(MouseButton::Right)
                || pad_trigger(pad, GamepadButton::LeftTrigger2, false))
                && hand.place_delay == 0);
        if place && authority.is_none() {
            hand.place_delay = 4;
            if let Some(slot) = entities
                .inventory
                .slots
                .get(entities.selected)
                .and_then(Option::as_ref)
                && slot.count > 0
                && let Some(hit) = player.target(&world.scene, 5.0)
            {
                let (dx, dy, dz) = hit.face.offset();
                let neighbour = (hit.pos.0 + dx, hit.pos.1 + dy, hit.pos.2 + dz);
                let old_hit =
                    minecraft_terrain::scene::Scene::block(&world.scene, hit.pos).cloned();
                let old_neighbour =
                    minecraft_terrain::scene::Scene::block(&world.scene, neighbour).cloned();
                let block = minecraftoss_player::Block::new(slot.id.clone());
                if let Some(pos) = player.place_target_with(&mut world.scene, block) {
                    let state = minecraft_terrain::scene::Scene::block(&world.scene, pos)
                        .and_then(|block| world.stream.states.state_of(block));
                    world.scene.set(hit.pos, old_hit);
                    world.scene.set(neighbour, old_neighbour);
                    if let Some(state) = state {
                        let _ = action_inbox.push(
                            local.0,
                            sim::ClientAction::MinecraftInteract {
                                request_id: request_ids.allocate(),
                                kind: 1,
                                position: [pos.0, pos.1, pos.2],
                                state: state.0,
                            },
                        );
                    }
                }
            }
        }
        if place && authority.is_some() {
            hand.place_delay = 4;
            if let Some(pos) = player.place_selected(
                &mut world.scene,
                &mut entities.inventory,
                minecraftoss_player::GameMode::Survival,
            ) {
                // Not into the player's own box.
                let [fx, fy, fz] = feet;
                let inside = (fx - 0.3) < f64::from(pos.0 + 1)
                    && (fx + 0.3) > f64::from(pos.0)
                    && fy < f64::from(pos.1 + 1)
                    && (fy + 1.8) > f64::from(pos.1)
                    && (fz - 0.3) < f64::from(pos.2 + 1)
                    && (fz + 0.3) > f64::from(pos.2);
                let block = minecraft_terrain::scene::Scene::block(&world.scene, pos).cloned();
                if inside {
                    world.scene.set(pos, None);
                    if let Some(block) = block {
                        let _ = entities.inventory.add_item(
                            minecraftoss_player::inventory::ItemStack::new(block.id.key(), 1),
                            entities.selected,
                        );
                    }
                } else if let Some(block) = block {
                    let state = world.stream.states.state_of(&block);
                    let blocks = &world.registries.blocks;
                    let shape =
                        state.map_or(0, |state| block_shape(blocks, state, shapes, shape_ids));
                    sim::voxel::set_block_shape(pos.0, pos.1, pos.2, shape);
                    world.stream.record_edits(&world.scene, &[pos]);
                    world.stream.mark_edited(&world.scene, &[pos]);
                    changed_blocks.insert(pos);
                    entities.placed(&world.scene, pos);
                    hand.swing = Some(0.0);
                    if let (Some(sounds), Some(kind)) =
                        (sounds.as_mut(), world.scene.sound_type(&block))
                    {
                        let centre = [pos.0 as f64 + 0.5, pos.1 as f64 + 0.5, pos.2 as f64 + 0.5];
                        let at = Vec3::from_array(sim::voxel::to_map(origin, centre));
                        sounds.play(
                            &world.packs,
                            &kind.place,
                            Some(at),
                            (kind.volume + 1.0) / 2.0,
                            kind.pitch * 0.8,
                        );
                    }
                }
            }
        }
    }

    // Shots and explosions from the authoritative game: bullets that met a
    // mob hurt it, the rest mine.
    let (mob_shots, events): (Vec<_>, Vec<_>) = all_events
        .into_iter()
        .partition(|event| matches!(event, sim::voxel::VoxelEvent::MobShot { .. }));
    if let Some(entities) = entities.as_mut() {
        for shot in mob_shots {
            if let sim::voxel::VoxelEvent::MobShot {
                key,
                damage,
                from,
                credit,
            } = shot
            {
                entities.shoot(key, damage, from, mc_yaw, credit);
            }
        }
    }
    // Vanilla's block sounds: a hit for each bullet into a block, then the
    // break of each block broken (`SoundType` volume and pitch as
    // `MultiPlayerGameMode` and `LevelRenderer` scale them), and blasts.
    let at = |b: [f64; 3]| Vec3::from_array(sim::voxel::to_map(origin, b));
    if let Some(sounds) = sounds.as_mut() {
        for event in &events {
            match *event {
                sim::voxel::VoxelEvent::Shot { block, .. } => {
                    let pos = (block[0], block[1], block[2]);
                    if let Some(kind) = minecraft_terrain::scene::Scene::block(&world.scene, pos)
                        .and_then(|b| world.scene.sound_type(b))
                    {
                        let centre = [
                            block[0] as f64 + 0.5,
                            block[1] as f64 + 0.5,
                            block[2] as f64 + 0.5,
                        ];
                        sounds.play(
                            &world.packs,
                            &kind.hit,
                            Some(at(centre)),
                            (kind.volume + 1.0) / 4.0,
                            kind.pitch * 0.5,
                        );
                    }
                }
                sim::voxel::VoxelEvent::Explosion { center, .. } => {
                    let pitch = (1.0 + (sounds.random() - sounds.random()) * 0.2) * 0.7;
                    sounds.play(
                        &world.packs,
                        "minecraft:entity.generic.explode",
                        Some(at(center)),
                        4.0,
                        pitch,
                    );
                }
                sim::voxel::VoxelEvent::MobShot { .. } | sim::voxel::VoxelEvent::Ray { .. } => {}
            }
        }
    }
    let broken = mining.apply(
        events,
        &mut crate::minecraft_mining::WorldRefs {
            stream: &mut world.stream,
            scene: &mut world.scene,
            packs: &world.packs,
            atlas: &world.atlas,
            registries: &world.registries,
        },
        time.elapsed_secs_f64(),
    );
    let mut remote_drops = HashSet::new();
    if authority.is_some() {
        for (pos, block, _, drops) in &broken {
            if let Some(client) = remote_mining_owner.remove(pos)
                && *drops
                && let Some(state) = world.stream.states.state_of(block)
            {
                let stock = remote_stock.entry(client).or_default();
                if stock.len() < 36 || stock.contains_key(&state.0) {
                    let count = stock.entry(state.0).or_default();
                    *count = count.saturating_add(1);
                    stock_dirty.insert(client);
                    remote_drops.insert(*pos);
                }
            }
        }
        remote_mining_owner
            .retain(|pos, _| minecraft_terrain::scene::Scene::block(&world.scene, *pos).is_some());
        if remote_mining_owner.len() > 4_096 {
            remote_mining_owner.clear();
        }
    }
    if authority.is_some() {
        changed_blocks.extend(broken.iter().map(|(pos, ..)| *pos));
    }
    if let Some(sounds) = sounds.as_mut() {
        for (pos, block, blast, _) in &broken {
            if *blast {
                continue;
            }
            if let Some(kind) = world.scene.sound_type(block) {
                let centre = [pos.0 as f64 + 0.5, pos.1 as f64 + 0.5, pos.2 as f64 + 0.5];
                sounds.play(
                    &world.packs,
                    &kind.break_sound,
                    Some(at(centre)),
                    (kind.volume + 1.0) / 2.0,
                    kind.pitch * 0.8,
                );
            }
        }
    }
    if let Some(entities) = entities.as_mut() {
        entities.broke(&world.scene, &broken);
        let local_drops: Vec<_> = broken
            .iter()
            .filter(|(pos, ..)| !remote_drops.contains(pos))
            .cloned()
            .collect();
        entities.drop_blocks(&local_drops);
    }

    let eye = sim::voxel::to_block(
        origin,
        [
            ps.origin[0],
            ps.origin[1],
            ps.origin[2] + ps.view_height_current,
        ],
    );
    let (pitch, yaw) = (ps.viewangles[0].to_radians(), ps.viewangles[1].to_radians());
    let map_forward = [
        pitch.cos() * yaw.cos(),
        pitch.cos() * yaw.sin(),
        -pitch.sin(),
    ];
    let forward = glam::Vec3::new(map_forward[0], map_forward[2], -map_forward[1]);
    let aspect = windows
        .single()
        .map(|w| w.width() / w.height().max(1.0))
        .unwrap_or(16.0 / 9.0);
    let detached_camera = cameras
        .iter()
        .next()
        .filter(|_| skate.active || remote_view)
        .map(|t| {
            let at = sim::voxel::to_block(origin, t.translation.to_array());
            let ahead = t.rotation * Vec3::NEG_Z;
            (
                glam::DVec3::new(at[0], at[1], at[2]),
                glam::Vec3::new(ahead.x, ahead.z, -ahead.y).normalize_or(forward),
            )
        });
    let (cull_at, cull_forward) =
        detached_camera.unwrap_or((glam::DVec3::new(eye[0], eye[1], eye[2]), forward));
    let cull_fov = if remote_view {
        lenses
            .iter()
            .find_map(|projection| match projection {
                Projection::Perspective(lens) => Some(lens.fov.to_degrees()),
                _ => None,
            })
            .unwrap_or(90.0)
    } else if skate.active {
        120.0
    } else {
        90.0
    };
    let camera = CullCamera {
        position: cull_at,
        terrain_center: remote_terrain_center,
        forward: cull_forward,
        fov_degrees: cull_fov,
        aspect,
        yaw_degrees: (-cull_forward.x).atan2(cull_forward.z).to_degrees(),
        pitch_degrees: (-cull_forward.y).asin().to_degrees(),
    };
    let update = world.stream.frame(
        &world.scene,
        &camera,
        FADE_MILLIS,
        &world.atlas,
        &world.packs,
    );
    view.uploads.extend(update.uploads);
    view.removed.extend(update.removed);
    view.visible = update.visible;
    let Some(light) = light.as_mut() else {
        return;
    };
    for (chunk, column) in update.lights {
        light.set_chunk_column(chunk, column);
    }

    if let Some(sounds) = sounds.as_mut() {
        sound_queue.0.append(&mut sounds.queued);
    }
    let selecting = presented
        .snapshot()
        .and_then(|snapshot| snapshot.meta.for_client(local.0))
        .is_some_and(|meta| meta.location_selection.is_some());
    ui.minimap = minimap
        .update(
            time.delta_secs_f64(),
            feet,
            selecting,
            &world.scene,
            &world.packs,
            &world.atlas,
            &mut images,
        )
        .map(|(image, [bx, bz])| {
            let corner = |x: i32, z: i32| {
                let p = sim::voxel::to_map(origin, [f64::from(x), feet[1], f64::from(z)]);
                [p[0], p[1]]
            };
            let size = crate::minecraft_minimap::SIZE;
            (image, corner(bx, bz), corner(bx + size, bz + size))
        });
    sim::voxel::set_minimap(
        ui.minimap
            .as_ref()
            .and_then(|(_, a, b)| hud_iw4::compass_map_bounds_from_minimap_corners(*a, *b, 90.0)),
    );
    // The Overworld clock and the environment attributes of MinecraftOSS.
    let dt = time.delta_secs_f64();
    let partial = mining.tick(&world.scene, dt);
    view.particles = mining.particle_mesh(&world.atlas, forward, partial, light);

    // The mobs: a server tick when due, the blocks it changed, its hits on
    // the player, the mobs' boxes for bullets and their meshes.
    let mut mob_ticks = 0;
    if let Some(entities) = entities.as_mut().filter(|_| authority.is_some()) {
        let player = crate::minecraft_entities::PlayerView {
            id: u64::from(local.0.0) + 1,
            feet,
            alive,
            health: ps.health as f32,
            yaw: mc_yaw,
            pitch: ps.viewangles[0],
        };
        let bright_outside = world.environment.sky_light_level() > 11.0;
        let peers: Vec<_> = authority.as_ref().map_or_else(Vec::new, |authority| {
            authority
                .0
                .clients_scoreboard()
                .into_iter()
                .filter_map(|(id, _)| {
                    if id == local.0 {
                        return None;
                    }
                    let ps = authority.0.player(id)?;
                    let feet = sim::voxel::to_block(origin, ps.origin);
                    let yaw_rad = ps.viewangles[1].to_radians();
                    Some(crate::minecraft_entities::PlayerView {
                        id: u64::from(id.0) + 1,
                        feet,
                        alive: ps.pm_type == 0,
                        health: ps.health as f32,
                        yaw: (-yaw_rad.cos()).atan2(-yaw_rad.sin()).to_degrees(),
                        pitch: ps.viewangles[0],
                    })
                })
                .collect()
        });
        let ticks_before = entities.client_ticks();
        let (changes, hits) = entities.tick(dt, day.ticks as i64, bright_outside, &player, &peers);
        for (credit, kills) in entities.take_feedback() {
            if let Some(authority) = authority.as_mut() {
                authority.0.confirm_mob_hit(credit, kills);
            }
        }
        mob_ticks = (entities.client_ticks() - ticks_before) as u32;
        if !changes.is_empty() {
            let blocks = &world.registries.blocks;
            let mut positions = Vec::with_capacity(changes.len());
            for (pos, block) in changes {
                changed_blocks.insert(pos);
                let state = block.as_ref().and_then(|b| world.stream.states.state_of(b));
                let shape = state.map_or(0, |state| block_shape(blocks, state, shapes, shape_ids));
                sim::voxel::set_block_shape(pos.0, pos.1, pos.2, shape);
                world.scene.set(pos, block);
                positions.push(pos);
            }
            world.stream.mark_edited(&world.scene, &positions);
        }
        for (player_id, amount, from) in hits {
            sim::voxel::push_player_damage(
                player_id.saturating_sub(1) as u32,
                amount,
                from.map(|b| sim::voxel::to_map(origin, b)),
            );
        }
    }
    if let Some(entities) = entities.as_mut() {
        // The inventory: MW2 guns as items, the HUD's clicks, the hotbar's
        // gun, and what the HUD shows.
        let owned: Vec<u32> = ps
            .weapons
            .iter()
            .filter(|&&w| w > 0)
            .map(|&w| w as u32)
            .filter(|&w| {
                authority.as_ref().map_or(true, |authority| {
                    authority
                        .0
                        .weapon_combat_row(w)
                        .is_some_and(|facts| facts.inventory_type == 0)
                })
            })
            .collect();
        ui.active = alive;
        if !alive {
            ui.inventory_open = false;
        }
        inventory_ui.sync_weapons(&mut entities.inventory, &owned);
        let mut selected = entities.selected;
        let thrown = inventory_ui.apply_input(&mut ui, &mut entities.inventory, &mut selected);
        let thrower = crate::minecraft_inventory::Thrower {
            eye: glam::DVec3::from_array(eye),
            yaw: mc_yaw,
            pitch: ps.viewangles[0],
        };
        crate::minecraft_inventory::throw(&mut entities.world_items, thrown, &thrower);
        ui.weapon_request =
            inventory_ui.weapon_request(&entities.inventory, &mut selected, ps.weapon as u32);
        entities.selected = selected;
        inventory_ui.publish(
            &mut ui,
            &entities.inventory,
            selected,
            &world.packs,
            &mut images,
        );

        if let Some(sounds) = sounds.as_mut() {
            for (event, position, volume, pitch) in std::mem::take(&mut entities.sounds) {
                sounds.play(
                    &world.packs,
                    &event,
                    Some(at(position.to_array())),
                    volume,
                    pitch,
                );
            }
        }
        // Minecraft items and the blocky right arm use the same atlas/pass.
        view.hand = Default::default();
        let swing = hand.swing.map_or(0.0, |t| {
            (t / crate::minecraft_hand::SWING_TICKS).clamp(0.0, 1.0)
        });
        ui.hand_swing = swing;
        if ui.holding_item && !puppet.active {
            let eye_light_at = glam::Vec3::new(eye[0] as f32, eye[1] as f32, eye[2] as f32);
            let stack = entities.inventory.slots[entities.selected].clone();
            let mesh = if let Some(stack) = stack {
                let display = minecraft_terrain::pack::ResourceId::parse(&stack.id)
                    .ok()
                    .and_then(|id| {
                        minecraft_terrain::model::item_first_person_transform(&world.packs, &id)
                            .ok()
                    })
                    .unwrap_or(glam::Mat4::IDENTITY);
                let pose = crate::minecraft_hand::item_pose(display, swing, 0.0);
                entities.held_item_mesh(
                    &stack.id,
                    pose,
                    eye_light_at,
                    &world.packs,
                    &world.atlas,
                    light,
                )
            } else {
                minecraft_terrain::cow_render::first_person_arm(
                    &world.atlas,
                    crate::minecraft_hand::arm_pose(swing),
                    view.eye_light,
                )
            };
            let vertices: Vec<minecraft_terrain::mesh::SectionVertex> = mesh
                .vertices
                .iter()
                .map(minecraft_terrain::mesh::SectionVertex::from_vertex)
                .collect();
            view.hand = (bytemuck::cast_slice(&vertices).to_vec(), mesh.indices);
            // Reverse-Z with no far plane, as the scene's; 70 degrees up.
            let f = 1.0 / (35.0f32.to_radians()).tan();
            let near = 0.05;
            view.hand_clip = Mat4::from_cols(
                Vec4::new(f / aspect, 0.0, 0.0, 0.0),
                Vec4::new(0.0, f, 0.0, 0.0),
                Vec4::new(0.0, 0.0, 0.0, -1.0),
                Vec4::new(0.0, 0.0, near, 0.0),
            )
            .to_cols_array();
        }

        sim::voxel::set_mob_boxes(entities.boxes());
        entities.tick_scene(&world.scene, mob_ticks);
        let sky_darken = (15.0 - world.environment.sky_light_level()).clamp(0.0, 15.0) as u8;
        let meshes = entities.meshes(
            &world.scene,
            &world.packs,
            &world.atlas,
            light,
            forward,
            glam::DVec3::from_array(eye),
            sky_darken,
        );
        let raw = |mesh: &minecraft_terrain::mesh::ChunkMesh| {
            (
                bytemuck::cast_slice::<_, u8>(&mesh.vertices).to_vec(),
                mesh.indices.clone(),
            )
        };
        view.entity_meshes = [
            raw(&meshes.models),
            raw(&meshes.culled),
            raw(&meshes.translucent),
            raw(&meshes.shadows),
        ];
        let mesh = meshes.items;
        let (bytes, indices) = &mut view.particles;
        let base =
            (bytes.len() / std::mem::size_of::<minecraft_terrain::mesh::SectionVertex>()) as u32;
        let vertices: Vec<minecraft_terrain::mesh::SectionVertex> = mesh
            .vertices
            .iter()
            .map(minecraft_terrain::mesh::SectionVertex::from_vertex)
            .collect();
        bytes.extend_from_slice(bytemuck::cast_slice(&vertices));
        indices.extend(mesh.indices.iter().map(|i| i + base));
    }
    view.cracks = mining.crack_mesh();
    day.advance(dt);
    // Remote environment and fog originate at the missile, with enough
    // depth to see the same bounded patch of ground its view area tracks.
    let (eye, forward) = if remote_view {
        (cull_at.to_array(), cull_forward)
    } else {
        (eye, forward)
    };
    let remote_fog_depth =
        remote_terrain_center.map_or(0.0, |center| cull_at.distance(center) as f32);
    let eye_block = (
        eye[0].floor() as i32,
        eye[1].floor() as i32,
        eye[2].floor() as i32,
    );
    view.eye_light = [
        f32::from(light.get(eye_block)),
        f32::from(light.get_block(eye_block)),
    ];
    world
        .environment
        .update_rain_fog(0.0, light.get(eye_block), false, (dt * 20.0) as f32);
    *environment_accumulator += dt;
    if !*environment_primed || *environment_accumulator >= TICK_SECONDS {
        *environment_accumulator = (*environment_accumulator % TICK_SECONDS).min(TICK_SECONDS);
        let scene = &world.scene;
        world.environment.tick(
            day.ticks.floor() as i64,
            0.0,
            0.0,
            eye,
            |x, y, z| scene.noise_biome((x, y, z)).map_or(0, |id| id.0),
            !*environment_primed,
        );
        *environment_primed = true;
    }
    let partial_tick = (*environment_accumulator / TICK_SECONDS).clamp(0.0, 1.0) as f32;
    let sky = world.environment.sky_state(&View {
        partial_tick,
        forward,
        camera_y: eye[1] as f32,
        render_distance: VIEW_DISTANCE as u32,
        rain_level: 0.0,
        thunder_level: 0.0,
    });
    let render_distance = VIEW_DISTANCE as f32 * 16.0 + remote_fog_depth;
    let right = forward.cross(glam::Vec3::Y).normalize_or(glam::Vec3::X);
    let up = right.cross(forward).normalize_or(glam::Vec3::Y);
    let put = |v: glam::Vec3| [v.x, v.y, v.z, 0.0];
    let game_time = day.ticks;
    view.environment = [
        put(forward),
        put(right),
        put(up),
        [eye[0] as f32, eye[1] as f32, eye[2] as f32, 0.0],
        put(sky.sky),
        [
            sky.fog.x,
            sky.fog.y,
            sky.fog.z,
            render_distance.min(sky.sky_fog_end),
        ],
        [
            sky.sky_light_color.x,
            sky.sky_light_color.y,
            sky.sky_light_color.z,
            sky.sky_light_factor,
        ],
        sky.sunset,
        [
            sky.sun_direction.x,
            sky.sun_direction.y,
            sky.sun_direction.z,
            sky.rain_brightness,
        ],
        [
            sky.moon_direction.x,
            sky.moon_direction.y,
            sky.moon_direction.z,
            sky.rain_brightness,
        ],
        // The brightness option at its default.
        [sky.cloud.x, sky.cloud.y, sky.cloud.z, 0.5],
        [aspect, 0.0, sky.star_brightness, sky.star_angle],
        [
            sky.moon_phase as f32,
            (game_time as f32) * 0.03,
            96.0,
            160.0,
        ],
        [
            sky.fog_start + remote_fog_depth,
            sky.fog_end + remote_fog_depth,
            render_distance - (render_distance / 10.0).clamp(4.0, 64.0),
            render_distance,
        ],
        [
            sky.ambient.x,
            sky.ambient.y,
            sky.ambient.z,
            match sky.skybox {
                Skybox::Overworld => 0.0,
                Skybox::End => 1.0,
                _ => 2.0,
            },
        ],
        [
            sky.block_light_tint.x,
            sky.block_light_tint.y,
            sky.block_light_tint.z,
            sky.block_factor,
        ],
    ];

    // Clouds, rebuilt when the camera crosses a cloud cell.
    if let Some(mask) = &world.cloud_mask {
        let center = mask.center(eye[0] as f32, eye[2] as f32, game_time);
        if *cloud_center != Some(center) {
            *cloud_center = Some(center);
            let mesh = mask.build(center, eye[1] as f32);
            view.clouds = Some(Arc::new((mesh.vertices, mesh.indices)));
        }
    }

    // The light MW2 models stand in, around the player.
    *light_volume_age += 1;
    let half = LIGHT_VOLUME / 2;
    let corner = [block.0 - half, block.1 - half, block.2 - half];
    let moved = light_volume_at.is_none_or(|at| (0..3).any(|k| (at[k] - corner[k]).abs() >= 4));
    if moved || *light_volume_age >= 20 {
        *light_volume_age = 0;
        *light_volume_at = Some(corner);
        let n = LIGHT_VOLUME;
        let index = |x: i32, y: i32, z: i32| (((y * n + z) * n + x) * 2) as usize;
        let mut raw = vec![0u8; (n * n * n * 2) as usize];
        for z in 0..n {
            for x in 0..n {
                let (bx, bz) = (corner[0] + x, corner[2] + z);
                // One column lookup for the whole stack, not two per cell.
                let column = light.chunk_column((bx >> 4, bz >> 4));
                for y in 0..n {
                    let pos = (bx, corner[1] + y, bz);
                    let at = index(x, y, z);
                    let (sky, block) = match column {
                        Some(column) => (column.get(pos), column.get_block(pos)),
                        None => (light.get(pos), light.get_block(pos)),
                    };
                    raw[at] = sky;
                    raw[at + 1] = block;
                }
            }
        }
        // Unlit cells (inside blocks) take their brightest neighbour, so a
        // model beside a block is not darkened by filtering into it. Levels
        // become unorm bytes.
        let mut data = vec![0u8; raw.len()];
        for y in 0..n {
            for z in 0..n {
                for x in 0..n {
                    let at = index(x, y, z);
                    let (mut sky, mut block) = (raw[at], raw[at + 1]);
                    if sky == 0 && block == 0 {
                        for (dx, dy, dz) in [
                            (1, 0, 0),
                            (-1, 0, 0),
                            (0, 1, 0),
                            (0, -1, 0),
                            (0, 0, 1),
                            (0, 0, -1),
                        ] {
                            let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                            if (0..n).contains(&nx) && (0..n).contains(&ny) && (0..n).contains(&nz)
                            {
                                let near = index(nx, ny, nz);
                                sky = sky.max(raw[near]);
                                block = block.max(raw[near + 1]);
                            }
                        }
                    }
                    data[at] = sky.min(15) * 17;
                    data[at + 1] = block.min(15) * 17;
                }
            }
        }
        view.light_volume = Some(Arc::new((corner, data)));
    }
    if authority.is_some() {
        let changes: Vec<_> = changed_blocks
            .drain()
            .map(|pos| {
                let state = minecraft_terrain::scene::Scene::block(&world.scene, pos)
                    .and_then(|block| world.stream.states.state_of(block))
                    .unwrap_or(BlockStateId::AIR);
                if let Some(old) = network_edits.insert(pos, u32::from(state.0)) {
                    *edits_hash ^= edit_fingerprint(pos, old);
                }
                *edits_hash ^= edit_fingerprint(pos, u32::from(state.0));
                ([pos.0, pos.1, pos.2], u32::from(state.0))
            })
            .collect();
        if !changes.is_empty() {
            *edit_revision += 1;
            network_log.push((*edit_revision, changes));
        }
        if let Some(hub) = hub.as_ref() {
            let clients = hub.committed_clients();
            let send_mobs = time.elapsed_secs_f64() - *last_mobs_sent >= 0.2;
            let mob_rows: Vec<Vec<net::MinecraftMobVisual>> = if send_mobs {
                let mobs = entities
                    .as_ref()
                    .map_or_else(Vec::new, |entities| entities.network_snapshot());
                *mob_count = mobs.len();
                if mobs.is_empty() {
                    vec![Vec::new()]
                } else {
                    mobs.chunks(16).take(8).map(|part| part.to_vec()).collect()
                }
            } else {
                Vec::new()
            };
            if send_mobs {
                *last_mobs_sent = time.elapsed_secs_f64();
                *mob_sequence = mob_sequence.wrapping_add(1).max(1);
            }
            sent_init.retain(|client| clients.contains(client));
            init_progress.retain(|client, _| clients.contains(client));
            live_progress.retain(|client, _| clients.contains(client));
            remote_stock.retain(|client, _| clients.contains(client));
            stock_dirty.retain(|client| clients.contains(client));
            for client in clients {
                let queue = reliable.queue_mut(client);
                if !sent_init.contains(&client) {
                    let init = init_progress.entry(client).or_insert_with(|| PeerInit {
                        edits: network_edits
                            .iter()
                            .map(|(&(x, y, z), &state)| ([x, y, z], state))
                            .collect(),
                        next: 0,
                        revision: *edit_revision,
                        began: false,
                        log_index: network_log.len(),
                    });
                    if !init.began && queue.pending_len() < 48 {
                        queue.push(net::ReliableRow::Minecraft(net::MinecraftMessage::Begin {
                            seed: world.seed,
                            origin,
                            revision: init.revision,
                            edits: init.edits.len() as u32,
                        }));
                        init.began = true;
                    }
                    while init.began && init.next < init.edits.len() && queue.pending_len() < 48 {
                        let end = (init.next + 32).min(init.edits.len());
                        queue.push(net::ReliableRow::Minecraft(net::MinecraftMessage::Blocks {
                            revision: init.revision,
                            changes: init.edits[init.next..end].to_vec(),
                        }));
                        init.next = end;
                    }
                    if init.began && init.next == init.edits.len() {
                        live_progress.insert(client, (init.log_index, 0));
                        sent_init.insert(client);
                        stock_dirty.insert(client);
                        init_progress.remove(&client);
                    }
                }
                if let Some((index, offset)) = live_progress.get_mut(&client) {
                    while *index < network_log.len() && queue.pending_len() < 48 {
                        let (revision, edits) = &network_log[*index];
                        let end = (*offset + 32).min(edits.len());
                        queue.push(net::ReliableRow::Minecraft(net::MinecraftMessage::Blocks {
                            revision: *revision,
                            changes: edits[*offset..end].to_vec(),
                        }));
                        *offset = end;
                        if *offset == edits.len() {
                            *index += 1;
                            *offset = 0;
                        }
                    }
                }
                if send_mobs
                    && sent_init.contains(&client)
                    && queue.pending_len() + mob_rows.len() < 48
                {
                    for (part, mobs) in mob_rows.iter().enumerate() {
                        queue.push(net::ReliableRow::Minecraft(net::MinecraftMessage::Mobs {
                            sequence: *mob_sequence,
                            part: part as u8,
                            total: mob_rows.len() as u8,
                            mobs: mobs.clone(),
                        }));
                    }
                }
                if sent_init.contains(&client)
                    && stock_dirty.contains(&client)
                    && queue.pending_len() < 48
                {
                    let mut entries: Vec<_> = remote_stock
                        .get(&client)
                        .into_iter()
                        .flat_map(|stock| stock.iter().map(|(&state, &count)| (state, count)))
                        .collect();
                    entries.sort_unstable();
                    queue.push(net::ReliableRow::Minecraft(net::MinecraftMessage::Stock {
                        entries,
                    }));
                    stock_dirty.remove(&client);
                }
            }
            let discard = init_progress
                .values()
                .map(|row| row.log_index)
                .chain(live_progress.values().map(|row| row.0))
                .min()
                .unwrap_or(network_log.len());
            if discard > 0 {
                network_log.drain(..discard);
                for row in init_progress.values_mut() {
                    row.log_index -= discard;
                }
                for row in live_progress.values_mut() {
                    row.0 -= discard;
                }
            }
        } else {
            network_log.clear();
        }
    }
    view.status = Some(MinecraftWorldStatus {
        seed: world.seed,
        origin,
        alive,
        health: ps.health,
        stream_ready: *was_alive,
        stream_center: stream_block,
        edit_revision: *edit_revision,
        edit_count: network_edits.len(),
        edit_fingerprint: *edits_hash,
        mob_sequence: *mob_sequence,
        mob_count: *mob_count,
    });
}

fn stop(runtime: &mut Runtime, view: &mut MinecraftWorldView) {
    runtime.network_origin = None;
    runtime.network_seed = None;
    runtime.network_edits_expected = None;
    runtime.network_edits.clear();
    runtime.network_initial_positions.clear();
    runtime.network_initial_revision = 0;
    runtime.changed_blocks.clear();
    runtime.edit_revision = 0;
    runtime.edit_fingerprint = 0;
    runtime.network_log.clear();
    runtime.remote_stock.clear();
    runtime.stock_dirty.clear();
    runtime.pending_stock = None;
    runtime.remote_mining_owner.clear();
    runtime.live_progress.clear();
    runtime.sent_init.clear();
    runtime.init_progress.clear();
    runtime.mob_parts = None;
    runtime.mob_sequence = 0;
    runtime.mob_count = 0;
    runtime.last_mobs_sent = 0.0;
    runtime.minimap = Default::default();
    runtime.retained_collision.clear();
    if runtime.world.take().is_some() || runtime.loading.take().is_some() || view.active {
        sim::voxel::deactivate();
        view.active = false;
        view.status = None;
        view.atlas = None;
        view.uploads.clear();
        view.removed.clear();
        view.visible.clear();
        view.celestial = None;
        view.crack_texture = None;
        view.particles = Default::default();
        view.entity_meshes = Default::default();
        view.hand = Default::default();
        view.cracks = Default::default();
        view.clouds = None;
        view.light_volume = None;
        view.generation += 1;
    }
}
