use crate::frame::FrameWorld;
use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MissileTarget {
    Point([f32; 3]),
    Entity {
        entity: crate::EntityRef,
        offset: [f32; 3],
    },
    Player {
        client: crate::ClientId,
        life: crate::LifeSequence,
        offset: [f32; 3],
    },
}

impl MissileTarget {
    pub fn is_point(self) -> bool {
        matches!(self, Self::Point(_))
    }

    pub(crate) fn resolve(self, world: &FrameWorld) -> Option<[f32; 3]> {
        let (origin, angles, offset) = match self {
            Self::Point(point) => return Some(point),
            Self::Entity { entity, offset } => {
                world.entity_kernel().resolve(entity).ok()?;
                let mover = world.script_mover_by_number(entity.number())?;
                (mover.state.tr_base, mover.state.apos_tr_base, offset)
            }
            Self::Player {
                client,
                life,
                offset,
            } => {
                let meta = world.client_meta(client)?;
                if meta.life_sequence != life || meta.lifecycle != crate::ClientLifecycle::Alive {
                    return None;
                }
                let ps = world.player(client)?;
                (ps.origin, ps.viewangles, offset)
            }
        };
        let axis = math_iw4::angles_to_axis(angles);
        Some(core::array::from_fn(|i| {
            origin[i] + offset[0] * axis[0][i] + offset[1] * axis[1][i] + offset[2] * axis[2][i]
        }))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MissileGuide {
    pub target: Option<MissileTarget>,
    pub top: bool,
    pub stage: u8,
    pub passed: bool,
}

pub(crate) fn turn_toward(current: Vec3, wanted: Vec3, max_radians: f32) -> Vec3 {
    let angle = current.angle_between(wanted);
    if angle <= max_radians || angle.is_nan() {
        return wanted;
    }
    let axis = current.cross(wanted);
    let axis = if axis.length_squared() < 1e-8 {
        current.any_orthonormal_vector()
    } else {
        axis.normalize()
    };
    glam::Quat::from_axis_angle(axis, max_radians) * current
}

pub(crate) fn steer(
    world: &FrameWorld,
    projectile: &mut crate::ProjectileState,
    now: i32,
    facts: crate::EquipmentRuntimeFacts,
) {
    if !projectile.live || projectile.guide.passed {
        return;
    }
    let Some(target) = projectile.guide.target else {
        return;
    };
    let Some(point) = target.resolve(world) else {
        projectile.guide.target = None;
        return;
    };
    steer_to_point(
        projectile,
        point,
        world.player(projectile.owner).map(|ps| ps.origin),
        now,
        facts,
    );
}

fn steer_to_point(
    projectile: &mut crate::ProjectileState,
    point: [f32; 3],
    owner_origin: Option<[f32; 3]>,
    now: i32,
    facts: crate::EquipmentRuntimeFacts,
) {
    let origin = Vec3::from_array(projectile.origin);
    let velocity = Vec3::from_array(projectile.velocity);
    let Some(forward) = velocity.try_normalize() else {
        return;
    };
    let delta = Vec3::from_array(point) - origin;
    let seconds = crate::MATCH_TICK_MS as f32 * 0.001;
    let mut speed = velocity.length();
    let mut goal = Vec3::from_array(point);
    let javelin = facts.missile_guidance == 3;
    if javelin {
        if now.saturating_sub(projectile.spawn_time_ms) < facts.ignition_delay_ms {
            return;
        }
        projectile.guide.stage = projectile.guide.stage.max(1);
        if projectile.guide.stage == 1 {
            let flat_forward = forward.truncate().normalize_or_zero();
            let climb_angle = (flat_forward.dot(delta.truncate()) / delta.z)
                .atan()
                .abs()
                .to_degrees();
            let ceiling = if projectile.guide.top { 3000.0 } else { 0.0 };
            let limit = if projectile.guide.top { 50.0 } else { 85.0 };
            if origin.z - point[2] > ceiling
                && (climb_angle < limit || delta.truncate().length() < 400.0)
            {
                projectile.guide.stage = 2;
            } else {
                goal.z += if projectile.guide.top {
                    15000.0
                } else {
                    10000.0
                };
                if !projectile.guide.top
                    && let Some(owner) = owner_origin
                {
                    let toward_owner = (Vec3::from_array(owner) - goal)
                        .truncate()
                        .normalize_or_zero();
                    goal.x += toward_owner.x * 700.0;
                    goal.y += toward_owner.y * 700.0;
                }
            }
        }
        if projectile.guide.stage == 2
            && delta.length_squared() <= 90000.0
            && forward.dot(delta.normalize_or_zero()) < -0.2
        {
            projectile.guide.passed = true;
            return;
        }
    }
    let Some(wanted) = (goal - origin).try_normalize() else {
        return;
    };
    let rate: f32 = if javelin {
        if projectile.guide.top { 100.0 } else { 60.0 }
    } else {
        240.0
    };
    let (direction, turn) = if javelin {
        let turn = (90.0 * (1.0 - forward.dot(wanted))).clamp(0.0, 180.0);
        if turn <= 0.1 || turn <= rate * seconds {
            (wanted, 0.0)
        } else {
            (
                // IW4 pitch is positive down, whereas a Z-up direction must
                // climb towards positive Z. Interpolate vectors directly to
                // avoid reversing pitch when converting through Euler angles.
                turn_toward(forward, wanted, rate.to_radians() * seconds),
                turn,
            )
        }
    } else {
        (
            turn_toward(forward, wanted, rate.to_radians() * seconds),
            0.0,
        )
    };
    if javelin {
        let climbing = projectile.guide.stage == 1 || velocity.z > 0.0;
        if turn < 30.0 {
            speed = (speed + if climbing { 300.0 } else { 4500.0 } * seconds).min(if climbing {
                1000.0
            } else {
                2500.0
            });
        }
        speed *= 1.0 - turn / 180.0 * 0.05;
    }
    projectile.velocity = (direction * speed).to_array();
    projectile.pos = entity_iw4::Trajectory {
        tr_time: now
            .saturating_sub(crate::MATCH_TICK_MS as i32)
            .max(projectile.spawn_time_ms),
        tr_type: entity_iw4::TR_LINEAR,
        tr_duration: 0,
        tr_delta: projectile.velocity,
        tr_base: projectile.origin,
    };
    projectile.apos = entity_iw4::fire_missile_apos(direction.to_array());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javelin_climbs_then_dives_onto_its_locked_point() {
        let point = [2200.0, 900.0, 0.0];
        let mut missile = crate::ProjectileState {
            id: crate::ProjectileId(1),
            owner: crate::ClientId(0),
            owner_life: crate::LifeSequence(1),
            weapon: 1,
            origin: [0.0, 0.0, 60.0],
            velocity: [250.0, 0.0, 100.0],
            pos: entity_iw4::Trajectory::default(),
            apos: entity_iw4::Trajectory::default(),
            entnum: 64,
            launch_time: 0,
            spawn_time_ms: 0,
            detonate_at_ms: None,
            cleanup_at_ms: 60000,
            travel_distance: 0.0,
            live: true,
            stuck_pane: None,
            grounded: false,
            guide: MissileGuide {
                target: Some(MissileTarget::Point(point)),
                top: true,
                ..Default::default()
            },
            attached_to: None,
        };
        let facts = crate::EquipmentRuntimeFacts {
            missile_guidance: 3,
            ignition_delay_ms: 500,
            ..Default::default()
        };
        let before = missile.velocity;
        steer_to_point(&mut missile, point, None, 499, facts);
        assert_eq!(missile.velocity, before);
        let mut highest = missile.origin[2];
        let mut reached = false;
        for tick in 10..1200 {
            steer_to_point(&mut missile, point, None, tick * 50, facts);
            missile.origin = (Vec3::from_array(missile.origin)
                + Vec3::from_array(missile.velocity) * 0.05)
                .to_array();
            highest = highest.max(missile.origin[2]);
            if Vec3::from_array(missile.origin).distance(Vec3::from_array(point)) < 100.0 {
                assert_eq!(missile.guide.stage, 2);
                assert!(missile.velocity[2] < 0.0);
                reached = true;
                break;
            }
            assert!(
                missile.origin[2] > 0.0,
                "rocket hit the launch terrain before reaching its target: {:?}",
                missile.origin
            );
        }
        assert!(highest > 3000.0, "top attack never climbed: {highest}");
        assert!(
            reached,
            "rocket missed its locked target: {:?}",
            missile.origin
        );
    }
}
