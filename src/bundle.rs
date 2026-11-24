use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 16] = b"TRANSCRAFT1END!!";
const FOOTER: u64 = 56;

#[derive(Clone, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub platform: String,
    pub minecraft: String,
    pub fabric: String,
    pub java: String,
    pub main_class: String,
    pub asset_index: String,
    pub classpath: Vec<String>,
    pub mods: Vec<String>,
    pub jvm_args: Vec<String>,
    pub game_args: Vec<String>,
}

pub struct Payload {
    pub offset: u64,
    pub length: u64,
    pub digest: [u8; 32],
}

pub fn lock(root: &Path, name: &str) -> Result<File> {
    fs::create_dir_all(root)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(name))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("Another Transcraft operation is using this folder. Wait for it to finish.")?;
    Ok(file)
}

impl Payload {
    pub fn read(file: &mut File) -> Result<Option<Self>> {
        let size = file.metadata()?.len();
        if size < FOOTER {
            return Ok(None);
        }
        file.seek(SeekFrom::End(-(FOOTER as i64)))?;
        let mut footer = [0u8; FOOTER as usize];
        file.read_exact(&mut footer)?;
        if &footer[40..] != MAGIC {
            return Ok(None);
        }
        let length = u64::from_le_bytes(footer[..8].try_into()?);
        ensure!(
            length > 0 && length <= size - FOOTER,
            "Invalid embedded payload length"
        );
        Ok(Some(Self {
            offset: size - FOOTER - length,
            length,
            digest: footer[8..40].try_into()?,
        }))
    }

    pub fn id(&self) -> String {
        format!(
            "{:x}",
            sha2::digest::generic_array::GenericArray::from(self.digest)
        )
    }

    fn check(&self, file: &mut File) -> Result<()> {
        file.seek(SeekFrom::Start(self.offset))?;
        let mut digest = Sha256::new();
        let read = std::io::copy(&mut file.take(self.length), &mut digest)?;
        ensure!(
            read == self.length && digest.finalize().as_slice() == self.digest,
            "Embedded payload checksum mismatch. Copy the executable again."
        );
        Ok(())
    }

    pub fn directory(&self, root: &Path) -> PathBuf {
        root.join("bundles").join(self.id())
    }

    pub fn install(
        &self,
        file: &mut File,
        root: &Path,
        force: bool,
        status: &impl Fn(String),
    ) -> Result<PathBuf> {
        let _lock = lock(root, "install.lock")?;
        let dest = self.directory(root);
        if !force && dest.join(".complete").is_file() && essentials(&dest).is_ok() {
            return Ok(dest);
        }
        status("Checking bundled files…".into());
        self.check(file)?;
        let stage = root.join("bundles").join(format!("{}.partial", self.id()));
        if stage.exists() {
            fs::remove_dir_all(&stage)?;
        }
        fs::create_dir_all(&stage)?;
        status("Extracting Java, Minecraft, and mods…".into());
        file.seek(SeekFrom::Start(self.offset))?;
        let result = (|| -> Result<()> {
            unpack(GzDecoder::new(file.take(self.length)), &stage)?;
            status("Verifying extracted files…".into());
            verify(&stage)?;
            essentials(&stage)?;
            fs::write(stage.join(".complete"), self.id())?;
            if dest.exists() {
                fs::remove_dir_all(&dest)?;
            }
            fs::rename(&stage, &dest)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&stage);
        }
        result?;
        Ok(dest)
    }
}

pub fn relative(path: &str) -> Result<&Path> {
    let p = Path::new(path);
    ensure!(
        !path.is_empty()
            && !path.contains(['\\', ':'])
            && p.components().all(|c| matches!(c, Component::Normal(_))),
        "Unsafe payload path: {path}"
    );
    Ok(p)
}

pub fn unpack(reader: impl Read, destination: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    for item in archive.entries()? {
        let mut item = item?;
        ensure!(
            item.header().entry_type().is_file(),
            "Payload must contain regular files only"
        );
        let name = item.path()?.to_string_lossy().into_owned();
        let path = destination.join(relative(&name)?);
        fs::create_dir_all(path.parent().context("Missing archive parent")?)?;
        item.unpack(path)?;
    }
    Ok(())
}

pub fn manifest(dir: &Path) -> Result<Manifest> {
    let m: Manifest = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    ensure!(m.format == 1, "Unsupported bundle format");
    let host = if cfg!(windows) { "windows" } else { "linux" };
    ensure!(
        m.platform == host,
        "This bundle is for {}, not {host}",
        m.platform
    );
    for path in m.classpath.iter().chain(std::iter::once(&m.java)) {
        relative(path)?;
    }
    for name in &m.mods {
        ensure!(
            relative(name)?.components().count() == 1,
            "Invalid mod filename"
        );
    }
    Ok(m)
}

fn essentials(dir: &Path) -> Result<()> {
    let m = manifest(dir)?;
    for path in m.classpath.iter().chain(std::iter::once(&m.java)) {
        ensure!(dir.join(path).is_file(), "Missing bundled file: {path}");
    }
    for name in &m.mods {
        ensure!(dir.join("mods").join(name).is_file(), "Missing mod: {name}");
    }
    Ok(())
}

pub fn verify(dir: &Path) -> Result<()> {
    let receipt: BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(dir.join("checksums.json"))?)?;
    ensure!(!receipt.is_empty(), "Empty payload receipt");
    for (name, expected) in receipt {
        let mut file = File::open(dir.join(relative(&name)?))
            .with_context(|| format!("Missing file: {name}"))?;
        let mut hash = Sha256::new();
        std::io::copy(&mut file, &mut hash)?;
        if format!("{:x}", hash.finalize()) != expected {
            bail!("Damaged bundled file: {name}. Use Repair bundled files.");
        }
    }
    Ok(())
}

pub fn embedded() -> Result<(File, Payload)> {
    let mut file = File::open(std::env::current_exe()?)?;
    let payload = Payload::read(&mut file)?.context("This launcher has no offline bundle. On an online build machine, follow README.md to package Minecraft and Java into this executable.")?;
    Ok((file, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_truncated_and_corrupt_payloads() {
        use std::io::Write;
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"EXEpayload").unwrap();
        file.write_all(&7u64.to_le_bytes()).unwrap();
        file.write_all(&Sha256::digest(b"payload")).unwrap();
        file.write_all(MAGIC).unwrap();
        let payload = Payload::read(&mut file).unwrap().unwrap();
        assert_eq!(payload.offset, 3);
        payload.check(&mut file).unwrap();
        file.seek(SeekFrom::Start(3)).unwrap();
        file.write_all(b"X").unwrap();
        assert!(payload.check(&mut file).is_err());
        file.seek(SeekFrom::End(-(FOOTER as i64))).unwrap();
        file.write_all(&u64::MAX.to_le_bytes()).unwrap();
        assert!(Payload::read(&mut file).is_err());
    }

    #[test]
    fn rejects_archive_symlinks() {
        let mut bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            archive.append_link(&mut header, "escape", "/tmp").unwrap();
            archive.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(unpack(bytes.as_slice(), dir.path()).is_err());
        assert!(!dir.path().join("escape").exists());
    }

    #[test]
    fn receipt_detects_missing_and_modified_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sample"), "original").unwrap();
        fs::write(
            dir.path().join("checksums.json"),
            format!(r#"{{"sample":"{:x}"}}"#, Sha256::digest(b"original")),
        )
        .unwrap();
        verify(dir.path()).unwrap();
        fs::write(dir.path().join("sample"), "changed").unwrap();
        assert!(verify(dir.path()).is_err());
        fs::remove_file(dir.path().join("sample")).unwrap();
        assert!(verify(dir.path()).is_err());
    }

    #[test]
    fn rejects_cross_platform_traversal() {
        for p in ["../evil", "/evil", "C:/evil", "a\\b", "a/../b", ""] {
            assert!(relative(p).is_err(), "{p}");
        }
        assert!(relative("runtime/bin/java").is_ok());
    }
}
