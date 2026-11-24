use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use md5::{Digest, Md5};

use crate::{
    bundle::{self, Manifest},
    config::Settings,
};

pub fn offline_uuid(name: &str) -> String {
    let mut bytes: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}")).into();
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}

// Java's argument-file grammar, rather than shell quoting. This avoids Windows'
// command-line limit even with hundreds of library paths and spaces in usernames.
pub fn quote_arg(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}

pub fn command_args(
    m: &Manifest,
    settings: &Settings,
    dir: &Path,
    game: &Path,
    natives: &Path,
) -> Result<Vec<String>> {
    settings.validate()?;
    let path = |p: &Path| -> Result<String> {
        Ok(p.to_str()
            .context("Game paths must be valid Unicode")?
            .to_owned())
    };
    let classpath = m
        .classpath
        .iter()
        .map(|p| path(&dir.join(p)))
        .collect::<Result<Vec<_>>>()?
        .join(if cfg!(windows) { ";" } else { ":" });
    if !cfg!(windows) {
        ensure!(
            !dir.to_string_lossy().contains(':'),
            "On Linux the data folder must not contain a colon (Java classpath separator)."
        );
    }
    let vars: BTreeMap<&str, String> = BTreeMap::from([
        ("auth_player_name", settings.nickname.clone()),
        ("auth_uuid", offline_uuid(&settings.nickname)),
        ("auth_access_token", "0".into()),
        ("clientid", "".into()),
        ("auth_xuid", "".into()),
        ("user_type", "legacy".into()),
        ("user_properties", "{}".into()),
        (
            "version_name",
            format!("fabric-loader-{}-{}", m.fabric, m.minecraft),
        ),
        ("version_type", "release".into()),
        ("game_directory", path(game)?),
        ("assets_root", path(&dir.join("assets"))?),
        ("assets_index_name", m.asset_index.clone()),
        ("natives_directory", path(natives)?),
        ("launcher_name", "Transcraft".into()),
        ("launcher_version", env!("CARGO_PKG_VERSION").into()),
        ("classpath", classpath),
        (
            "classpath_separator",
            if cfg!(windows) { ";" } else { ":" }.into(),
        ),
        ("library_directory", path(&dir.join("libraries"))?),
        ("bundle_directory", path(dir)?),
        ("resolution_width", settings.width.to_string()),
        ("resolution_height", settings.height.to_string()),
    ]);
    let expand = |s: &str| -> Result<String> {
        let mut value = s.to_owned();
        for (key, replacement) in &vars {
            value = value.replace(&format!("${{{key}}}"), replacement);
        }
        ensure!(!value.contains("${"), "Unresolved launch argument: {value}");
        Ok(value)
    };
    let mut args = vec![
        "-Xms512M".into(),
        format!("-Xmx{}M", settings.memory_mib),
        "-XX:+UseG1GC".into(),
    ];
    for arg in &m.jvm_args {
        args.push(expand(arg)?);
    }
    args.push(m.main_class.clone());
    for arg in &m.game_args {
        args.push(expand(arg)?);
    }
    if settings.fullscreen {
        args.push("--fullscreen".into());
    }
    Ok(args)
}

fn install_mods(m: &Manifest, dir: &Path, game: &Path) -> Result<()> {
    let mods = game.join("mods");
    fs::create_dir_all(&mods)?;
    let managed_path = game.join(".transcraft-mods.json");
    let old: Vec<String> = if managed_path.exists() {
        serde_json::from_slice(&fs::read(&managed_path)?)?
    } else {
        Vec::new()
    };
    for name in &m.mods {
        fs::copy(dir.join("mods").join(name), mods.join(name))?;
    }
    for name in old {
        if !m.mods.contains(&name) && bundle::relative(&name)?.components().count() == 1 {
            let path = mods.join(name);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
    }
    fs::write(managed_path, serde_json::to_vec_pretty(&m.mods)?)?;
    Ok(())
}

pub fn run(root: &Path, settings: &Settings, status: &impl Fn(String)) -> Result<()> {
    settings.validate()?;
    let _root_lock = bundle::lock(root, "game.lock")?;
    let game = settings.game_dir(root);
    // A second data directory must not launch into the same saves simultaneously.
    let _game_lock = bundle::lock(&game, ".transcraft.lock")?;
    let (mut exe, payload) = bundle::embedded()?;
    let dir = payload.install(&mut exe, root, false, status)?;
    let m = bundle::manifest(&dir)?;
    install_mods(&m, &dir, &game)?;
    let natives = game.join(".transcraft-natives").join(payload.id());
    for sub in ["java", "jna", "lwjgl", "netty"] {
        fs::create_dir_all(natives.join(sub))?;
    }
    let bundled_natives = dir.join("natives/java");
    if bundled_natives.is_dir() {
        copy_tree(&bundled_natives, &natives.join("java"))?;
    }
    let args = command_args(&m, settings, &dir, &game, &natives)?;
    let argfile = game.join(".transcraft-java.args");
    fs::write(
        &argfile,
        args.iter()
            .map(|s| quote_arg(s))
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    let logs = game.join("logs");
    fs::create_dir_all(&logs)?;
    let log_path = logs.join("transcraft-java.log");
    let log = File::create(&log_path)?;
    let mut cmd = Command::new(dir.join(&m.java));
    // Use an ASCII filename relative to the Unicode-aware process cwd. Java's
    // native Windows argument-file opener can otherwise mangle non-ACP paths.
    cmd.arg("@.transcraft-java.args")
        .current_dir(&game)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Prevent host Java configuration from changing the self-contained runtime.
    for name in [
        "JAVA_TOOL_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "_JAVA_OPTIONS",
        "CLASSPATH",
    ] {
        cmd.env_remove(name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    status("Starting Minecraft…".into());
    let mut child = cmd.spawn().context("Could not start bundled Java")?;
    status("Minecraft is running. Close the game to return here.".into());
    let exit = child.wait()?;
    ensure!(
        exit.success(),
        "Minecraft exited with {exit}. See {}",
        log_path.display()
    );
    status("Ready to play".into());
    Ok(())
}

fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dest.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dest.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_keep_paths_and_nickname_as_single_tokens() {
        let m = Manifest {
            format: 1,
            platform: "linux".into(),
            minecraft: "26.3".into(),
            fabric: "0.20.1".into(),
            java: "runtime/bin/java".into(),
            main_class: "net.fabricmc.loader.impl.launch.knot.KnotClient".into(),
            asset_index: "34".into(),
            classpath: vec!["libraries/a.jar".into()],
            mods: vec![],
            jvm_args: vec!["-cp".into(), "${classpath}".into()],
            game_args: vec![
                "--username".into(),
                "${auth_player_name}".into(),
                "--gameDir".into(),
                "${game_directory}".into(),
            ],
        };
        let settings = Settings {
            nickname: "Test_Player".into(),
            ..Default::default()
        };
        let args = command_args(
            &m,
            &settings,
            Path::new("/test data"),
            Path::new("/my worlds"),
            Path::new("/natives"),
        )
        .unwrap();
        assert!(args.iter().any(|s| s == "/my worlds"));
        assert!(args.iter().any(|s| s == "Test_Player"));
        assert!(args.iter().any(|s| s == "-Xmx4096M"));
        let mut broken = m;
        broken.game_args.push("${unknown_variable}".into());
        assert!(
            command_args(
                &broken,
                &settings,
                Path::new("/data"),
                Path::new("/game"),
                Path::new("/natives")
            )
            .is_err()
        );
    }
    #[test]
    fn java_compatible_offline_identity() {
        assert_eq!(offline_uuid("Notch"), "b50ad385829d3141a2167e7d7539ba7f");
        assert_ne!(offline_uuid("Player"), offline_uuid("player"));
    }

    #[test]
    fn java_argfile_escaping() {
        assert_eq!(
            quote_arg("C:\\Some Folder\\java"),
            "\"C:\\\\Some Folder\\\\java\""
        );
        assert_eq!(quote_arg("a\"b\nc"), "\"a\\\"b\\nc\"");
        assert_eq!(quote_arg(""), "\"\"");
    }
}
