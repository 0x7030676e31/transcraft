use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub nickname: String,
    pub memory_mib: u32,
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
    pub game_directory: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            nickname: "Player".into(),
            memory_mib: 4096,
            width: 1280,
            height: 720,
            fullscreen: false,
            game_directory: String::new(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (3..=16).contains(&self.nickname.len())
                && self
                    .nickname
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_'),
            "Nickname must contain 3–16 ASCII letters, numbers, or underscores."
        );
        ensure!(
            (1024..=65536).contains(&self.memory_mib),
            "Memory must be 1024–65536 MiB."
        );
        ensure!(
            (640..=7680).contains(&self.width) && (360..=4320).contains(&self.height),
            "Window size must be between 640×360 and 7680×4320."
        );
        if !self.game_directory.is_empty() {
            ensure!(
                Path::new(&self.game_directory).is_absolute(),
                "Game folder must be an absolute path."
            );
        }
        Ok(())
    }

    pub fn load(root: &Path) -> Result<Self> {
        let mut path = root.join("settings.json");
        if !path.exists() {
            let backup = root.join("settings.json.bak");
            if backup.exists() {
                path = backup;
            } else {
                return Ok(Self::default());
            }
        }
        serde_json::from_slice(&fs::read(path)?)
            .context("Cannot read settings.json; fix or rename it to reset settings")
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        self.validate()?;
        fs::create_dir_all(root)?;
        // Serialize under a separate lock so two launcher windows cannot interleave writes.
        let _lock = crate::bundle::lock(root, "settings.lock")?;
        let temp = root.join("settings.json.tmp");
        fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        // Windows rename does not replace an existing file. Preserve the previous copy
        // until the new copy is safely written; recover it on failure.
        let path = root.join("settings.json");
        let backup = root.join("settings.json.bak");
        if path.exists() {
            fs::copy(&path, &backup)?;
            fs::remove_file(&path)?;
        }
        if let Err(e) = fs::rename(&temp, &path) {
            if backup.exists() {
                let _ = fs::copy(&backup, &path);
            }
            return Err(e.into());
        }
        Ok(())
    }

    pub fn game_dir(&self, root: &Path) -> PathBuf {
        if self.game_directory.is_empty() {
            root.join("game")
        } else {
            self.game_directory.clone().into()
        }
    }
}

pub fn data_root() -> Result<PathBuf> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mut override_path = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--data-dir" {
            i += 1;
            override_path = Some(PathBuf::from(
                args.get(i).context("--data-dir needs a path")?,
            ));
        } else if !["--verify", "--install", "--launch", "--help", "--version"]
            .iter()
            .any(|s| args[i] == *s)
        {
            bail!("Unknown argument: {}", args[i].to_string_lossy());
        }
        i += 1;
    }
    if let Some(path) = override_path {
        return Ok(std::path::absolute(path)?);
    }
    Ok(ProjectDirs::from("", "", "Transcraft")
        .context("Cannot locate user data folder; supply --data-dir")?
        .data_local_dir()
        .into())
}
