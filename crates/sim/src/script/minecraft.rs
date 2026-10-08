//! Confirmed Minecraft damage enters the normal MW2 HUD and reward scripts on
//! the authority step. A delayed reply cannot reward another life after respawn.
use bevy_ecs::prelude::{Resource, World};

pub const REWARDS_MODULE: &str = "iw4l/minecraft_rewards";
pub const REWARDS_SOURCE: &str = include_str!("minecraft_rewards.gsc.txt");

#[derive(Resource, Clone, Default)]
pub(crate) struct MobFeedback(pub Vec<(crate::voxel::MobAttackCredit, u32)>);

pub(crate) fn apply_feedback(world: &mut World) {
    if !world
        .resource::<crate::step::StepRequest>()
        .reason
        .advances_authority_world()
    {
        return;
    }
    let feedback = std::mem::take(&mut world.resource_mut::<MobFeedback>().0);
    for (credit, kills) in feedback {
        let frame = crate::frame::FrameWorld::from_world(world);
        let eligible = frame.client_meta(credit.client).is_some_and(|meta| {
            meta.lifecycle == crate::ClientLifecycle::Alive && meta.life_sequence == credit.life
        });
        drop(frame);
        if !eligible {
            continue;
        }
        let player = super::host::players::player_object(world, credit.client.0);
        if player == super::Value::Undefined {
            continue;
        }
        let now = super::host::players::now_ms(world);
        if super::runtime::run_now(
            world,
            "iw4l/minecraft_rewards::hit",
            player.clone(),
            Vec::new(),
            now,
        )
        .is_err()
        {
            return;
        }
        for _ in 0..kills {
            let all_rewards = super::killstreaks::minecraft_all(
                world.resource::<super::Runtime>(),
                credit.client.0,
            );
            if super::runtime::run_now(
                world,
                "iw4l/minecraft_rewards::kill",
                player.clone(),
                vec![super::Value::Int(i32::from(all_rewards))],
                now,
            )
            .is_err()
            {
                return;
            }
            award_all(world, credit.client.0);
        }
    }
}

pub(crate) fn award_all(world: &mut World, client: u32) {
    if !super::killstreaks::minecraft_all(world.resource::<super::Runtime>(), client) {
        return;
    }
    let rows = super::killstreaks::catalog(world.resource::<super::Runtime>());
    let names = rows
        .iter()
        .map(|(name, _)| super::Value::string(name))
        .collect();
    let costs = rows
        .iter()
        .map(|(_, cost)| super::Value::Int(*cost))
        .collect();
    let Ok(names) = super::host::arrays::new_array(world, names) else {
        return;
    };
    let Ok(costs) = super::host::arrays::new_array(world, costs) else {
        return;
    };
    let player = super::host::players::player_object(world, client);
    let now = super::host::players::now_ms(world);
    let _ = super::runtime::run_now(
        world,
        "iw4l/minecraft_rewards::award_all",
        player,
        vec![names, costs],
        now,
    );
}
