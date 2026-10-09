# Native Windows portable launcher

`Play.exe` starts Minecraft with six bots and sequential earned killstreaks. It grants no rewards at startup. Numpad skating runs inside the game, so this launcher needs no keyboard mapper, ViGEmBus driver, or .NET runtime.

Build on Windows with the Rust MSVC toolchain:

```powershell
New-Item -ItemType Directory -Force tools/portable/bin
rustc --edition 2024 -C opt-level=2 -C target-feature=+crt-static tools/portable/play.rs -o tools/portable/bin/Play.exe
```

Prepare a writable folder with this layout, using your own game files:

```text
Play.exe
iw4l.exe
vcruntime140.dll
vcruntime140_1.dll
games/mw2/main/*.iwd
games/mw2/zone/...
skate-data/assets/...
iw4l-artifacts/minecraft-26.3/...
```

Build `iw4l.exe` using `cargo build -p launcher --profile play --locked`. For this MSVC build, the two runtime DLLs can be supplied beside the executable from the licensed Visual Studio x64 CRT redistribution directory. `Play.exe` itself links its CRT statically. Windows 10/11 x64 and a supported GPU driver are required.

The converted Skate assets and completed Minecraft resource folder come from the game's normal setup. Preserve `.complete` inside the Minecraft folder to avoid repeating its download. MW2 campaign videos and generated caches are unnecessary for this launch; caches rebuild on first use. Game files, runtime DLLs, and prepared bundles are not included in this repository.

The launcher anchors paths at its own directory and writes `.env` for this folder layout. Double-click `Play.exe` after extracting the entire folder. Run `Play.exe --check` to check the required launcher files without starting the game; it writes `launcher-check.txt` or `launcher-error.txt`. This check does not validate every game archive or GPU support.
