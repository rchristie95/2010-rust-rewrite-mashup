#![windows_subsystem = "windows"]

use std::{
    env, fs,
    path::Path,
    process::{Command, ExitCode},
};

#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(
        window: *mut std::ffi::c_void,
        text: *const u16,
        title: *const u16,
        flags: u32,
    ) -> i32;
}

const COMMANDS: &str = "set ui_minecraft_all_killstreaks 1; spawn 0; bot add 6; force_match_start";

fn run() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let root = exe.parent().ok_or("Cannot locate the game folder")?;
    for file in [
        "iw4l.exe",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
        "skate-data/assets/private/skater.glb",
        "iw4l-artifacts/minecraft-26.3/.complete",
    ] {
        if !root.join(file).is_file() {
            return Err(format!(
                "Missing {file}. Extract the entire ZIP before running Play.exe."
            ));
        }
    }
    fs::write(
        root.join(".env"),
        "IW4L_GAMES=\"games/mw2\"\nIW4L_SKATE_ASSETS=\"skate-data/assets\"\n",
    )
    .map_err(|e| e.to_string())?;
    if env::args().any(|arg| arg == "--check") {
        fs::write(root.join("launcher-check.txt"), format!("PASS: native launcher; no helper, driver or installer.\nMap: minecraft:overworld\nCommands: {COMMANDS}\n")).map_err(|e| e.to_string())?;
        return Ok(());
    }
    Command::new(root.join("iw4l.exe"))
        .args(["map", "minecraft:overworld", "--cmds", COMMANDS])
        .current_dir(root)
        .env("IW4L_GAMES", root.join("games/mw2"))
        .env("IW4L_SKATE_ASSETS", root.join("skate-data/assets"))
        .env(
            "MINECRAFTOSS_ROOT",
            root.join("iw4l-artifacts/minecraft-26.3"),
        )
        .env(
            "IW4L_SETTINGS_PATH",
            root.join("iw4l-artifacts/settings.cfg"),
        )
        .env("IW4L_ACCOUNT_PATH", root.join("iw4l-artifacts/account.dat"))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let text: Vec<u16> = error.encode_utf16().chain(Some(0)).collect();
            let title: Vec<u16> = "MW2 Skate Minecraft"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            if env::args().any(|arg| arg == "--check") {
                let path = env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(Path::to_owned));
                if let Some(path) = path {
                    let _ = fs::write(path.join("launcher-error.txt"), error);
                }
            } else {
                unsafe {
                    MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10);
                }
            }
            ExitCode::FAILURE
        }
    }
}
