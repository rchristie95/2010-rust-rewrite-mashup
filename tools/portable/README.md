# Native Windows portable launcher

`Play.exe` starts Minecraft with six bots and sequential earned killstreaks. It grants no rewards at startup. Numpad skating runs inside the game, so this launcher needs no keyboard mapper, ViGEmBus driver, or .NET runtime.

Build on Windows with the Rust MSVC toolchain:

```powershell
New-Item -ItemType Directory -Force tools/portable/bin
rustc --edition 2024 -C opt-level=2 -C target-feature=+crt-static tools/portable/play.rs -o tools/portable/bin/Play.exe
rustc --edition 2024 --cfg online_menu -C opt-level=2 -C target-feature=+crt-static tools/portable/play.rs -o tools/portable/bin/Multiplayer.exe
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

For multiplayer, double-click `Multiplayer.exe`, or use End Game to return to the menu, then Create Online Game or Find Lobbies. `Play.exe --menu` also opens that menu directly. Players must use matching builds and a descriptor for the same relay, as described in [MASTER.md](../../docs/MASTER.md). A descriptor with `[updates] auto_update=false` keeps the packaged build and still verifies the relay certificate.

To run the relay on Windows, build `cargo build -p iw4l-master --profile play --locked`. Place the resulting executable, `server.ps1`, and the provisioned `server-cert.pem`, `server-key.pem`, and `ca.pem` in a separate private server folder. Run `./server.ps1` to start it in the background, `./server.ps1 -Action Status` to check it, or `./server.ps1 -Action Stop` to stop it. Keep the private key and server folder out of player ZIPs; only the public CA belongs in client descriptors.

Internet hosting requires inbound UDP 4433 to reach this PC. Forward that port on the router and, from an administrator PowerShell, use `./server.ps1 -Action AllowFirewall` for the narrow executable/port rule. A VPN or carrier NAT can prevent inbound connections even when local tests pass. Use the host's reachable address in players' descriptors; `127.0.0.1` works only on the host itself. Each player should get a fresh `account.dat`, so omit that file when preparing the ZIP.

When router access is unavailable, install [Tailscale](https://tailscale.com/download/windows) on the host and each player's PC, sign in, and give those players access to the host device. Run `./server.ps1 -Action ExportCommunity -ClientDirectory 'C:/path/to/player/game'` on the host to write `friends.iw4l-server` using its Tailscale IPv4 address. Copy that public descriptor into each extracted game folder. For a forwarded address or LAN address, supply `-RelayAddress host.example.com` or an IPv4 address. This exports only the public CA; it does not copy server keys. Verify a lobby join from the other PC before assuming the network path is working. See [Tailscale's game server guide](https://tailscale.com/docs/use-cases/personal-or-at-home-use/share-private-game-server).
