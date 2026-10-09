use std::collections::HashMap;

use minecraft_terrain::block_particles::BlockParticles;
use minecraft_terrain::mesh::{Atlas, ChunkMesh, SectionVertex};
use minecraft_terrain::pack::{PackStack, ResourceId};
use minecraft_terrain::scene::{HandcraftedScene, Scene};
use minecraft_terrain::terrain::TerrainStream;
use minecraftoss_core::registries::Registries;

type BlockPos = (i32, i32, i32);

/// Bullet damage that mines a block of hardness 1 in four shots: a 40-damage
/// bullet (an ACR up close) breaks dirt, of hardness 0.5, on the second.
const DAMAGE_PER_HARDNESS: f32 = 160.0;
const MAX_BLAST_RADIUS: f32 = 32.0;
/// Progress on a block no longer being shot is forgotten after this long.
const PROGRESS_SECONDS: f64 = 6.0;
const TICK_SECONDS: f64 = 1.0 / 20.0;

pub(crate) struct Mining {
    /// Progress in 0..1 of each block being shot, and when it was last hit.
    progress: HashMap<BlockPos, (f32, f64)>,
    particles: Option<BlockParticles>,
    tick_clock: f64,
    rng: u64,
}

impl Default for Mining {
    fn default() -> Self {
        Self {
            progress: HashMap::new(),
            particles: None,
            tick_clock: 0.0,
            rng: 0x2545_f491_4f6c_dd1d,
        }
    }
}

/// The world pieces mining reads and edits.
pub(crate) struct WorldRefs<'a> {
    pub stream: &'a mut TerrainStream,
    pub scene: &'a mut HandcraftedScene,
    pub packs: &'a PackStack,
    pub atlas: &'a Atlas,
    pub registries: &'a Registries,
}

impl Mining {
    fn next_f32(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        ((self.rng >> 40) as u32 as f32) / ((1u32 << 24) as f32)
    }

    pub(crate) fn apply(
        &mut self,
        events: Vec<sim::voxel::VoxelEvent>,
        world: &mut WorldRefs<'_>,
        now: f64,
    ) -> Vec<(BlockPos, minecraft_terrain::scene::Block, bool, bool)> {
        if self.particles.is_none() {
            self.particles = BlockParticles::new(world.packs).ok();
        }
        let mut broken: Vec<(BlockPos, bool, bool)> = Vec::new();
        // Blocks broken by this batch: the rest of a shotgun's pellets into
        // one pass through, rather than starting a crack on a block that is
        // about to go.
        let mut gone = std::collections::HashSet::new();
        for event in events {
            match event {
                sim::voxel::VoxelEvent::Shot {
                    block,
                    damage,
                    drop_items,
                } => {
                    let pos = (block[0], block[1], block[2]);
                    if gone.contains(&pos) {
                        continue;
                    }
                    let Some(hardness) = hardness(world, pos) else {
                        continue;
                    };
                    let entry = self.progress.entry(pos).or_insert((0.0, now));
                    entry.0 += if hardness <= 0.0 {
                        1.0
                    } else {
                        damage / DAMAGE_PER_HARDNESS / hardness
                    };
                    entry.1 = now;
                    if entry.0 >= 1.0 {
                        self.progress.remove(&pos);
                        if let Some(block) = Scene::block(&*world.scene, pos).cloned()
                            && let Some(particles) = self.particles.as_mut()
                        {
                            let _ = particles.spawn(
                                world.packs,
                                &*world.scene,
                                pos,
                                &block,
                                world.atlas,
                            );
                        }
                        broken.push((pos, false, drop_items));
                        gone.insert(pos);
                    }
                }
                sim::voxel::VoxelEvent::Explosion {
                    center,
                    radius,
                    cone,
                } => {
                    for pos in self.exploded_positions(world, center, radius, cone) {
                        if hardness(world, pos).is_some() && gone.insert(pos) {
                            self.progress.remove(&pos);
                            broken.push((pos, true, false));
                        }
                    }
                }
                sim::voxel::VoxelEvent::Ray {
                    from,
                    to,
                    damage,
                    drop_items,
                } => {
                    // Blocks without collision the bullet passed through.
                    for pos in blocks_on_segment(from, to) {
                        if gone.contains(&pos) || !passable(world, pos) {
                            continue;
                        }
                        let Some(hardness) = hardness(world, pos) else {
                            continue;
                        };
                        let entry = self.progress.entry(pos).or_insert((0.0, now));
                        entry.0 += if hardness <= 0.0 {
                            1.0
                        } else {
                            damage / DAMAGE_PER_HARDNESS / hardness
                        };
                        entry.1 = now;
                        if entry.0 >= 1.0 {
                            self.progress.remove(&pos);
                            if let Some(block) = Scene::block(&*world.scene, pos).cloned()
                                && let Some(particles) = self.particles.as_mut()
                            {
                                let _ = particles.spawn(
                                    world.packs,
                                    &*world.scene,
                                    pos,
                                    &block,
                                    world.atlas,
                                );
                            }
                            broken.push((pos, false, drop_items));
                            gone.insert(pos);
                        }
                    }
                }
                sim::voxel::VoxelEvent::MobShot { .. } => {}
            }
        }
        // No crack outlives its block, however it went.
        self.progress.retain(|pos, (_, at)| {
            now - *at < PROGRESS_SECONDS
                && !gone.contains(pos)
                && Scene::block(&*world.scene, *pos).is_some()
        });
        if broken.is_empty() {
            return Vec::new();
        }
        broken.sort_unstable();
        broken.dedup_by_key(|(pos, ..)| *pos);
        let mut out = Vec::with_capacity(broken.len());
        for &(pos, blast, drop_items) in &broken {
            if let Some(block) = Scene::block(&*world.scene, pos).cloned() {
                out.push((pos, block, blast, drop_items));
            }
            world.scene.set(pos, None);
            sim::voxel::set_block_shape(pos.0, pos.1, pos.2, 0);
        }
        let positions: Vec<BlockPos> = broken.iter().map(|(pos, ..)| *pos).collect();
        world.stream.record_edits(world.scene, &positions);
        world.stream.mark_edited(world.scene, &positions);
        out
    }

    fn exploded_positions(
        &mut self,
        world: &WorldRefs<'_>,
        center: [f64; 3],
        radius: f32,
        cone: Option<([f32; 3], f32)>,
    ) -> Vec<BlockPos> {
        if !radius.is_finite() || radius <= 0.0 || !center.iter().all(|v| v.is_finite()) {
            return Vec::new();
        }
        let radius = radius.min(MAX_BLAST_RADIUS);
        let power = (radius * 0.75).max(4.0);
        let range = world.stream.states.vertical_range();
        let mut seen = std::collections::HashSet::new();
        let mut inserted = Vec::new();
        for xx in 0..16 {
            for yy in 0..16 {
                for zz in 0..16 {
                    if !(xx == 0 || xx == 15 || yy == 0 || yy == 15 || zz == 0 || zz == 15) {
                        continue;
                    }
                    let unit = |i: i32| f64::from(i as f32 / 15.0f32 * 2.0f32 - 1.0f32);
                    let (mut xd, mut yd, mut zd) = (unit(xx), unit(yy), unit(zz));
                    let d = (xd * xd + yd * yd + zd * zd).sqrt();
                    xd /= d;
                    yd /= d;
                    zd /= d;
                    if cone.is_some_and(|(forward, cosine)| {
                        xd * f64::from(forward[0])
                            + yd * f64::from(forward[1])
                            + zd * f64::from(forward[2])
                            < f64::from(cosine)
                    }) {
                        continue;
                    }
                    let mut remaining = power * (0.7f32 + self.next_f32() * 0.6f32);
                    let [mut xp, mut yp, mut zp] = center;
                    let step = f64::from(0.3f32);
                    let mut distance = 0.0;
                    while remaining > 0.0 && distance <= f64::from(radius) {
                        let pos = (xp.floor() as i32, yp.floor() as i32, zp.floor() as i32);
                        if (yp < f64::from(range.start) && yd <= 0.0)
                            || (yp >= f64::from(range.end) && yd >= 0.0)
                        {
                            break;
                        }
                        if range.contains(&pos.1)
                            && let Some(resistance) = explosion_resistance(world, pos)
                        {
                            remaining -= (resistance + 0.3f32) * 0.3f32;
                        }
                        if remaining > 0.0 && range.contains(&pos.1) && seen.insert(pos) {
                            inserted.push(pos);
                        }
                        xp += xd * step;
                        yp += yd * step;
                        zp += zd * step;
                        remaining -= 0.225_000_01_f32;
                        distance += step;
                    }
                }
            }
        }
        inserted
    }

    /// Steps the break particles at 20 ticks a second.
    pub(crate) fn tick(&mut self, scene: &HandcraftedScene, dt: f64) -> f32 {
        self.tick_clock += dt;
        while self.tick_clock >= TICK_SECONDS {
            self.tick_clock -= TICK_SECONDS;
            if let Some(particles) = self.particles.as_mut() {
                particles.tick(scene);
            }
        }
        (self.tick_clock / TICK_SECONDS) as f32
    }

    /// The break particles as section vertices, facing `forward`.
    pub(crate) fn particle_mesh(
        &self,
        atlas: &Atlas,
        forward: glam::Vec3,
        partial: f32,
        light: &minecraft_terrain::lighting::SkyLight,
    ) -> (Vec<u8>, Vec<u32>) {
        let Some(particles) = self.particles.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let mut mesh = ChunkMesh::default();
        particles.append_mesh(&mut mesh, atlas, forward, partial, light);
        let vertices: Vec<SectionVertex> = mesh
            .vertices
            .iter()
            .map(SectionVertex::from_vertex)
            .collect();
        (bytemuck::cast_slice(&vertices).to_vec(), mesh.indices)
    }

    /// A cube a hair larger than each block being mined, textured with its
    /// destroy stage from a strip of the ten: position then uv.
    pub(crate) fn crack_mesh(&self) -> (Vec<[f32; 5]>, Vec<u32>) {
        let (mut vertices, mut indices) = (Vec::new(), Vec::new());
        for (&(x, y, z), &(progress, _)) in &self.progress {
            let stage = ((progress * 10.0) as i32).clamp(0, 9) as f32;
            let a = [x as f32 - 0.002, y as f32 - 0.002, z as f32 - 0.002];
            let b = [x as f32 + 1.002, y as f32 + 1.002, z as f32 + 1.002];
            let p = [
                [a[0], a[1], a[2]],
                [b[0], a[1], a[2]],
                [b[0], b[1], a[2]],
                [a[0], b[1], a[2]],
                [a[0], a[1], b[2]],
                [b[0], a[1], b[2]],
                [b[0], b[1], b[2]],
                [a[0], b[1], b[2]],
            ];
            // The viewer's `interaction_mesh` faces.
            for face in [
                [0, 3, 2, 1],
                [5, 6, 7, 4],
                [4, 7, 3, 0],
                [1, 2, 6, 5],
                [3, 7, 6, 2],
                [4, 0, 1, 5],
            ] {
                let first = vertices.len() as u32;
                for (index, uv) in
                    face.into_iter()
                        .zip([[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]])
                {
                    let q = p[index];
                    vertices.push([q[0], q[1], q[2], (stage + uv[0]) / 10.0, uv[1]]);
                }
                indices.extend_from_slice(&[
                    first,
                    first + 1,
                    first + 2,
                    first,
                    first + 2,
                    first + 3,
                ]);
            }
        }
        (vertices, indices)
    }
}

/// The destroy speed of a breakable block, or `None` for air, fluids and
/// unbreakable blocks.
/// A block bullets pass through: one with no collision shape.
fn passable(world: &WorldRefs<'_>, pos: BlockPos) -> bool {
    Scene::block(&*world.scene, pos)
        .and_then(|block| world.stream.states.state_of(block))
        .is_some_and(|state| world.registries.blocks.collision_boxes(state).is_empty())
}

/// The blocks a segment passes through, in order (Amanatides–Woo).
fn blocks_on_segment(from: [f64; 3], to: [f64; 3]) -> Vec<BlockPos> {
    let d: [f64; 3] = std::array::from_fn(|k| to[k] - from[k]);
    let mut cell: [i32; 3] = std::array::from_fn(|k| from[k].floor() as i32);
    let end: [i32; 3] = std::array::from_fn(|k| to[k].floor() as i32);
    let step: [i32; 3] = std::array::from_fn(|k| if d[k] > 0.0 { 1 } else { -1 });
    let delta: [f64; 3] = std::array::from_fn(|k| {
        if d[k] == 0.0 {
            f64::INFINITY
        } else {
            1.0 / d[k].abs()
        }
    });
    let mut next: [f64; 3] = std::array::from_fn(|k| {
        if d[k] == 0.0 {
            f64::INFINITY
        } else if d[k] > 0.0 {
            (f64::from(cell[k]) + 1.0 - from[k]) * delta[k]
        } else {
            (from[k] - f64::from(cell[k])) * delta[k]
        }
    });
    let mut out = vec![(cell[0], cell[1], cell[2])];
    // A shot reaches a few hundred blocks at most.
    while cell != end && out.len() < 512 {
        let k = if next[0] <= next[1] && next[0] <= next[2] {
            0
        } else if next[1] <= next[2] {
            1
        } else {
            2
        };
        if next[k] > 1.0 {
            break;
        }
        cell[k] += step[k];
        next[k] += delta[k];
        out.push((cell[0], cell[1], cell[2]));
    }
    out
}

fn hardness(world: &WorldRefs<'_>, pos: BlockPos) -> Option<f32> {
    let block = Scene::block(&*world.scene, pos)?;
    if matches!(
        block.id.path.as_str(),
        "water" | "lava" | "air" | "cave_air" | "void_air"
    ) {
        return None;
    }
    let state = world.stream.states.state_of(block)?;
    let speed = world.registries.blocks.state(state).destroy_speed;
    (speed >= 0.0).then_some(speed)
}

/// `ExplosionDamageCalculator.getBlockExplosionResistance`: none for air.
fn explosion_resistance(world: &WorldRefs<'_>, pos: BlockPos) -> Option<f32> {
    let block = Scene::block(&*world.scene, pos)?;
    let state = world.stream.states.state_of(block)?;
    let blocks = &world.registries.blocks;
    if blocks.is_air(state) {
        return None;
    }
    let fluid = if matches!(block.id.path.as_str(), "water" | "lava") {
        100.0f32
    } else {
        0.0
    };
    Some(blocks.state(state).explosion_resistance.max(fluid))
}

/// The ten destroy stages side by side, 16 pixels each.
pub(crate) fn crack_strip(packs: &PackStack) -> anyhow::Result<image::RgbaImage> {
    let mut strip = image::RgbaImage::new(16 * 10, 16);
    for stage in 0..10u32 {
        let id = ResourceId::parse(&format!("minecraft:block/destroy_stage_{stage}"))?;
        if let Some(bytes) = packs.texture(&id)? {
            let img =
                image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)?.to_rgba8();
            let tile = image::imageops::resize(&img, 16, 16, image::imageops::FilterType::Nearest);
            image::imageops::replace(&mut strip, &tile, i64::from(stage) * 16, 0);
        }
    }
    Ok(strip)
}
