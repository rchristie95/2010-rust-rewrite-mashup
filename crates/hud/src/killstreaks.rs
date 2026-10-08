use asset_game::MenuCatalog;
use frame::HudInputView;

use crate::draw2d::{Draw2dCmd, Draw2dList, Draw2dOp, Draw2dProvenance, TEXT_STYLE_HUDELEM};
use crate::images::HUD_CHROME_NAMESPACE;
use crate::surface::Hud2dSurface;

pub(crate) fn title(name: &str) -> String {
    match name {
        "uav" => "UAV".into(),
        "counter_uav" => "COUNTER-UAV".into(),
        "airdrop" => "CARE PACKAGE".into(),
        "airdrop_mega" => "EMERGENCY AIRDROP".into(),
        "helicopter" => "ATTACK HELICOPTER".into(),
        "helicopter_flares" => "PAVE LOW".into(),
        "helicopter_minigun" => "CHOPPER GUNNER".into(),
        _ => name.replace('_', " ").to_uppercase(),
    }
}

pub(crate) fn paint(
    list: &mut Draw2dList,
    input: &HudInputView,
    catalog: &MenuCatalog,
    surface: &Hud2dSurface,
) {
    if !input.killstreak_shortcuts
        || input.owned_killstreaks.is_empty()
        || input.console_open
        || input.script_menu_open
    {
        return;
    }
    let font = "fonts/smallfont";
    let Some(def) = catalog.font(font) else {
        return;
    };
    let scale =
        hud_iw4::normalized_text_scale(def.pixel_height, 12.0 / hud_iw4::ui_text_height(1.0));
    let page = input.killstreak_page;
    for (index, name) in input
        .owned_killstreaks
        .iter()
        .skip(page * 9)
        .take(9)
        .enumerate()
    {
        if name.is_empty() {
            continue;
        }
        let rect = surface.apply_rect(24.0, 224.0 + index as f32 * 18.0, scale, scale, 1, 1);
        list.cmds.push(Draw2dCmd {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            s0: 0.0,
            t0: 0.0,
            s1: 1.0,
            t1: 1.0,
            color: [1.0, 1.0, 0.65, 1.0],
            material: def.material.clone(),
            material_namespace: HUD_CHROME_NAMESPACE,
            op: Draw2dOp::TextRun {
                font: font.into(),
                scale,
                text: format!("[CTRL+{}]  {}", index + 1, title(name)),
                loc_key: String::new(),
                style: TEXT_STYLE_HUDELEM,
                fx: None,
                glow: None,
            },
            provenance: Draw2dProvenance::CgDraw {
                site: "killstreak_shortcuts",
            },
            layer: 1,
        });
    }
    if input
        .owned_killstreaks
        .iter()
        .enumerate()
        .any(|(index, name)| !name.is_empty() && index / 9 != page)
    {
        let rect = surface.apply_rect(24.0, 206.0, scale, scale, 1, 1);
        list.cmds.push(Draw2dCmd {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            s0: 0.0,
            t0: 0.0,
            s1: 1.0,
            t1: 1.0,
            color: [1.0, 1.0, 1.0, 1.0],
            material: def.material.clone(),
            material_namespace: HUD_CHROME_NAMESPACE,
            op: Draw2dOp::TextRun {
                font: font.into(),
                scale,
                text: format!(
                    "[CTRL+0] PAGE {}/{}",
                    page + 1,
                    input.owned_killstreaks.len().div_ceil(9)
                ),
                loc_key: String::new(),
                style: TEXT_STYLE_HUDELEM,
                fx: None,
                glow: None,
            },
            provenance: Draw2dProvenance::CgDraw {
                site: "killstreak_shortcuts",
            },
            layer: 1,
        });
    }
}
