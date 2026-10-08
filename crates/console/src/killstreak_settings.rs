use bevy::prelude::*;

fn rewards(catalog: &asset_game::MenuCatalog) -> Vec<(String, u32)> {
    let Some(table) = catalog.string_table("mp/killstreakTable.csv") else {
        return Vec::new();
    };
    let mut rows: Vec<_> = (0..table.rows)
        .filter_map(|row| {
            let name = table.cell(row as i32, 1);
            let cost = table.cell(row as i32, 4).parse::<u32>().ok()?;
            ((1..=25).contains(&cost) && !matches!(name, "none" | "sentry"))
                .then(|| (name.to_owned(), cost))
        })
        .collect();
    rows.sort_by_key(|(_, cost)| *cost);
    rows
}

fn valid(names: &[String; 3], rows: &[(String, u32)]) -> bool {
    let mut costs = Vec::new();
    names.iter().all(|name| {
        let Some((_, cost)) = rows.iter().find(|(key, _)| key == name) else {
            return false;
        };
        if costs.contains(cost) {
            return false;
        }
        costs.push(*cost);
        true
    })
}

pub(crate) fn menu(
    mut events: MessageReader<crate::ConsoleCommand>,
    mut settings: ResMut<frame::GameSettings>,
    mut catalog: ResMut<asset_game::MenuCatalog>,
    mut dvars: ResMut<frame::UiMenuDvars>,
) {
    let rows = rewards(&catalog);
    if rows.is_empty() {
        return;
    }
    if !valid(&settings.killstreaks, &rows) {
        settings.killstreaks = ["uav", "airdrop", "predator_missile"].map(str::to_owned);
        settings.touch();
    }
    for command in events.read() {
        if !matches!(command.name.as_str(), "set" | "seta") {
            continue;
        }
        let [key, value, ..] = command.args.as_slice() else {
            continue;
        };
        if key == "ui_minecraft_all_killstreaks" {
            settings.minecraft_all_killstreaks = value == "1";
            settings.touch();
        } else if let Some(slot) = key
            .strip_prefix("ui_killstreak_")
            .and_then(|slot| slot.parse::<usize>().ok())
            .filter(|&slot| slot < 3)
        {
            let mut next = settings.killstreaks.clone();
            next[slot] = value.clone();
            if valid(&next, &rows) {
                settings.killstreaks = next;
                settings.touch();
                dvars.set("ui_killstreak_status", "Saved. Changes apply immediately.");
            } else {
                dvars.set(
                    "ui_killstreak_status",
                    "Choose three rewards with different kill requirements.",
                );
            }
        }
    }
    let choices: Vec<_> = rows
        .iter()
        .map(|(name, cost)| {
            let label = match name.as_str() {
                "airdrop" => "Care Package".into(),
                "airdrop_sentry_minigun" => "Sentry Gun".into(),
                "airdrop_mega" => "Emergency Airdrop".into(),
                "helicopter_flares" => "Pave Low".into(),
                "helicopter_minigun" => "Chopper Gunner".into(),
                "stealth_airstrike" => "Stealth Bomber".into(),
                "nuke" => "Tactical Nuke".into(),
                _ => name.replace('_', " ").to_uppercase(),
            };
            (format!("{label} ({cost} kills)"), name.clone())
        })
        .collect();
    let slot_choices: [Vec<(String, String)>; 3] = std::array::from_fn(|slot| {
        let blocked: Vec<_> = settings
            .killstreaks
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != slot)
            .filter_map(|(_, name)| {
                rows.iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, cost)| *cost)
            })
            .collect();
        choices
            .iter()
            .filter(|(_, name)| {
                rows.iter()
                    .find(|(key, _)| key == name)
                    .is_some_and(|(_, cost)| !blocked.contains(cost))
            })
            .cloned()
            .collect()
    });
    let changed = catalog.get("options_killstreaks").is_some_and(|menu| {
        menu.items.iter().any(|item| {
            item.dvar
                .strip_prefix("ui_killstreak_")
                .and_then(|slot| slot.parse::<usize>().ok())
                .is_some_and(|slot| slot < 3 && item.choices != slot_choices[slot])
        })
    });
    if changed && let Some(menu) = catalog.menus.get_mut("options_killstreaks") {
        for item in &mut menu.items {
            if let Some(slot) = item
                .dvar
                .strip_prefix("ui_killstreak_")
                .and_then(|slot| slot.parse::<usize>().ok())
                .filter(|&slot| slot < 3)
            {
                item.choices = slot_choices[slot].clone();
            }
        }
    }
    for (slot, name) in settings.killstreaks.iter().enumerate() {
        dvars.set(&format!("ui_killstreak_{slot}"), name.clone());
    }
    dvars.set(
        "ui_minecraft_all_killstreaks",
        if settings.minecraft_all_killstreaks {
            "1"
        } else {
            "0"
        },
    );
}

pub(crate) fn sync(
    settings: Res<frame::GameSettings>,
    catalog: Res<asset_game::MenuCatalog>,
    generation: Res<frame::WorldGeneration>,
    screen: Res<frame::AppScreen>,
    role: Res<frame::RuntimeRole>,
    time: Res<Time>,
    presented: Res<net::PresentedSnapshot>,
    local: Option<Res<net::LocalPresentClient>>,
    mut inbox: Option<ResMut<net::ClientActionInbox>>,
    mut ids: ResMut<net::ActionRequestIds>,
    mut sent: Local<Option<(frame::WorldGeneration, sim::ClientId, String)>>,
    mut sent_at: Local<f32>,
) {
    if *role == frame::RuntimeRole::Replay
        || !matches!(
            *screen,
            frame::AppScreen::InGame | frame::AppScreen::ClassSelect
        )
    {
        *sent = None;
        return;
    }
    let (Some(local), Some(inbox)) = (local, inbox.as_deref_mut()) else {
        return;
    };
    let payload = format!(
        "{}|{}",
        u8::from(settings.minecraft_all_killstreaks),
        settings.killstreaks.join("|")
    );
    let rows = rewards(&catalog);
    let Some(indices) = settings
        .killstreaks
        .iter()
        .map(|name| rows.iter().position(|(key, _)| key == name))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let response_payload = format!(
        "{}|{}",
        u8::from(settings.minecraft_all_killstreaks),
        indices
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join("|")
    );
    let next = (*generation, local.0, payload.clone());
    let acknowledged = presented
        .snapshot()
        .and_then(|snapshot| snapshot.meta.for_client(local.0))
        .is_some_and(|meta| {
            meta.client_dvars.iter().any(|(key, value)| {
                key == sim::script::killstreaks::LOADOUT_DVAR && value == &payload
            })
        });
    if acknowledged || (sent.as_ref() == Some(&next) && time.elapsed_secs() - *sent_at < 0.75) {
        return;
    }
    let (Some(menu), Some(response)) = (
        sim::menu_response_field(sim::script::killstreaks::LOADOUT_MENU),
        sim::menu_response_field(&response_payload),
    ) else {
        return;
    };
    if inbox
        .push(
            local.0,
            sim::ClientAction::MenuResponse {
                request_id: ids.allocate(),
                menu,
                response,
            },
        )
        .is_ok()
    {
        *sent = Some(next);
        *sent_at = time.elapsed_secs();
    }
}
