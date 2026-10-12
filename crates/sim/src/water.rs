//! Authoritative Minecraft breath and drowning; the existing client dvar wire
//! carries the HUD meter. Prediction uses the same voxel water movement query.
use crate::{ClientLifecycle, frame::FrameWorld, world::Tick};

pub const AIR_DVAR: &str = "iw4l_minecraft_air";
pub const AIR_CAPACITY_MS: i32 = 15_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AirSupply {
    remaining: i32,
    drowning: i32,
    lava_ms: i32,
}

impl Default for AirSupply {
    fn default() -> Self {
        Self {
            remaining: AIR_CAPACITY_MS,
            drowning: 0,
            lava_ms: 0,
        }
    }
}

impl AirSupply {
    fn lava_damage(&mut self, elapsed: i32, contact: bool) -> i32 {
        if !contact {
            self.lava_ms = 0;
            return 0;
        }
        self.lava_ms += elapsed.max(0);
        let damage = (self.lava_ms / 500) * 20;
        self.lava_ms %= 500;
        damage
    }
    /// Return damage ticks after the air runs out. Wading does not consume air.
    fn advance(&mut self, elapsed: i32, submerged: bool) -> i32 {
        let elapsed = elapsed.max(0);
        if !submerged {
            self.remaining = (self.remaining + elapsed * 4).min(AIR_CAPACITY_MS);
            self.drowning = 0;
            return 0;
        }
        let spent = self.remaining.min(elapsed);
        self.remaining -= spent;
        self.drowning += elapsed - spent;
        let ticks = self.drowning / 1000;
        self.drowning %= 1000;
        ticks
    }
}

pub(crate) fn advance(world: &mut FrameWorld, tick: Tick, elapsed: i32) {
    if !crate::voxel::active() {
        return;
    }
    for id in world.client_ids_sorted() {
        let Some(ps) = world.player(id).copied() else {
            continue;
        };
        let Some(meta) = world.client_meta(id) else {
            continue;
        };
        let alive = meta.lifecycle == ClientLifecycle::Alive;
        let submerged = alive && crate::voxel::water_level(ps.origin, ps.view_height_current) == 3;
        let in_lava = alive && crate::voxel::lava_contact(ps.origin);
        let meta = world.client_meta_mut(id);
        if !alive {
            meta.minecraft_air = AirSupply::default();
        }
        let damage_ticks = meta.minecraft_air.advance(elapsed, submerged);
        let lava_damage = meta.minecraft_air.lava_damage(elapsed, in_lava);
        let payload = format!("{}:{}", meta.minecraft_air.remaining, u8::from(submerged));
        if let Some((_, value)) = meta
            .client_dvars
            .iter_mut()
            .find(|(key, _)| key == AIR_DVAR)
        {
            *value = payload;
        } else {
            meta.client_dvars.push((AIR_DVAR.into(), payload));
        }
        if damage_ticks > 0 {
            crate::damage::apply_script_hit(
                world,
                tick,
                &crate::script::ScriptHit {
                    piece: None,
                    target: crate::script::HitTarget::Player(id),
                    amount: 10 * damage_ticks,
                    origin: ps.origin,
                    attacker: None,
                    inflictor: None,
                    means: "MOD_DROWN",
                    weapon: 0,
                    flags: 0,
                    hitloc: 0,
                },
            );
        }
        if lava_damage > 0 {
            crate::damage::apply_script_hit(
                world,
                tick,
                &crate::script::ScriptHit {
                    piece: None,
                    target: crate::script::HitTarget::Player(id),
                    amount: lava_damage,
                    origin: ps.origin,
                    attacker: None,
                    inflictor: None,
                    means: "MOD_BURNED",
                    weapon: 0,
                    flags: 0,
                    hitloc: 0,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lava_deals_damage_every_half_second_and_stops_after_leaving() {
        let mut air = AirSupply::default();
        assert_eq!(air.lava_damage(499, true), 0);
        assert_eq!(air.lava_damage(1, true), 20);
        assert_eq!(air.lava_damage(1000, true), 40);
        assert_eq!(air.lava_damage(400, true), 0);
        assert_eq!(air.lava_damage(100, false), 0);
        assert_eq!(air.lava_damage(100, true), 0);
    }
    #[test]
    fn air_supply_only_drowns_after_fifteen_seconds() {
        let mut air = AirSupply::default();
        assert_eq!(air.advance(15000, true), 0);
        assert_eq!(air.remaining, 0);
        assert_eq!(air.advance(999, true), 0);
        assert_eq!(air.advance(1, true), 1);
        assert_eq!(air.advance(2000, true), 2);
    }
    #[test]
    fn surfacing_recovers_air_and_resets_damage_timer() {
        let mut air = AirSupply::default();
        air.advance(15999, true);
        assert_eq!(air.advance(1000, false), 0);
        assert_eq!(air.remaining, 4000);
        assert_eq!(air.advance(4000, true), 0);
        assert_eq!(air.advance(999, true), 0);
        air.advance(4000, false);
        assert_eq!(air, AirSupply::default());
    }
}

#[cfg(test)]
mod swim_tests {
    use movement_iw4::GroundTraceInput;
    use movement_iw4::{CollisionBackend, MoveBounds, Pml, swim_move};
    use playerstate_iw4::{PlayerState, UserCmd, buttons};
    struct Wall;
    impl CollisionBackend for Wall {
        fn water_level(&self, _: [f32; 3], _: f32) -> u8 {
            2
        }
        fn trace(&self, i: GroundTraceInput) -> trace_iw4::Trace {
            let stop = 50.0 - i.maxs[0];
            let fraction = if i.end[0] > stop && i.end[0] > i.start[0] {
                ((stop - i.start[0]) / (i.end[0] - i.start[0])).clamp(0.0, 1.0)
            } else {
                1.0
            };
            trace_iw4::Trace {
                fraction,
                normal: [-1.0, 0.0, 0.0],
                ..Default::default()
            }
        }
    }
    fn run(cmd: UserCmd, velocity: [f32; 3]) -> PlayerState {
        let mut ps = crate::world::blank_player_state();
        ps.speed = 190;
        ps.velocity = velocity;
        let pml = Pml {
            forward: [1.0, 0.0, 0.0],
            right: [0.0, 1.0, 0.0],
            up: [0.0, 0.0, 1.0],
            frametime: 0.05,
            msec: 50,
            walking: 0,
            ground_plane: 0,
            almost_ground_plane: 0,
            ground_trace: [0; 11],
            previous_origin: [0.0; 3],
            previous_velocity: velocity,
            holdrand: 0,
            jump_animations: [None; 4],
            mantle_movetype: None,
            landing_animation: false,
            fall_damage: 0,
        };
        for _ in 0..20 {
            swim_move(
                &mut ps,
                &pml,
                &cmd,
                MoveBounds {
                    mins: [-15.0, -15.0, 0.0],
                    maxs: [15.0, 15.0, 70.0],
                    tracemask: 1,
                },
                &Wall,
            );
        }
        ps
    }
    #[test]
    fn water_swimming_rises_dives_slows_falls_and_keeps_wall_collision() {
        let up = run(
            UserCmd {
                buttons: buttons::JUMP,
                ..Default::default()
            },
            [0.0; 3],
        );
        let down = run(
            UserCmd {
                buttons: buttons::CROUCH,
                ..Default::default()
            },
            [0.0; 3],
        );
        let falling = run(UserCmd::default(), [0.0, 0.0, -1000.0]);
        let forward = run(
            UserCmd {
                forwardmove: 127,
                ..Default::default()
            },
            [0.0; 3],
        );
        assert!(up.origin[2] > 40.0);
        assert!(down.origin[2] < -40.0);
        assert!(falling.velocity[2] > -20.0);
        assert!(
            falling.velocity[2] > 0.0,
            "an idle submerged player must float upwards"
        );
        assert!(forward.origin[0] > 20.0 && forward.origin[0] <= 35.1);
    }
}
