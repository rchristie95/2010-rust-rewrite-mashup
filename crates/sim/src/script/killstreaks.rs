use bevy_ecs::prelude::World;

use super::{ArrayKey, Runtime, Value};

pub const CONTROLS_MODULE: &str = "iw4l/killstreak_controls";
pub const CONTROLS_SOURCE: &str = include_str!("killstreak_controls.gsc.txt");
pub const REWARDS_DVAR: &str = "ui_iw4l_owned_killstreaks";
pub const SELECTED_DVAR: &str = "ui_iw4l_selected_killstreak";
pub const USE_MENU: &str = "iw4l_use_killstreak";
pub const LOADOUT_MENU: &str = "iw4l_killstreak_loadout";
pub const LOADOUT_DVAR: &str = "ui_iw4l_killstreak_loadout";
const MINECRAFT_ALL_FIELD: &str = "iw4l_minecraft_all_killstreaks";

pub(crate) fn catalog(runtime: &Runtime) -> Vec<(String, i32)> {
    let Some(table) = runtime.tables.get("mp/killstreaktable.csv") else {
        return Vec::new();
    };
    let mut rows: Vec<_> = (0..table.rows)
        .filter_map(|row| {
            let name = table.cell(row, 1)?;
            let cost = table.cell(row, 4)?.parse::<i32>().ok()?;
            ((1..=25).contains(&cost) && !matches!(name, "none" | "sentry"))
                .then(|| (name.to_owned(), cost))
        })
        .collect();
    rows.sort_by_key(|(_, cost)| *cost);
    rows
}

fn minecraft_mode(runtime: &Runtime, client: u32) -> Option<bool> {
    let player = runtime.players.get(&client)?;
    let symbol = runtime
        .program
        .as_ref()?
        .symbol_ids
        .get(MINECRAFT_ALL_FIELD)
        .or_else(|| runtime.dynamic_symbols.get(MINECRAFT_ALL_FIELD))?;
    match runtime.objects.get(&player.object)?.get(symbol)? {
        Value::Int(value) => Some(*value != 0),
        _ => None,
    }
}

pub(crate) fn minecraft_all(runtime: &Runtime, client: u32) -> bool {
    runtime
        .program
        .as_ref()
        .is_some_and(|program| program.names.contains_key("iw4l/minecraft_rewards::kill"))
        && runtime.players.contains_key(&client)
        && minecraft_mode(runtime, client).unwrap_or(true)
}

fn profile(world: &World, client: u32) -> String {
    let Some(all) = minecraft_mode(world.resource::<Runtime>(), client) else {
        return String::new();
    };
    let store = world.resource::<crate::PersistentDataStore>();
    let mut values = vec![u8::from(all).to_string()];
    for index in 0..3 {
        let keys = [
            structured_data_iw4::Key::Name("killstreaks"),
            structured_data_iw4::Key::Index(index),
        ];
        let Ok(structured_data_iw4::Value::String(name)) =
            store.read(crate::ClientId(client), &keys)
        else {
            return String::new();
        };
        values.push(name.to_owned());
    }
    values.join("|")
}

pub(crate) fn configure(world: &mut World, client: u32, response: &str) {
    if !world
        .resource::<crate::step::StepRequest>()
        .reason
        .advances_authority_world()
    {
        return;
    }
    let fields: Vec<_> = response.split('|').collect();
    let [all, a, b, c] = fields.as_slice() else {
        return;
    };
    if !matches!(*all, "0" | "1") {
        return;
    }
    let rows = catalog(world.resource::<Runtime>());
    let mut costs = Vec::new();
    let mut names = Vec::new();
    for index in [a, b, c] {
        let Ok(index) = index.parse::<usize>() else {
            return;
        };
        let Some((name, cost)) = rows.get(index) else {
            return;
        };
        if costs.contains(cost) {
            return;
        }
        costs.push(*cost);
        names.push(name.as_str());
    }
    let [a, b, c] = names.as_slice() else {
        return;
    };
    let player = super::host::players::player_object(world, client);
    if player == Value::Undefined {
        return;
    }
    let data = [a, b, c]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            (
                vec![Value::string("killstreaks"), Value::Int(index as i32)],
                Value::string(**name),
            )
        })
        .collect::<Vec<_>>();
    if super::host::natives::player::write_class_data(world, client, &data).is_err() {
        return;
    }
    if let Value::Object(object) = player {
        world.resource_mut::<Runtime>().set_object_field(
            object,
            MINECRAFT_ALL_FIELD,
            Value::Int(i32::from(*all == "1")),
        );
    }
    let now = super::host::players::now_ms(world);
    let all_rewards = minecraft_all(world.resource::<Runtime>(), client);
    let _ = super::runtime::run_now(
        world,
        "iw4l/killstreak_controls::configure",
        player,
        [a, b, c]
            .into_iter()
            .map(|name| Value::string(if all_rewards { "none" } else { *name }))
            .collect(),
        now,
    );
}

fn owned(runtime: &mut Runtime, client: u32) -> Vec<String> {
    let Some(object) = runtime.players.get(&client).map(|player| player.object) else {
        return Vec::new();
    };
    let Value::Array(pers) = runtime.object_field(object, "pers") else {
        return Vec::new();
    };
    let Some(Value::Array(queue)) = runtime
        .arrays
        .get(&pers)
        .and_then(|pers| pers.get(&ArrayKey::String("killstreaks".into())))
    else {
        return Vec::new();
    };
    let entries: Vec<u64> = runtime
        .arrays
        .get(queue)
        .into_iter()
        .flat_map(|queue| queue.iter())
        .filter_map(|(key, value)| match (key, value) {
            (ArrayKey::Integer(_), Value::Object(object)) => Some(*object),
            _ => None,
        })
        .collect();
    entries
        .into_iter()
        .filter_map(|object| match runtime.object_field(object, "streakname") {
            Value::String(name) => Some(name.to_string()),
            _ => None,
        })
        .collect()
}

fn shortcuts(runtime: &mut Runtime, client: u32) -> Vec<String> {
    let available = owned(runtime, client);
    let loadout = runtime
        .players
        .get(&client)
        .map(|player| player.object)
        .map(|object| runtime.object_field(object, "killstreaks"));
    let mut names: Vec<String> = if minecraft_all(runtime, client) {
        catalog(runtime).into_iter().map(|(name, _)| name).collect()
    } else {
        match loadout {
            Some(Value::Array(array)) => runtime
                .arrays
                .get(&array)
                .into_iter()
                .flat_map(|entries| entries.iter())
                .filter_map(|(key, value)| match (key, value) {
                    (ArrayKey::Integer(_), Value::String(name)) => Some(name.to_string()),
                    _ => None,
                })
                .filter(|name| !name.contains("-rollover"))
                .collect(),
            _ => Vec::new(),
        }
    };
    for name in &available {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
        .into_iter()
        .map(|name| {
            if available.contains(&name) {
                name
            } else {
                String::new()
            }
        })
        .collect()
}

pub(crate) fn publish(world: &mut World) {
    if !world
        .resource::<crate::step::StepRequest>()
        .reason
        .advances_authority_world()
    {
        return;
    }
    let runtime = world.resource::<Runtime>();
    if !runtime.program.as_ref().is_some_and(|program| {
        program
            .names
            .contains_key("iw4l/killstreak_controls::activate")
    }) {
        return;
    }
    let clients: Vec<_> = runtime.players.keys().copied().collect();
    let profiles: std::collections::BTreeMap<_, _> = clients
        .iter()
        .map(|client| (*client, profile(world, *client)))
        .collect();
    let mut runtime = world.resource_mut::<Runtime>();
    let queues: Vec<_> = clients
        .into_iter()
        .flat_map(|client| {
            let selected = owned(&mut runtime, client)
                .into_iter()
                .next()
                .unwrap_or_default();
            [
                (
                    client,
                    REWARDS_DVAR,
                    shortcuts(&mut runtime, client).join("|"),
                ),
                (client, SELECTED_DVAR, selected),
                (
                    client,
                    LOADOUT_DVAR,
                    profiles.get(&client).cloned().unwrap_or_default(),
                ),
            ]
        })
        .collect();
    drop(runtime);
    let mut frame = crate::frame::FrameWorld::from_world(world);
    for (client, dvar, queue) in queues {
        if let Some(meta) = frame.client_meta(crate::ClientId(client)) {
            if meta
                .client_dvars
                .iter()
                .any(|(key, value)| key == dvar && value == &queue)
            {
                continue;
            }
            let dvars = &mut frame.client_meta_mut(crate::ClientId(client)).client_dvars;
            if let Some((_, value)) = dvars.iter_mut().find(|(key, _)| key == dvar) {
                *value = queue;
            } else {
                dvars.push((dvar.into(), queue));
            }
        }
    }
}

pub(crate) fn activate(world: &mut World, client: u32, response: &str) {
    if !world
        .resource::<crate::step::StepRequest>()
        .reason
        .advances_authority_world()
    {
        return;
    }
    let Some((index, name)) = response.split_once(':') else {
        return;
    };
    let Ok(index) = index.parse::<usize>() else {
        return;
    };
    let player = super::host::players::player_object(world, client);
    if player == Value::Undefined {
        return;
    }
    let slots = shortcuts(&mut world.resource_mut::<Runtime>(), client);
    if name.is_empty() || slots.get(index).map(String::as_str) != Some(name) {
        return;
    }
    let available = owned(&mut world.resource_mut::<Runtime>(), client);
    let Some(queue_index) = available.iter().position(|reward| reward == name) else {
        return;
    };
    let function = world
        .resource::<Runtime>()
        .program
        .as_ref()
        .and_then(|program| program.names.get("iw4l/killstreak_controls::activate"))
        .copied();
    if world.query::<&super::Thread>().iter(world).any(|thread| {
        thread
            .frames
            .iter()
            .any(|frame| Some(frame.function) == function && frame.receiver == player)
    }) {
        return;
    }
    let now = super::host::players::now_ms(world);
    let _ = super::runtime::run_now(
        world,
        "iw4l/killstreak_controls::activate",
        player,
        vec![Value::Int(queue_index as i32), Value::string(name)],
        now,
    );
}
