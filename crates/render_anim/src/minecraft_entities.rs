//! The Minecraft world's mobs, as MinecraftOSS runs them: its integrated
//! server (`engine/viewer/src/server.rs`) ticks the level at 20 Hz on its own
//! thread with natural spawning and the entity world, every passive and
//! hostile mob's AI and pathfinding. Here it is fed the streamed chunks and
//! the player, and hands back the tracked mobs, which are drawn with the
//! viewer's mob renderers. MW2 bullets hurt the mobs with their real damage
//! (a hundred MW2 health to Minecraft's twenty), and mobs hurt the player
//! the other way round.
use glam::{DVec3, Vec3};
use minecraft_terrain::client_mobs::{ClientMobs, server_mobs};
use minecraft_terrain::lighting::SkyLight;
use minecraft_terrain::mesh::ItemVisuals;
use minecraft_terrain::mesh::{Atlas, ChunkMesh};
use minecraft_terrain::pack::PackStack;
use minecraft_terrain::poof_particles::PoofParticles;
use minecraft_terrain::portal_particles::PortalParticles;
use minecraft_terrain::scene::{Block, HandcraftedScene, Scene};
use minecraft_terrain::server::{EntitySnapshot, ServerItem};
use minecraft_terrain::server::{PlayerEdit, ServerHandle, ServerSim, TickInput};
use minecraft_terrain::terrain::TerrainStream;
use minecraftoss_entities::tempt::PlayerCandidate;
use minecraftoss_entities::world::{EntityWorld, MobHit, PlayerHitKind};
use minecraftoss_player::inventory::{Inventory, ItemStack};
use minecraftoss_player::items::{ItemEntity, WorldItems};
use minecraftoss_player::loot::LootBook;
use minecraftoss_player::rng::XoroshiroRandom;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

const TICK_SECONDS: f64 = 1.0 / 20.0;
/// Minecraft health per MW2 health.
const HEALTH_SCALE: f32 = 20.0 / 100.0;
/// The player's id in the entity world.
const PLAYER: u64 = 0;
/// `Player.getEyeHeight` standing.
const EYE_HEIGHT: f32 = 1.62;

pub(crate) struct Entities {
    server: Option<ServerHandle>,
    remote_mobs: HashMap<u64, RemoteMob>,
    /// The mobs the player tracks, as of the last server tick.
    world: EntityWorld,
    /// The client's copies of them: tracked, interpolated, animated.
    client: ClientMobs,
    poof: PoofParticles,
    portal: PortalParticles,
    items: ItemVisuals,
    clock: f64,
    ticks: u64,
    /// The player's inventory: vanilla slots, stacking and recipes.
    pub(crate) inventory: Inventory,
    /// The selected hotbar slot.
    pub(crate) selected: usize,
    /// Item entities as the client shows them: the server's, and drops the
    /// client spawned that the server has not taken yet.
    pub(crate) world_items: WorldItems,
    server_item_ids: HashSet<u32>,
    /// Client drops handed to the server: the command count that sent each.
    server_handed: HashMap<u32, (u64, ItemEntity)>,
    /// Stacks the server let the player pick up, not yet in the inventory.
    server_picked: Vec<(i32, [f64; 3], String, i32, Option<String>)>,
    server_snapshot: Option<EntitySnapshot>,
    server_handled: u64,
    loot: Option<LootBook>,
    loot_sequences: HashMap<String, XoroshiroRandom>,
    seed: i64,
    /// Sounds the mob world made since the last take: event, block point,
    /// volume, pitch.
    pub(crate) sounds: Vec<(String, DVec3, f32, f32)>,
    random: minecraftoss_player::rng::LegacyRandom,
    attacks: HashMap<u64, sim::voxel::MobAttackCredit>,
    next_attack: u64,
    feedback: Vec<(sim::voxel::MobAttackCredit, u32)>,
}

struct RemoteMob {
    visual: net::MinecraftMobVisual,
    from: [f32; 3],
    received: Instant,
}

impl RemoteMob {
    fn position(&self) -> [f32; 3] {
        let blend = (self.received.elapsed().as_secs_f32() * 5.0).min(1.0);
        std::array::from_fn(|axis| {
            self.from[axis] + (self.visual.position[axis] - self.from[axis]) * blend
        })
    }
}

const LOOT_TOOL: &str = "minecraft:diamond_pickaxe";

/// The mobs drawn this frame: entity models (cut out, back-face culled,
/// translucent) and their shadows, in `mesh::Vertex`s; and what goes with
/// the particles (held items, puffs, flames, potions).
#[derive(Default)]
pub(crate) struct MobMeshes {
    pub models: ChunkMesh,
    pub culled: ChunkMesh,
    pub translucent: ChunkMesh,
    pub shadows: ChunkMesh,
    pub items: ChunkMesh,
}

/// The player as the mobs see it this frame.
pub(crate) struct PlayerView {
    pub id: u64,
    pub feet: [f64; 3],
    pub alive: bool,
    /// MW2 health, 0..100.
    pub health: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl Entities {
    pub(crate) fn new(stream: &TerrainStream, seed: i64) -> Self {
        let sim = ServerSim::new(
            stream.world_gen(),
            stream.states.clone(),
            "minecraft:overworld",
        );
        let mut server = ServerHandle::spawn(sim);
        // Mob and block loot and the recipes (stack sizes), from the game's
        // data JAR when MinecraftOSS has one.
        let jar = data_jar();
        let mut inventory = Inventory::default();
        let mut loot = None;
        if let Some(jar) = jar {
            server.load_loot(jar.clone(), seed as u64);
            match minecraftoss_player::crafting::RecipeBook::from_jar(&jar) {
                Ok(recipes) => {
                    let recipes = std::sync::Arc::new(recipes);
                    server.set_recipe_book(recipes.clone());
                    inventory.recipes = recipes;
                }
                Err(error) => diag::warn!(World, "Minecraft recipes unavailable: {error:#}"),
            }
            match LootBook::from_jar(&jar) {
                Ok(book) => loot = Some(book),
                Err(error) => diag::warn!(World, "Minecraft block loot unavailable: {error:#}"),
            }
        } else {
            diag::warn!(World, "Minecraft data JAR not found: no loot or recipes");
        }
        Self::with_server(Some(server), seed, inventory, loot)
    }

    pub(crate) fn new_remote(seed: i64) -> Self {
        Self::with_server(None, seed, Inventory::default(), None)
    }

    fn with_server(
        server: Option<ServerHandle>,
        seed: i64,
        inventory: Inventory,
        loot: Option<LootBook>,
    ) -> Self {
        Self {
            server,
            remote_mobs: HashMap::new(),
            world: EntityWorld::default(),
            client: ClientMobs::default(),
            poof: PoofParticles::default(),
            portal: PortalParticles::default(),
            items: ItemVisuals::default(),
            clock: 0.0,
            ticks: 0,
            inventory,
            selected: 0,
            world_items: WorldItems::default(),
            server_item_ids: HashSet::new(),
            server_handed: HashMap::new(),
            server_picked: Vec::new(),
            server_snapshot: None,
            server_handled: 0,
            loot,
            loot_sequences: HashMap::new(),
            seed,
            sounds: Vec::new(),
            random: minecraftoss_player::rng::LegacyRandom::new((seed ^ 0x1735) as u64),
            attacks: HashMap::new(),
            next_attack: 0,
            feedback: Vec::new(),
        }
    }

    pub(crate) fn load_chunk(&mut self, chunk: &std::sync::Arc<minecraftoss_core::Chunk>) {
        if let Some(server) = &mut self.server {
            server.load_chunk(chunk);
        }
    }

    pub(crate) fn network_snapshot(&self) -> Vec<net::MinecraftMobVisual> {
        let w = &self.world;
        let mut kinds = HashMap::new();
        for e in w.cows() {
            kinds.insert(e.id, 0);
        }
        for e in w.sheep() {
            kinds.insert(e.id, 1);
        }
        for e in w.pigs() {
            kinds.insert(e.id, 2);
        }
        for e in w.chickens() {
            kinds.insert(e.id, 3);
        }
        for e in w.bats() {
            kinds.insert(e.id, 4);
        }
        for e in w.zombies() {
            kinds.insert(e.id, 5);
        }
        for e in w.skeletons() {
            kinds.insert(e.id, 6);
        }
        for e in w.spiders() {
            kinds.insert(e.id, 7);
        }
        for e in w.creepers() {
            kinds.insert(e.id, 8);
        }
        for e in w.villagers() {
            kinds.insert(e.id, 9);
        }
        for e in w.endermen() {
            kinds.insert(e.id, 10);
        }
        for e in w.iron_golems() {
            kinds.insert(e.id, 11);
        }
        for e in w.slimes() {
            kinds.insert(e.id, 12);
        }
        for e in w.witches() {
            kinds.insert(e.id, 13);
        }
        for e in w.wolves() {
            kinds.insert(e.id, 14);
        }
        let mut mobs: Vec<_> = server_mobs(w)
            .into_iter()
            .filter_map(|mob| {
                let kind = *kinds.get(&mob.id)?;
                let position = mob.position.to_array().map(|v| v as f32);
                if !position.into_iter().all(f32::is_finite)
                    || ![mob.y_rot, mob.x_rot, mob.y_head_rot]
                        .into_iter()
                        .all(f32::is_finite)
                {
                    return None;
                }
                Some(net::MinecraftMobVisual {
                    id: mob.id,
                    kind,
                    position,
                    yaw: mob.y_rot,
                    pitch: mob.x_rot,
                    head_yaw: mob.y_head_rot,
                    flags: u8::from(mob.baby)
                        | (u8::from(mob.on_ground) << 1)
                        | (u8::from(mob.resting) << 2)
                        | (u8::from(mob.dead) << 3),
                })
            })
            .collect();
        mobs.sort_by_key(|mob| mob.id);
        mobs.truncate(128);
        mobs
    }

    pub(crate) fn apply_network_snapshot(&mut self, snapshot: &[net::MinecraftMobVisual]) {
        if self.server.is_some() {
            return;
        }
        let now = Instant::now();
        let mut seen = HashSet::new();
        for visual in snapshot.iter().take(128) {
            if visual.kind > 14
                || !visual.position.into_iter().all(f32::is_finite)
                || ![visual.yaw, visual.pitch, visual.head_yaw]
                    .into_iter()
                    .all(f32::is_finite)
            {
                continue;
            }
            seen.insert(visual.id);
            self.remote_mobs
                .entry(visual.id)
                .and_modify(|mob| {
                    mob.from = mob.position();
                    mob.visual = visual.clone();
                    mob.received = now;
                })
                .or_insert_with(|| RemoteMob {
                    visual: visual.clone(),
                    from: visual.position,
                    received: now,
                });
        }
        self.remote_mobs.retain(|id, _| seen.contains(id));
    }

    pub(crate) fn unload_chunk(&mut self, pos: minecraftoss_core::ChunkPos) {
        if let Some(server) = &mut self.server {
            server.unload_chunk(pos);
        }
    }

    pub(crate) fn drop_blocks(&mut self, broken: &[((i32, i32, i32), Block, bool, bool)]) {
        let Some(loot) = self.loot.as_ref() else {
            return;
        };
        let tool = ItemStack::new(LOOT_TOOL, 1);
        for (pos, block, _, drop_items) in broken {
            if !drop_items {
                continue;
            }
            let block = minecraftoss_player::Block {
                id: block.id.key(),
                properties: block.properties.clone(),
            };
            let Some(drops) = loot.roll_drops_named(
                &block,
                Some(&tool),
                self.seed as u64,
                &mut self.loot_sequences,
            ) else {
                continue;
            };
            for mut drop in drops {
                if drop.components.is_none() {
                    drop.max = drop.max.min(self.inventory.recipes.max_stack(&drop.id));
                }
                self.world_items.spawn_block_drop(drop, *pos);
            }
        }
    }

    /// `ItemEntity`s to draw and pick up, as the viewer's
    /// `server_items_tick` does for a server-simulated world: client drops
    /// go to the server, stacks the server offered go into the inventory,
    /// and the server's items are mirrored for drawing.
    fn server_items_tick(&mut self, feet: DVec3) {
        let server = self.server.as_mut().expect("host Minecraft entities");
        let entities = std::mem::take(&mut self.world_items.entities);
        let to_hand: Vec<ItemEntity> = entities
            .iter()
            .filter(|e| {
                !self.server_item_ids.contains(&e.entity_id)
                    && !self.server_handed.contains_key(&e.entity_id)
            })
            .cloned()
            .collect();
        for entity in &to_hand {
            let components = entity.stack.components.as_ref().map(|c| c.to_string());
            server.spawn_item(
                &entity.stack.id,
                i32::from(entity.stack.count),
                components.as_deref(),
                entity.position.to_array(),
                entity.velocity.to_array(),
                i32::from(entity.pickup_delay),
                entity.age as i32,
            );
            self.server_handed
                .insert(entity.entity_id, (server.sent(), entity.clone()));
        }
        let previous: HashMap<u32, ItemEntity> =
            entities.into_iter().map(|e| (e.entity_id, e)).collect();
        let target = feet + DVec3::Y * 0.81;
        self.world_items.tick_pickup_effects(target);
        for (id, position, item, count, components) in std::mem::take(&mut self.server_picked) {
            let recipes = self.inventory.recipes.clone();
            let make_stack = |item: &str, count: i32| {
                let mut stack = ItemStack::new(item, count.clamp(0, 255) as u8);
                stack.components = components
                    .as_deref()
                    .and_then(|c| serde_json::from_str(c).ok());
                if stack.components.is_none() {
                    stack.max = stack.max.min(recipes.max_stack(item));
                }
                stack
            };
            let taken = match self
                .inventory
                .add_item(make_stack(&item, count), self.selected)
            {
                None => count,
                Some(rest) => {
                    let rest_count = i32::from(rest.count);
                    server.spawn_item(
                        &item,
                        rest_count,
                        components.as_deref(),
                        feet.to_array(),
                        [0.0; 3],
                        0,
                        0,
                    );
                    count - rest_count
                }
            };
            if taken <= 0 {
                continue;
            }
            let transfer = make_stack(&item, taken);
            let snapshot = previous
                .get(&(id as u32))
                .cloned()
                .unwrap_or_else(|| ItemEntity {
                    entity_id: id as u32,
                    stack: transfer.clone(),
                    position: DVec3::from_array(position),
                    previous_position: DVec3::from_array(position),
                    velocity: DVec3::ZERO,
                    age: 0,
                    bob_offset: bob_offset(id),
                    pickup_delay: 0,
                    on_ground: true,
                });
            self.world_items.note_pickup(snapshot, target, transfer);
        }
        let Some(snapshot) = self.server_snapshot.take() else {
            self.world_items.entities = previous.into_values().collect();
            self.world_items.entities.sort_by_key(|e| e.entity_id);
            return;
        };
        let recipes = self.inventory.recipes.clone();
        self.world_items.entities = snapshot
            .items
            .into_iter()
            .map(|item: ServerItem| {
                let mut stack = ItemStack::new(&item.item, item.count.clamp(0, 255) as u8);
                stack.components = item
                    .components
                    .as_deref()
                    .and_then(|c| serde_json::from_str(c).ok());
                if stack.components.is_none() {
                    stack.max = stack.max.min(recipes.max_stack(&item.item));
                }
                ItemEntity {
                    entity_id: item.id as u32,
                    stack,
                    position: DVec3::from_array(item.position),
                    previous_position: DVec3::from_array(item.previous_position),
                    velocity: DVec3::from_array(item.velocity),
                    age: item.age.max(0) as u32,
                    bob_offset: bob_offset(item.id),
                    pickup_delay: item.pickup_delay.clamp(0, i32::from(u16::MAX)) as u16,
                    on_ground: item.on_ground,
                }
            })
            .collect();
        self.server_item_ids = self
            .world_items
            .entities
            .iter()
            .map(|e| e.entity_id)
            .collect();
        let handled = self.server_handled;
        self.server_handed.retain(|_, (sent, _)| *sent > handled);
        for (_, entity) in self.server_handed.values() {
            self.world_items.entities.push(entity.clone());
        }
    }

    /// A held item's model under a view-space pose, lit at `light_at`.
    pub(crate) fn held_item_mesh(
        &mut self,
        id: &str,
        pose: glam::Mat4,
        light_at: glam::Vec3,
        packs: &PackStack,
        atlas: &Atlas,
        light: &SkyLight,
    ) -> ChunkMesh {
        let mut mesh = ChunkMesh::default();
        let _ = self.items.append_posed_blocks(
            &mut mesh,
            &[(pose, light_at, id.to_owned())],
            packs,
            atlas,
            light,
        );
        mesh
    }

    /// A fist on the mob the player looks at within reach (one damage, as
    /// an empty hand deals).
    pub(crate) fn punch(
        &mut self,
        eye: DVec3,
        look: DVec3,
        yaw: f32,
        credit: sim::voxel::MobAttackCredit,
    ) -> bool {
        let Some(server) = &mut self.server else {
            return false;
        };
        let Some((hit, _)) = self.world.mob_on_ray(eye, look, 3.0) else {
            return false;
        };
        let attack = minecraftoss_entities::world::PlayerAttack {
            player_id: PLAYER,
            position: eye - DVec3::Y * f64::from(EYE_HEIGHT),
            yaw,
            attack_damage: 1.0,
            strength: 1.0,
            sprinting: false,
            can_critical: false,
            can_sweep: false,
        };
        self.next_attack = self.next_attack.wrapping_add(1);
        self.attacks.insert(self.next_attack, credit);
        server.credited_mob_attack(
            self.next_attack,
            hit,
            attack,
            &self.inventory.clone(),
            self.selected,
        );
        true
    }

    /// A block the player placed, for the level the mobs walk in.
    pub(crate) fn placed(&mut self, scene: &HandcraftedScene, pos: (i32, i32, i32)) {
        if let Some(server) = &mut self.server {
            server.player_edit(scene, pos, PlayerEdit::Place);
        }
    }

    /// Client ticks run so far.
    pub(crate) fn client_ticks(&self) -> u64 {
        self.ticks
    }

    /// Blocks the player's weapons broke, for the level the mobs walk in.
    pub(crate) fn broke(
        &mut self,
        scene: &HandcraftedScene,
        broken: &[((i32, i32, i32), Block, bool, bool)],
    ) {
        let Some(server) = &mut self.server else {
            return;
        };
        let mut without_drops = Vec::new();
        for (pos, _, _, drop_items) in broken {
            if *drop_items {
                server.player_edit(scene, *pos, PlayerEdit::Break);
            } else {
                without_drops.push(*pos);
            }
        }
        if !without_drops.is_empty()
            && let Some(states) = scene.states()
        {
            let positions: HashSet<_> = without_drops.iter().copied().collect();
            let keep = |entity: &ItemEntity| {
                if states
                    .registries()
                    .blocks
                    .parse_state(&entity.stack.id)
                    .is_err()
                {
                    return true;
                }
                let [x, y, z] = entity.position.to_array().map(|v| v.floor() as i32);
                !(-1..=1).any(|dx| {
                    (-1..=1)
                        .any(|dy| (-1..=1).any(|dz| positions.contains(&(x + dx, y + dy, z + dz))))
                })
            };
            self.world_items.entities.retain(&keep);
            self.server_handed.retain(|_, (_, entity)| keep(entity));
        }
        server.destroy_blocks_without_drops(without_drops);
    }

    /// A bullet on a mob: an attack with the bullet's damage from where it
    /// was fired.
    pub(crate) fn shoot(
        &mut self,
        key: u64,
        damage: f32,
        from: [f64; 3],
        yaw: f32,
        credit: Option<sim::voxel::MobAttackCredit>,
    ) {
        let Some(server) = &mut self.server else {
            return;
        };
        let Some(hit) = decode(key) else {
            return;
        };
        let attack = minecraftoss_entities::world::PlayerAttack {
            player_id: PLAYER,
            position: DVec3::from_array(from),
            yaw,
            attack_damage: f64::from(damage * HEALTH_SCALE),
            strength: 1.0,
            sprinting: false,
            can_critical: false,
            can_sweep: false,
        };
        if let Some(credit) = credit {
            self.next_attack = self.next_attack.wrapping_add(1);
            self.attacks.insert(self.next_attack, credit);
            server.credited_mob_attack(self.next_attack, hit, attack, &Inventory::default(), 0);
        } else {
            server.mob_action(hit, Some(attack), &Inventory::default(), 0, false);
        }
    }

    pub(crate) fn take_feedback(&mut self) -> Vec<(sim::voxel::MobAttackCredit, u32)> {
        std::mem::take(&mut self.feedback)
    }

    /// Sends a server tick when one is due and takes what came back: block
    /// changes to show, and mob hits on the player in MW2 damage.
    pub(crate) fn tick(
        &mut self,
        dt: f64,
        day_ticks: i64,
        bright_outside: bool,
        player: &PlayerView,
        peers: &[PlayerView],
    ) -> (
        Vec<((i32, i32, i32), Option<Block>)>,
        Vec<(u64, i32, Option<[f64; 3]>)>,
    ) {
        self.clock += dt;
        let ticked = self.clock >= TICK_SECONDS;
        if ticked {
            self.clock = (self.clock - TICK_SECONDS).min(TICK_SECONDS);
            self.ticks += 1;
            // Packets first, then the client level's entity ticks.
            self.client.tick();
            for enderman in self.world.endermen() {
                if let Some(mob) = self.client.get(enderman.id) {
                    self.portal.emit_enderman(mob.position, 0.6, 2.9);
                }
            }
            self.portal.tick();
            if self.ticks % 600 == 0 {
                diag::info!(World, "Minecraft mobs: {} tracked", self.boxes().len());
            }
            let feet = DVec3::from_array(player.feet);
            let center = ((feet.x.floor() as i32) >> 4, (feet.z.floor() as i32) >> 4);
            let candidate = |player: &PlayerView| PlayerCandidate {
                id: player.id,
                position: DVec3::from_array(player.feet),
                eye_height: EYE_HEIGHT,
                main_hand_cow_food: false,
                offhand_cow_food: false,
                main_hand_pig_food: false,
                offhand_pig_food: false,
                main_hand_chicken_food: false,
                offhand_chicken_food: false,
                main_hand_carrot_on_a_stick: false,
                offhand_carrot_on_a_stick: false,
                main_hand_wolf_interest: false,
                offhand_wolf_interest: false,
                main_hand_horse_tempt: false,
                offhand_horse_tempt: false,
                alive: player.alive,
                spectator: !player.alive,
                attackable: player.alive,
            };
            let all_players = std::iter::once(player).chain(peers.iter());
            self.server
                .as_mut()
                .expect("host Minecraft entities")
                .tick(TickInput {
                    day_ticks,
                    players: all_players
                        .clone()
                        .filter(|p| p.alive)
                        .map(|p| p.feet)
                        .collect(),
                    difficulty: 2,
                    simulation_center: center,
                    simulation_distance: 8,
                    pickup: player
                        .alive
                        .then(|| (player.feet, Box::new(self.inventory.clone()), self.selected)),
                    mob_players: all_players.clone().map(candidate).collect(),
                    mob_views: all_players
                        .clone()
                        .map(|p| {
                            (
                                p.id,
                                minecraftoss_entities::enderman::PlayerView {
                                    head_yaw: p.yaw,
                                    pitch: p.pitch,
                                    disguised: false,
                                },
                            )
                        })
                        .collect(),
                    mob_vitals: all_players
                        .map(|p| {
                            (
                                p.id,
                                minecraftoss_entities::monster_ai::PlayerVitals {
                                    health: p.health * HEALTH_SCALE,
                                    ..Default::default()
                                },
                            )
                        })
                        .collect(),
                    bright_outside,
                    tracking: (player.feet, 160.0),
                    player_hurts: Vec::new(),
                    spawn_mobs: true,
                });
        }
        if self.server.is_none() {
            return (Vec::new(), Vec::new());
        }
        let mut changes = Vec::new();
        let mut hits = Vec::new();
        for output in self
            .server
            .as_mut()
            .expect("host Minecraft entities")
            .poll()
        {
            self.server_handled = output.handled;
            if let Some(snapshot) = output.entities {
                self.server_snapshot = Some(snapshot);
            }
            self.server_picked.extend(output.picked);
            changes.extend(output.changes);
            for sound in &output.mob_sounds {
                self.sounds.push((
                    sound.event.clone(),
                    sound.position,
                    sound.volume,
                    sound.pitch,
                ));
            }
            for sound in output
                .mob_results
                .iter()
                .flat_map(|result| result.sounds.iter())
            {
                self.sounds.push((
                    sound.event.clone(),
                    sound.position,
                    sound.volume,
                    sound.pitch,
                ));
            }
            for result in &output.mob_results {
                if let Some(credit) = result
                    .request
                    .and_then(|request| self.attacks.remove(&request))
                {
                    if result.hurt {
                        self.feedback.push((credit, result.kills));
                    }
                }
            }
            // `ServerExplosion`'s sound: loud, pitched down.
            for blast in &output.explosions {
                let pitch =
                    (1.0 + (self.random.next_float() - self.random.next_float()) * 0.2) * 0.7;
                self.sounds.push((
                    "minecraft:entity.generic.explode".to_owned(),
                    blast.position,
                    4.0,
                    pitch,
                ));
            }
            if let Some(mobs) = output.mobs {
                self.world = *mobs;
                for (feet, width, height) in self.client.receive(server_mobs(&self.world)) {
                    self.poof.spawn(feet, width, height);
                }
            }
            for hit in output.player_hits {
                let from = match hit.kind {
                    PlayerHitKind::Melee { attacker, .. } => Some(attacker.to_array()),
                    _ => None,
                };
                hits.push((
                    hit.player_id,
                    (hit.damage / HEALTH_SCALE).round() as i32,
                    from,
                ));
            }
        }
        if ticked {
            self.server_items_tick(DVec3::from_array(player.feet));
            // `ItemEntity.playerTouch`'s pickup pop.
            for _ in 0..self.world_items.take_pickup_sounds() {
                let pitch =
                    ((self.random.next_float() - self.random.next_float()) * 0.7 + 1.0) * 2.0;
                self.sounds.push((
                    "minecraft:entity.item.pickup".to_owned(),
                    DVec3::from_array(player.feet),
                    0.2,
                    pitch,
                ));
            }
        }
        (changes, hits)
    }

    /// Every living mob's key and box in blocks, for bullets.
    pub(crate) fn boxes(&self) -> Vec<(u64, [f64; 6])> {
        if self.server.is_none() {
            return self
                .remote_mobs
                .values()
                .filter_map(|mob| {
                    let hit = hit_of(mob.visual.kind, mob.visual.id)?;
                    let p = mob.position().map(f64::from);
                    let (mut width, mut height) = mob_size(mob.visual.kind);
                    if mob.visual.flags & 1 != 0 {
                        width *= 0.5;
                        height *= 0.5;
                    }
                    let half = width * 0.5;
                    Some((
                        encode(hit),
                        [
                            p[0] - half,
                            p[1],
                            p[2] - half,
                            p[0] + half,
                            p[1] + height,
                            p[2] + half,
                        ],
                    ))
                })
                .collect();
        }
        let w = &self.world;
        let mut out = Vec::new();
        let mut add = |hit: MobHit, body: &minecraftoss_entities::movement::Body| {
            let p = body.position;
            let half = f64::from(body.width) * 0.5;
            out.push((
                encode(hit),
                [
                    p.x - half,
                    p.y,
                    p.z - half,
                    p.x + half,
                    p.y + f64::from(body.height),
                    p.z + half,
                ],
            ));
        };
        for e in w.bats().iter().filter(|e| e.bat.health > 0.0) {
            add(MobHit::Bat(e.id), &e.bat.body);
        }
        for e in w.zombies().iter().filter(|e| e.zombie.health > 0.0) {
            add(MobHit::Zombie(e.id), &e.zombie.body);
        }
        for e in w.skeletons().iter().filter(|e| e.skeleton.health > 0.0) {
            add(MobHit::Skeleton(e.id), &e.skeleton.body);
        }
        for e in w
            .creepers()
            .iter()
            .filter(|e| e.creeper.health > 0.0 && !e.creeper.exploded)
        {
            add(MobHit::Creeper(e.id), &e.creeper.body);
        }
        for e in w.spiders().iter().filter(|e| e.spider.health > 0.0) {
            add(MobHit::Spider(e.id), &e.spider.body);
        }
        for e in w.slimes().iter().filter(|e| e.slime.health > 0.0) {
            add(MobHit::Slime(e.id), &e.slime.body);
        }
        for e in w.endermen().iter().filter(|e| e.enderman.health > 0.0) {
            add(MobHit::Enderman(e.id), &e.enderman.body);
        }
        for e in w.witches().iter().filter(|e| e.witch.health > 0.0) {
            add(MobHit::Witch(e.id), &e.witch.body);
        }
        for e in w.iron_golems().iter().filter(|e| e.golem.health > 0.0) {
            add(MobHit::IronGolem(e.id), &e.golem.body);
        }
        for e in w.wolves().iter().filter(|e| e.wolf.health > 0.0) {
            add(MobHit::Wolf(e.id), &e.wolf.body);
        }
        for e in w.villagers().iter().filter(|e| e.villager.health > 0.0) {
            add(MobHit::Villager(e.id), &e.villager.body);
        }
        for e in w.cows().iter().filter(|e| e.cow.health > 0.0) {
            let hit = if e.mooshroom.is_some() {
                MobHit::Mooshroom(e.id)
            } else {
                MobHit::Cow(e.id)
            };
            add(hit, &e.cow.body);
        }
        for e in w.sheep().iter().filter(|e| e.health > 0.0) {
            add(MobHit::Sheep(e.id), &e.body);
        }
        for e in w.pigs().iter().filter(|e| e.pig.health > 0.0) {
            add(MobHit::Pig(e.id), &e.pig.body);
        }
        for e in w.chickens().iter().filter(|e| e.chicken.health > 0.0) {
            add(MobHit::Chicken(e.id), &e.chicken.body);
        }
        out
    }

    /// Steps the client-side puffs, which settle on the scene's blocks.
    pub(crate) fn tick_scene(&mut self, scene: &HandcraftedScene, ticks: u32) {
        for _ in 0..ticks {
            self.poof.tick(scene);
        }
    }

    /// The mobs this frame, drawn with the viewer's renderers.
    pub(crate) fn meshes(
        &mut self,
        scene: &HandcraftedScene,
        packs: &PackStack,
        atlas: &Atlas,
        light: &SkyLight,
        forward: Vec3,
        camera: DVec3,
        sky_darken: u8,
    ) -> MobMeshes {
        use minecraft_terrain::*;
        if self.server.is_none() {
            let mut out = MobMeshes::default();
            let texture = pack::ResourceId::parse("minecraft:block/white_concrete").unwrap();
            let region = atlas.region(&texture);
            for mob in self.remote_mobs.values() {
                let (mut width, mut height) = mob_size(mob.visual.kind);
                if mob.visual.flags & 1 != 0 {
                    width *= 0.5;
                    height *= 0.5;
                }
                let position = mob.position();
                append_remote_mob(
                    &mut out.models,
                    position,
                    width as f32,
                    height as f32,
                    region,
                    mob.visual.kind,
                );
            }
            return out;
        }
        // Mobs appear on the client once their trackers start.
        self.client.spawn_missing(&server_mobs(&self.world));
        let w = &self.world;
        let poses = &self.client;
        let partial = (self.clock / TICK_SECONDS).clamp(0.0, 1.0) as f32;
        let mut out = MobMeshes::default();
        cow_render::append_cows(
            &mut out.models,
            w.cows().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        sheep_render::append_sheep(
            &mut out.models,
            w.sheep().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        pig_render::append_pigs(
            &mut out.models,
            w.pigs().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        chicken_render::append_chickens(
            &mut out.models,
            w.chickens().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        bat_render::append_bats(
            &mut out.culled,
            w.bats().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        let zombie_items = zombie_render::append_zombies(
            &mut out.models,
            w.zombies().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        creeper_render::append_creepers(
            &mut out.models,
            w.creepers().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        spider_render::append_spiders(
            &mut out.models,
            w.spiders().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        let skeleton_items = skeleton_render::append_skeletons(
            &mut out.models,
            w.skeletons().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        let villager_items = villager_render::append_villagers(
            &mut out.models,
            w.villagers().iter(),
            poses,
            &|pos| Scene::block(scene, pos).and_then(|b| b.properties.get("facing").cloned()),
            atlas,
            light,
            partial,
        );
        horse_render::append_horses(
            &mut out.models,
            &mut out.translucent,
            w.cows().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        slime_render::append_slimes(
            &mut out.models,
            &mut out.translucent,
            w.slimes().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        let carried = enderman_render::append_endermen(
            &mut out.models,
            w.endermen().iter(),
            poses,
            atlas,
            light,
            partial,
            self.ticks.rotate_left(32) ^ (partial.to_bits() as u64),
        );
        let witch_items = witch_render::append_witches(
            &mut out.models,
            w.witches().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        let poppies = golem_render::append_iron_golems(
            &mut out.models,
            w.iron_golems().iter(),
            poses,
            atlas,
            light,
            partial,
        );
        wolf_render::append_wolves(
            &mut out.models,
            w.wolves().iter(),
            poses,
            atlas,
            light,
            partial,
            w.game_time(),
        );
        flame_render::append_flames(
            &mut out.items,
            w.burning()
                .into_iter()
                .map(|(previous, now, width, height)| {
                    (previous.lerp(now, f64::from(partial)), width, height)
                }),
            atlas,
            forward,
            light,
        );
        witch_render::append_potions(
            &mut out.items,
            w.potions().iter().filter(|p| p.potion.alive).map(|p| {
                (
                    p,
                    p.previous_position
                        .lerp(p.potion.position, f64::from(partial)),
                )
            }),
            atlas,
            forward,
            light,
        );
        if let Ok(items) = self
            .items
            .mesh(&self.world_items, packs, atlas, light, partial)
        {
            let base = out.items.vertices.len() as u32;
            out.items.vertices.extend(items.vertices);
            out.items
                .indices
                .extend(items.indices.iter().map(|i| i + base));
        }
        self.poof
            .append_mesh(&mut out.items, atlas, forward, partial, light);
        self.portal
            .append_mesh(&mut out.items, atlas, forward, partial, light);
        let held: Vec<_> = skeleton_items
            .into_iter()
            .chain(zombie_items)
            .chain(villager_items)
            .chain(witch_items)
            .collect();
        let _ = self
            .items
            .append_held_items(&mut out.items, &held, packs, atlas, light);
        let carried: Vec<_> = carried.into_iter().chain(poppies).collect();
        let _ = self
            .items
            .append_posed_blocks(&mut out.items, &carried, packs, atlas, light);
        // Their shadows, at `getMaxLocalRawBrightness`.
        let casters = client_mobs::shadow_casters(w, poses, camera, partial);
        let raw = |pos: (i32, i32, i32)| {
            light
                .get(pos)
                .saturating_sub(sky_darken)
                .max(light.get_block(pos))
        };
        if let Ok(shadows) = mesh::entity_shadows(&casters, scene, atlas, &raw, 0.0) {
            out.shadows = shadows;
        }
        out
    }
}

/// The viewer's `server_bob_offset`: a server item's bob phase from its id.
fn bob_offset(id: i32) -> f32 {
    let hash = (id as u32)
        .wrapping_mul(0x9e37_79b9)
        .rotate_left(13)
        .wrapping_mul(0x85eb_ca6b);
    hash as f32 / u32::MAX as f32 * std::f32::consts::TAU
}

fn mob_size(kind: u8) -> (f64, f64) {
    match kind {
        3 | 4 => (0.5, 0.7),
        7 => (1.4, 0.9),
        10 => (0.6, 2.9),
        11 => (1.4, 2.7),
        12 => (0.9, 0.9),
        14 => (0.6, 0.85),
        _ => (0.7, 1.7),
    }
}

fn hit_of(kind: u8, id: u64) -> Option<MobHit> {
    Some(match kind {
        0 => MobHit::Cow(id),
        1 => MobHit::Sheep(id),
        2 => MobHit::Pig(id),
        3 => MobHit::Chicken(id),
        4 => MobHit::Bat(id),
        5 => MobHit::Zombie(id),
        6 => MobHit::Skeleton(id),
        7 => MobHit::Spider(id),
        8 => MobHit::Creeper(id),
        9 => MobHit::Villager(id),
        10 => MobHit::Enderman(id),
        11 => MobHit::IronGolem(id),
        12 => MobHit::Slime(id),
        13 => MobHit::Witch(id),
        14 => MobHit::Wolf(id),
        _ => return None,
    })
}

fn append_remote_mob(
    mesh: &mut ChunkMesh,
    feet: [f32; 3],
    width: f32,
    height: f32,
    region: [f32; 4],
    kind: u8,
) {
    let palette = [
        [0.45, 0.28, 0.18],
        [0.9, 0.9, 0.85],
        [0.95, 0.55, 0.65],
        [0.9, 0.8, 0.3],
        [0.35, 0.32, 0.3],
        [0.3, 0.65, 0.25],
        [0.8, 0.8, 0.75],
        [0.25, 0.25, 0.28],
        [0.28, 0.65, 0.35],
        [0.55, 0.38, 0.25],
        [0.18, 0.16, 0.22],
        [0.75, 0.68, 0.61],
        [0.43, 0.8, 0.43],
        [0.42, 0.25, 0.48],
        [0.6, 0.6, 0.58],
    ];
    let tint = palette.get(kind as usize).copied().unwrap_or([1.0; 3]);
    let [x, y, z] = feet;
    let half = width * 0.5;
    let lo = [x - half, y, z - half];
    let hi = [x + half, y + height, z + half];
    let faces = [
        [
            [lo[0], lo[1], hi[2]],
            [hi[0], lo[1], hi[2]],
            [hi[0], hi[1], hi[2]],
            [lo[0], hi[1], hi[2]],
        ],
        [
            [hi[0], lo[1], lo[2]],
            [lo[0], lo[1], lo[2]],
            [lo[0], hi[1], lo[2]],
            [hi[0], hi[1], lo[2]],
        ],
        [
            [lo[0], lo[1], lo[2]],
            [lo[0], lo[1], hi[2]],
            [lo[0], hi[1], hi[2]],
            [lo[0], hi[1], lo[2]],
        ],
        [
            [hi[0], lo[1], hi[2]],
            [hi[0], lo[1], lo[2]],
            [hi[0], hi[1], lo[2]],
            [hi[0], hi[1], hi[2]],
        ],
        [
            [lo[0], hi[1], hi[2]],
            [hi[0], hi[1], hi[2]],
            [hi[0], hi[1], lo[2]],
            [lo[0], hi[1], lo[2]],
        ],
        [
            [lo[0], lo[1], lo[2]],
            [hi[0], lo[1], lo[2]],
            [hi[0], lo[1], hi[2]],
            [lo[0], lo[1], hi[2]],
        ],
    ];
    let uv = [
        [region[0], region[1]],
        [region[2], region[1]],
        [region[2], region[3]],
        [region[0], region[3]],
    ];
    for face in faces {
        let base = mesh.vertices.len() as u32;
        for (position, uv) in face.into_iter().zip(uv) {
            mesh.vertices.push(minecraft_terrain::mesh::Vertex {
                position,
                uv,
                color: [tint[0], tint[1], tint[2], 1.0],
                sky_light: 15.0,
                block_light: 0.0,
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        mesh.faces += 1;
    }
}

/// The game's JAR with its loot tables and recipes.
pub(crate) fn data_jar() -> Option<std::path::PathBuf> {
    let root = assets::minecraft_map::root()?;
    // The client JAR fetched from Mojang, or the one a checkout's harness got.
    let fetched = root.join(assets::minecraft_setup::CLIENT_JAR);
    if fetched.is_file() {
        return Some(fetched);
    }
    let root = root.join("harness/.gradle/loom-cache/minecraftMaven/net/minecraft");
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("minecraft-common-")
        {
            continue;
        }
        for jar in std::fs::read_dir(entry.path().join("26.3")).ok()?.flatten() {
            if jar.path().extension().is_some_and(|ext| ext == "jar") {
                return Some(jar.path());
            }
        }
    }
    None
}

/// A mob hit as one number: the kind in the top byte, the id below.
fn encode(hit: MobHit) -> u64 {
    let kind: u64 = match hit {
        MobHit::Bat(_) => 0,
        MobHit::Zombie(_) => 1,
        MobHit::Skeleton(_) => 2,
        MobHit::Creeper(_) => 3,
        MobHit::Spider(_) => 4,
        MobHit::Slime(_) => 5,
        MobHit::Enderman(_) => 6,
        MobHit::Witch(_) => 7,
        MobHit::IronGolem(_) => 8,
        MobHit::Wolf(_) => 9,
        MobHit::Villager(_) => 10,
        MobHit::Cow(_) => 11,
        MobHit::Mooshroom(_) => 12,
        MobHit::Sheep(_) => 13,
        MobHit::Pig(_) => 14,
        MobHit::Chicken(_) => 15,
    };
    (kind << 56) | (hit.id() & ((1 << 56) - 1))
}

fn decode(key: u64) -> Option<MobHit> {
    let id = key & ((1 << 56) - 1);
    Some(match key >> 56 {
        0 => MobHit::Bat(id),
        1 => MobHit::Zombie(id),
        2 => MobHit::Skeleton(id),
        3 => MobHit::Creeper(id),
        4 => MobHit::Spider(id),
        5 => MobHit::Slime(id),
        6 => MobHit::Enderman(id),
        7 => MobHit::Witch(id),
        8 => MobHit::IronGolem(id),
        9 => MobHit::Wolf(id),
        10 => MobHit::Villager(id),
        11 => MobHit::Cow(id),
        12 => MobHit::Mooshroom(id),
        13 => MobHit::Sheep(id),
        14 => MobHit::Pig(id),
        15 => MobHit::Chicken(id),
        _ => return None,
    })
}
