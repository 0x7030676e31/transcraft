# Transcraft

A small Rust desktop launcher for **Minecraft Java 26.3 + Fabric**, built for a PC with no network connection. Supports Windows 10/11 x86-64 and Linux x86-64, including Arch Linux. Uses offline nicknames; no Microsoft login, telemetry, update checker, or runtime downloader. Skins are a future feature.

The GUI lets you choose a nickname, memory allocation, window dimensions, fullscreen, and game folder. Includes Sodium 0.9.3, Lithium 0.26.4, Mod Menu 21.0.1, and pinned Fabric API dependencies. Exact upstream downloads and checksums are in `bundle.lock.json`.

## Build a fully offline executable

**Do this on an online build machine.** You need stable Rust, Python 3.12+, and the platform's C/C++ build tools. The offline PC needs only the resulting executable, normal graphics drivers, and a writable user profile. Java is bundled. No installation or administrator privileges are needed to run it.

Windows (PowerShell, with Visual Studio Build Tools' C++ workload):

```powershell
python tools/bundle.py prepare --platform windows
cargo build --release --locked
python tools/bundle.py pack --binary target/release/transcraft.exe --payload build/windows/payload.tar.gz --output dist/Transcraft-0.1.1-windows-x86_64.exe
```

Linux (Arch build prerequisites: `base-devel pkgconf libxkbcommon wayland libx11 libxcursor libxi libxrandr mesa`):

```sh
python3 tools/bundle.py prepare --platform linux
cargo build --release --locked
python3 tools/bundle.py pack --binary target/release/transcraft --payload build/linux/payload.tar.gz --output dist/Transcraft-0.1.1-linux-x86_64
```

Copy the file in `dist/` to the offline PC and open it. Linux may need `chmod +x Transcraft-0.1.1-linux-x86_64` after copying. Windows and Linux require separate executables. The adjacent `.sha256` file is an optional transfer-integrity check, not a runtime dependency.

The packer appends a gzip-compressed archive to the native executable. First Play extracts Java 25, the game, libraries, assets, and mods in the background and verifies their checksums. Subsequent launches reuse that installation. Allow roughly 2 GiB of free disk space for extraction, plus space for worlds. Compression is already applied to the payload; UPX is unnecessary and must not be run after packing because it can discard the appended bundle. Builds without packing open the GUI but clearly report that they cannot launch Minecraft offline.

For Unicode folders, the builder patches the bundled Windows Java launchers' embedded process manifests to opt into UTF-8 on Windows 10 1903+ and Windows 11. This follows [Bazel's JDK packaging approach](https://bazel.googlesource.com/bazel/+/901cf95651c88ef796ebee17b36885c700c52633/src/minimize_jdk.sh), preserves `asInvoker`, and requires no system locale changes. Original runtime archives are hash-verified before modification; the patched files receive new bundle checksums. On earlier Windows 10 builds, use an ASCII data and game folder.

## Data and offline behavior

Default storage:

| System | Data folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%\Transcraft\data` |
| Linux | `${XDG_DATA_HOME:-$HOME/.local/share}/transcraft` |

The displayed **More options → Data** path is authoritative. Inside it, `settings.json` stores launcher options, `bundles/` stores immutable extracted game/runtime bundles, and `game/` stores saves, mods, options, screenshots, and logs. Changing the game folder moves future launches to that folder; it does not move existing saves. Updating the executable keeps the same settings and game folder. Old bundle folders can be removed when Minecraft is closed to reclaim disk space.

Use `--data-dir` for an explicit writable storage location, including a portable drive:

```powershell
.\Transcraft-0.1.1-windows-x86_64.exe --data-dir "D:\Games\Transcraft data"
```

```sh
./Transcraft-0.1.1-linux-x86_64 --data-dir "$HOME/Games/Transcraft data"
```

Nicknames must contain 3–16 letters, numbers, or underscores. The offline UUID follows Minecraft's `OfflinePlayer:<nickname>` convention. **Changing nickname changes player identity**, which affects inventory and player data in existing worlds. Servers that require Microsoft authentication and Realms are unavailable with these profiles. Normal single-player does not require a network adapter or IP address. Minecraft/mods may attempt their own online services and time out; the launcher itself performs no HTTP requests. Skins would require a separate offline skin mod and local image handling.

Managed mods are copied into the selected game's `mods/` folder on launch. New bundle versions remove only previously managed filenames; your other mods remain. Additional mods must be installed manually and may conflict. **Repair bundled files** re-extracts the payload without deleting saves, game options, or additional mods. It takes effect on the next launch for managed mods.

For diagnostics, read `game/logs/transcraft-java.log`, Minecraft's `game/logs/latest.log`, and `game/crash-reports/`. Startup failures are written to `launcher-error.log` in the data folder. These CLI switches also work without opening a window:

```sh
./Transcraft-0.1.1-linux-x86_64 --install --data-dir /path/to/data
./Transcraft-0.1.1-linux-x86_64 --verify --data-dir /path/to/data
./Transcraft-0.1.1-linux-x86_64 --launch --data-dir /path/to/data
```

`--verify` checks every extracted file against the bundled receipt. The executable's SHA-256 footer detects accidental payload corruption; it is not a publisher signature. The Windows GUI subsystem suppresses terminal output; use log files and the command's exit code there. The launcher stays open while the game runs to hold the instance lock.

## Development and releases

Builds use stable Rust. Formatting uses nightly rustfmt for the import grouping settings in `rustfmt.toml`. Install it with `rustup toolchain install nightly --profile minimal --component rustfmt`, and format with `cargo +nightly fmt --all`.

```sh
cargo +nightly fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s tests -v
```

The checked-in lockfiles make normal builds use pinned Rust dependencies and game downloads. `prepare` downloads verified files into an ignored cache; it never resolves newer versions. To deliberately update game dependencies, put the original three supplied mod JARs in the root (or `mods/`) and run `python3 tools/bundle.py resolve`; review and commit the changed lockfile. `resolve` selects stable Fabric Loader, a release Fabric API if available (otherwise a compatible prerelease), and the current Java 25 Temurin JRE. The three supplied mod versions stay fixed by their exact file hashes. Mod JARs are fetched by hash from Modrinth during normal builds and do not need to be committed.

To reduce downloads, `prepare` also accepts `--minecraft-cache /path/to/.minecraft`. Only files matching the pinned checksums are reused; existing saves and settings are never read or changed.

CI runs formatting, Clippy, and tests on Linux and Windows. Releases follow **SemVer**: update `Cargo.toml` and the root package in `Cargo.lock`, commit, then push a matching tag such as `v0.1.1` or `v0.2.0-beta.1`. The tag workflow validates that it matches the package version, builds both launchers, creates SHA-256 files, and publishes a GitHub release automatically. It does not change versions or create tags for you. No contribution templates, issue templates, or community-management files are included.

GitHub release binaries are explicitly **launcher-only**; package one with `tools/bundle.py pack` and a locally prepared payload to make it fully offline. Game/runtime bundles, caches, and local distribution files are ignored by Git. This keeps Mojang game files out of the repository and public releases. Minecraft's [EULA](https://www.minecraft.net/en-us/eula) restricts redistribution of the game; make personal bundles for your own licensed use. The launcher source is MIT-licensed; that license does not cover bundled Minecraft or third-party dependencies. Java licensing/notices remain in `runtime/`, and mods retain their notices inside their JARs. Not affiliated with Mojang, Microsoft, or Fabric.

Linux release launchers are built on Ubuntu 22.04 for glibc compatibility. Linux still uses the host's desktop libraries and graphics drivers; this is not a fully static Linux application. Windows releases statically link the Rust/C runtime and use Windows' system graphics libraries. A real Windows game launch should be checked on the destination hardware before relying on it.
