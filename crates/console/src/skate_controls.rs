use bevy::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

const ACTIONS: [&str; 26] = [
    "TOGGLE", "LUP", "LDOWN", "LLEFT", "LRIGHT", "RDOWN", "RUP", "RLEFT", "RRIGHT", "A", "B", "X",
    "LT", "RT", "LB", "RB", "Y", "START", "BACK", "OLLIE", "UP", "DOWN", "LEFT", "RIGHT", "LTB",
    "RTB",
];
const DEFAULTS: [&str; 26] = [
    "NumLock",
    "NumPad8",
    "NumPad2",
    "NumPad4",
    "NumPad6",
    "NumPad1",
    "NumEnter+NumPad3",
    "NumPad7",
    "NumPad9",
    "NumPad0",
    "NumPad5",
    "None",
    "Divide",
    "Multiply",
    "Subtract",
    "Add",
    "NumEnter+NumPad5",
    "NumEnter+Divide",
    "NumEnter+Multiply",
    "Decimal",
    "NumEnter+NumPad8",
    "NumEnter+NumPad2",
    "NumEnter+NumPad4",
    "NumEnter+NumPad6",
    "NumEnter+Subtract",
    "NumEnter+Add",
];
const KEYS: [&str; 32] = [
    "None",
    "NumLock",
    "NumPad0",
    "NumPad1",
    "NumPad2",
    "NumPad3",
    "NumPad4",
    "NumPad5",
    "NumPad6",
    "NumPad7",
    "NumPad8",
    "NumPad9",
    "Decimal",
    "Divide",
    "Multiply",
    "Subtract",
    "Add",
    "NumEnter+NumPad0",
    "NumEnter+NumPad1",
    "NumEnter+NumPad2",
    "NumEnter+NumPad3",
    "NumEnter+NumPad4",
    "NumEnter+NumPad5",
    "NumEnter+NumPad6",
    "NumEnter+NumPad7",
    "NumEnter+NumPad8",
    "NumEnter+NumPad9",
    "NumEnter+Decimal",
    "NumEnter+Divide",
    "NumEnter+Multiply",
    "NumEnter+Subtract",
    "NumEnter+Add",
];
type Bindings = [String; 26];

#[derive(Default)]
pub(crate) struct KeyboardState {
    held: HashMap<KeyCode, String>,
    ollie: bool,
    flick_until: f32,
}

pub(crate) fn keyboard_input(
    keys: &ButtonInput<KeyCode>,
    dvars: &frame::UiMenuDvars,
    state: &mut KeyboardState,
    now: f32,
    blocked: bool,
    skating: bool,
) -> (bool, frame::skate::SkateKeyboardInput) {
    let mut input = frame::skate::SkateKeyboardInput::default();
    if blocked {
        state.held.clear();
        state.ollie = false;
        state.flick_until = 0.0;
        return (false, input);
    }
    state.held.retain(|key, _| keys.pressed(*key));
    for key in keys.get_pressed() {
        if let Some(chord) = captured_key(*key, keys.pressed(KeyCode::NumpadEnter)) {
            state.held.entry(*key).or_insert(chord);
        }
    }
    let binding = |index: usize| {
        dvars
            .get(&format!("ui_skate_{}", ACTIONS[index]))
            .unwrap_or(DEFAULTS[index])
            .to_owned()
    };
    let toggle = state
        .held
        .iter()
        .any(|(key, chord)| keys.just_pressed(*key) && *chord == binding(0));
    if !skating {
        state.ollie = false;
        state.flick_until = 0.0;
        return (toggle, input);
    }
    let down = |index: usize| state.held.values().any(|chord| *chord == binding(index));
    let axis = |positive, negative| (i16::from(down(positive)) - i16::from(down(negative))) * 30000;
    input.left = [axis(4, 3), axis(1, 2)];
    input.right = [axis(8, 7), axis(6, 5)];
    input.triggers = [
        if down(12) { 255 } else { 0 },
        if down(13) { 255 } else { 0 },
    ];
    for (index, bit) in [
        (9, 0x1000),
        (10, 0x2000),
        (11, 0x4000),
        (14, 0x0100),
        (15, 0x0200),
        (16, 0x8000),
        (17, 0x0010),
        (18, 0x0020),
        (20, 0x0001),
        (21, 0x0002),
        (22, 0x0004),
        (23, 0x0008),
        (24, 0x0040),
        (25, 0x0080),
    ] {
        if down(index) {
            input.buttons |= bit;
        }
    }
    let ollie = down(19);
    if state.ollie && !ollie {
        state.flick_until = now + 0.12;
    }
    state.ollie = ollie;
    if ollie {
        input.right = [0, -30000];
    } else if now < state.flick_until {
        input.right = [0, 30000];
    }
    input.engaged = input.buttons != 0
        || input.triggers != [0; 2]
        || input.left != [0; 2]
        || input.right != [0; 2];
    (toggle, input)
}

#[derive(Default)]
pub(crate) struct SkateControlsState {
    bindings: Option<Bindings>,
    writable: bool,
    context: Option<u8>,
    last_heartbeat: f32,
}

fn defaults() -> Bindings {
    DEFAULTS.map(str::to_owned)
}

fn validate(bindings: &Bindings) -> Result<(), String> {
    if bindings[0] == "None" {
        return Err("Choose a skating toggle key.".into());
    }
    let mut used = HashSet::new();
    for key in bindings {
        if !KEYS.contains(&key.as_str()) {
            return Err(format!("Unsupported skate key: {key}"));
        }
        if key != "None" && !used.insert(key) {
            return Err(format!("Duplicate skate key: {key}"));
        }
    }
    Ok(())
}

fn parse(source: &str) -> Result<Bindings, String> {
    let mut bindings = defaults();
    let mut assigned = [false; ACTIONS.len()];
    let mut section = "";
    for line in source.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if section == "unassigned" && value == "true" {
            if let Some(index) = ACTIONS.iter().position(|known| *known == key) {
                bindings[index] = "None".into();
                assigned[index] = true;
            }
            continue;
        }
        let action = if section == "config" && value == "skateToggle" {
            "TOGGLE"
        } else if section == "pad1" {
            value
        } else {
            continue;
        };
        if let Some(index) = ACTIONS.iter().position(|known| *known == action) {
            bindings[index] = key.to_owned();
            assigned[index] = true;
        }
    }
    // The previous mapper kept Num Lock in code rather than in the INI.
    if bindings[0] == "None" {
        bindings[0] = "NumLock".into();
    }
    for index in 1..bindings.len() {
        if !assigned[index]
            && bindings[index] != "None"
            && (0..bindings.len()).any(|other| {
                other != index && assigned[other] && bindings[other] == bindings[index]
            })
        {
            bindings[index] = "None".into();
        }
    }
    validate(&bindings)?;
    Ok(bindings)
}

fn serialize(bindings: &Bindings) -> Result<String, String> {
    validate(bindings)?;
    let mut text = format!(
        "[startup]\nenabled = true\n[config]\n{} = skateToggle\n[pad1]\n",
        bindings[0]
    );
    for (action, key) in ACTIONS.iter().zip(bindings).skip(1) {
        if key != "None" {
            text.push_str(&format!("{key} = {action}\n"));
        }
    }
    text.push_str("[unassigned]\n");
    for (action, key) in ACTIONS.iter().zip(bindings).skip(1) {
        if key == "None" {
            text.push_str(&format!("{action} = true\n"));
        }
    }
    Ok(text)
}

fn publish(dvars: &mut frame::UiMenuDvars, bindings: &Bindings) {
    for (action, key) in ACTIONS.iter().zip(bindings) {
        dvars.set(&format!("ui_skate_{action}"), key);
        dvars.set(&format!("ui_bind_ui_skate_{action}"), key_label(key));
    }
}

fn key_label(key: &str) -> String {
    if let Some(key) = key.strip_prefix("NumEnter+") {
        return format!("Num Enter + {}", key_label(key));
    }
    match key {
        "None" => "Unassigned".into(),
        "NumLock" => "Num Lock".into(),
        "Decimal" => "Num Del / .".into(),
        "Divide" => "Num /".into(),
        "Multiply" => "Num *".into(),
        "Subtract" => "Num -".into(),
        "Add" => "Num +".into(),
        _ => key.replace("NumPad", "Num "),
    }
}

fn captured_key(key: KeyCode, layer: bool) -> Option<String> {
    let key = match key {
        KeyCode::NumLock => return Some("NumLock".into()),
        KeyCode::Numpad0 => "NumPad0",
        KeyCode::Numpad1 => "NumPad1",
        KeyCode::Numpad2 => "NumPad2",
        KeyCode::Numpad3 => "NumPad3",
        KeyCode::Numpad4 => "NumPad4",
        KeyCode::Numpad5 => "NumPad5",
        KeyCode::Numpad6 => "NumPad6",
        KeyCode::Numpad7 => "NumPad7",
        KeyCode::Numpad8 => "NumPad8",
        KeyCode::Numpad9 => "NumPad9",
        KeyCode::NumpadDecimal => "Decimal",
        KeyCode::NumpadDivide => "Divide",
        KeyCode::NumpadMultiply => "Multiply",
        KeyCode::NumpadSubtract => "Subtract",
        KeyCode::NumpadAdd => "Add",
        _ => return None,
    };
    Some(if layer {
        format!("NumEnter+{key}")
    } else {
        key.into()
    })
}

pub(crate) fn capture_binding(
    keys: &ButtonInput<KeyCode>,
    capture: &mut frame::UiBindingCapture,
    dvars: &mut frame::UiMenuDvars,
) {
    if keys.just_pressed(KeyCode::Escape) {
        capture.command = None;
        capture.consumed_input = true;
        dvars.set("ui_skate_status", "Cancelled. Binding unchanged.");
        return;
    }
    let Some(command) = capture.command.clone() else {
        return;
    };
    if keys.just_pressed(KeyCode::Backspace) {
        capture.consumed_input = true;
        if command == "ui_skate_TOGGLE" {
            dvars.set(
                "ui_skate_status",
                "The skateboard toggle needs a key. Esc cancels.",
            );
            return;
        }
        dvars.set(&command, "None");
        capture.command = None;
        return;
    }
    let layer = keys.pressed(KeyCode::NumpadEnter);
    if let Some(key) = keys
        .get_just_pressed()
        .find_map(|key| captured_key(*key, layer))
    {
        dvars.set(&command, key);
        capture.command = None;
        capture.consumed_input = true;
    } else if keys
        .get_just_pressed()
        .any(|key| *key != KeyCode::NumpadEnter)
    {
        capture.consumed_input = true;
        dvars.set(
            "ui_skate_status",
            "Use the separate numpad; PC keys stay reserved for MW2.",
        );
    }
}

pub(crate) fn native_menu_skate_controls(
    mut dvars: ResMut<frame::UiMenuDvars>,
    mut state: Local<SkateControlsState>,
    mode: Res<frame::SkateMode>,
    time: Res<Time>,
) {
    // The mapper only sends gameplay reports while skating and outside menus.
    let context = if mode.input_blocked {
        2
    } else if mode.active {
        1
    } else {
        0
    };
    if state.context != Some(context) || time.elapsed_secs() - state.last_heartbeat > 0.25 {
        let write_context = || -> std::io::Result<()> {
            fs::write(
                "keyboard/skate-context.new",
                format!("{}:{context}", std::process::id()),
            )?;
            fs::rename("keyboard/skate-context.new", "keyboard/skate-context.txt")
        };
        if write_context().is_ok() {
            state.context = Some(context);
            state.last_heartbeat = time.elapsed_secs();
        }
    }
    let path = Path::new("keyboard/mapping.ini");
    if state.bindings.is_none() {
        if !path.exists() && fs::create_dir_all("keyboard").is_ok() {
            if let Ok(text) = serialize(&defaults()) {
                let _ = fs::write(path, text);
            }
        }
        let loaded = fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|source| parse(&source));
        let bindings = match loaded {
            Ok(bindings) => {
                // Migrate the earlier partial controller list, preserving assigned keys.
                state.writable = serialize(&bindings)
                    .ok()
                    .is_some_and(|text| fs::write(path, text).is_ok());
                bindings
            }
            Err(error) => {
                dvars.set("ui_skate_status", format!("Controls unavailable: {error}"));
                defaults()
            }
        };
        if state.writable {
            dvars.set("ui_skate_status", "Changes save and apply automatically.");
        }
        publish(&mut dvars, &bindings);
        dvars.set("ui_skate_reset", "0");
        state.bindings = Some(bindings);
        return;
    }
    let previous = state.bindings.as_ref().expect("initialized").clone();
    let reset = dvars.get("ui_skate_reset") == Some("1");
    let mut next = if reset {
        defaults()
    } else {
        std::array::from_fn(|i| {
            dvars
                .get(&format!("ui_skate_{}", ACTIONS[i]))
                .unwrap_or(&previous[i])
                .to_owned()
        })
    };
    dvars.set("ui_skate_reset", "0");
    if next == previous {
        return;
    }
    // Selecting an assigned key swaps the two bindings, so one key never sends two actions.
    if !reset {
        if let Some(changed) = next.iter().zip(&previous).position(|(a, b)| a != b) {
            if next[changed] != "None" {
                if let Some(other) =
                    (0..next.len()).find(|&i| i != changed && next[i] == next[changed])
                {
                    next[other] = previous[changed].clone();
                }
            }
        }
    }
    let saved = (|| -> Result<(), String> {
        if !state.writable {
            return Err(
                "The existing controls file could not be read; it has been preserved.".into(),
            );
        }
        let text = serialize(&next)?;
        // Write atomically so the running mapper only ever sees a complete configuration.
        let temporary = path.with_extension("ini.new");
        fs::write(&temporary, text).map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    match saved {
        Ok(()) => {
            publish(&mut dvars, &next);
            state.bindings = Some(next);
            dvars.set("ui_skate_status", "Saved. Controls apply immediately.");
        }
        Err(error) => {
            publish(&mut dvars, &previous);
            dvars.set("ui_skate_status", format!("Not saved: {error}"));
        }
    }
}
