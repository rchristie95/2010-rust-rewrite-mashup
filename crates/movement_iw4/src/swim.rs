use crate::{CollisionBackend, MoveBounds, Pml, accelerate, step_slide_move};
use playerstate_iw4::{PlayerState, UserCmd, buttons};

/// Collision-aware swimming, with view-directed movement, rise and dive.
pub fn swim_move<C: CollisionBackend>(
    ps: &mut PlayerState,
    pml: &Pml,
    cmd: &UserCmd,
    bounds: MoveBounds,
    collision: &C,
) {
    let drag = libm::expf(-4.0 * pml.frametime);
    for v in &mut ps.velocity {
        *v = (*v * drag).clamp(-180.0, 180.0);
    }
    let mut wish = core::array::from_fn::<_, 3, _>(|i| {
        pml.forward[i] * f32::from(cmd.forwardmove) / 127.0
            + pml.right[i] * f32::from(cmd.rightmove) / 127.0
    });
    if cmd.buttons & buttons::JUMP != 0 {
        wish[2] += 1.0;
    }
    if cmd.buttons & (buttons::CROUCH | buttons::PRONE) != 0 {
        wish[2] -= 1.0;
    }
    // With no dive input the body is buoyant, rather than slowly sinking
    // under reduced gravity forever. Explicit view-directed diving still wins.
    if wish[2].abs() < 0.01 {
        let buoyancy = if collision.water_level(ps.origin, ps.view_height_current) >= 2 {
            24.0
        } else {
            0.0
        };
        ps.velocity[2] += (buoyancy - ps.velocity[2]) * (6.0 * pml.frametime).min(1.0);
    }
    let length = libm::sqrtf(wish.iter().map(|v| v * v).sum());
    if length > 0.0 {
        for v in &mut wish {
            *v /= length;
        }
        accelerate(
            ps,
            pml,
            &wish,
            ps.speed as f32 * 0.55 * length.min(1.0),
            6.0,
        );
    }
    step_slide_move(
        ps,
        pml,
        collision,
        bounds.mins,
        bounds.maxs,
        bounds.tracemask,
        Some(40.0),
    );
}
