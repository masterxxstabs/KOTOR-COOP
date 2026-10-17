//! Mod/patch updater.
//!
//! An update source is a plain HTTP directory (the master server's `/updates`,
//! or any static host) containing:
//!
//! ```text
//! manifest.json        list of files with SHA-256 and size
//! manifest.json.sig    ed25519 signature of manifest.json (hex)
//! files/<path>         each file, at its path relative to the game folder
//! ```
//!
//! Files are verified before they replace anything. Game files we overwrite
//! are backed up to `kotor-coop/backup/` the first time, so `uninstall`
//! restores a vanilla game exactly.

use anyhow::{bail, Context};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const COOP_DIR: &str = "kotor-coop";
const INSTALLED_FILE: &str = "installed.json";
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub protocol_version: u32,
    #[serde(default)]
    pub notes: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to the game folder, `/`-separated (e.g. `Override/coop_spawn.ncs`).
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Installed {
    pub version: Option<String>,
    pub files: Vec<FileEntry>,
    /// Paths of original game files we backed up before overwriting.
    pub backups: Vec<String>,
}

impl Installed {
    pub fn load(game: &Path) -> Installed {
        std::fs::read(game.join(COOP_DIR).join(INSTALLED_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, game: &Path) -> anyhow::Result<()> {
        let dir = game.join(COOP_DIR);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(INSTALLED_FILE), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

/// Rejects absolute paths, `..`, and anything that would escape the game folder.
pub fn safe_relative(path: &str) -> anyhow::Result<PathBuf> {
    let p = Path::new(path);
    if path.is_empty() || path.contains('\\') || path.contains(':') {
        bail!("unsafe path in manifest: {path:?}");
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            _ => bail!("unsafe path in manifest: {path:?}"),
        }
    }
    Ok(out)
}

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn verify_signature(manifest: &[u8], sig_hex: &str, key_hex: &str) -> anyhow::Result<()> {
    let key: [u8; 32] = hex::decode(key_hex.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("bad public key"))?;
    let sig: [u8; 64] = hex::decode(sig_hex.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("bad signature"))?;
    VerifyingKey::from_bytes(&key)?
        .verify(manifest, &Signature::from_bytes(&sig))
        .context("manifest signature does not match the trusted update key")
}

/// Where update files come from. HTTP in practice; a local folder in tests.
pub trait Source {
    fn fetch(&self, rel: &str, limit: u64) -> anyhow::Result<Vec<u8>>;
}

pub struct HttpSource(pub String);

impl Source for HttpSource {
    fn fetch(&self, rel: &str, limit: u64) -> anyhow::Result<Vec<u8>> {
        let url = format!("{}/{rel}", self.0.trim_end_matches('/'));
        let resp = ureq::get(&url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        let mut body = Vec::new();
        resp.into_body()
            .into_reader()
            .take(limit + 1)
            .read_to_end(&mut body)?;
        anyhow::ensure!(body.len() as u64 <= limit, "{url} is larger than expected");
        Ok(body)
    }
}

pub struct DirSource(pub PathBuf);

impl Source for DirSource {
    fn fetch(&self, rel: &str, _limit: u64) -> anyhow::Result<Vec<u8>> {
        Ok(std::fs::read(self.0.join(rel))?)
    }
}

/// `http(s)://...` downloads; anything else is treated as a local folder (LAN/offline installs).
pub fn source_for(location: &str) -> Box<dyn Source + Send> {
    if location.starts_with("http://") || location.starts_with("https://") {
        Box::new(HttpSource(location.to_owned()))
    } else {
        Box::new(DirSource(PathBuf::from(location)))
    }
}

pub struct UpdateCheck {
    pub manifest: Manifest,
    /// Files that are missing or differ locally.
    pub needed: Vec<FileEntry>,
    pub stale: Vec<String>,
}

/// Downloads and (if a key is configured) verifies the manifest, then compares it with the game folder.
pub fn check(
    src: &dyn Source,
    game: &Path,
    public_key: Option<&str>,
) -> anyhow::Result<UpdateCheck> {
    let raw = src.fetch("manifest.json", 4 * 1024 * 1024)?;
    // Without a key the caller warns that updates are unverified.
    if let Some(key) = public_key.filter(|k| !k.trim().is_empty()) {
        let sig = src
            .fetch("manifest.json.sig", 1024)
            .context("update is not signed")?;
        verify_signature(&raw, std::str::from_utf8(&sig)?, key)?;
    }
    let manifest: Manifest = serde_json::from_slice(&raw).context("parsing manifest.json")?;
    let mut needed = Vec::new();
    for f in &manifest.files {
        let local = game.join(safe_relative(&f.path)?);
        let ok = local.is_file()
            && sha256_file(&local)
                .map(|h| h.eq_ignore_ascii_case(&f.sha256))
                .unwrap_or(false);
        if !ok {
            needed.push(f.clone());
        }
    }
    let installed = Installed::load(game);
    let stale = installed
        .files
        .iter()
        .filter(|old| !manifest.files.iter().any(|f| f.path == old.path))
        .map(|f| f.path.clone())
        .collect();
    Ok(UpdateCheck {
        manifest,
        needed,
        stale,
    })
}

/// Applies a checked update. `progress` gets human-readable lines.
pub fn apply(
    src: &dyn Source,
    game: &Path,
    check: &UpdateCheck,
    progress: &mut dyn FnMut(String),
) -> anyhow::Result<()> {
    let mut installed = Installed::load(game);
    let backup_root = game.join(COOP_DIR).join("backup");

    for (i, f) in check.needed.iter().enumerate() {
        progress(format!("[{}/{}] {}", i + 1, check.needed.len(), f.path));
        anyhow::ensure!(f.size <= MAX_FILE_BYTES, "{} is too large", f.path);
        let rel = safe_relative(&f.path)?;
        let data = src.fetch(&format!("files/{}", f.path), f.size)?;
        let hash = hex::encode(Sha256::digest(&data));
        anyhow::ensure!(
            data.len() as u64 == f.size && hash.eq_ignore_ascii_case(&f.sha256),
            "{} failed verification (corrupt download or tampered server)",
            f.path
        );
        let dest = game.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let ours = installed.files.iter().any(|e| e.path == f.path);
        if dest.exists() && !ours && !installed.backups.contains(&f.path) {
            let b = backup_root.join(&rel);
            std::fs::create_dir_all(b.parent().unwrap())?;
            std::fs::copy(&dest, &b).with_context(|| format!("backing up {}", f.path))?;
            installed.backups.push(f.path.clone());
            progress(format!("    backed up original {}", f.path));
        }
        let tmp = dest.with_extension("coop-download");
        std::fs::write(&tmp, &data)?;
        std::fs::rename(&tmp, &dest)
            .with_context(|| format!("replacing {} (is the game running?)", f.path))?;
    }
    for path in &check.stale {
        progress(format!("removing {path} (no longer part of the mod)"));
        remove_or_restore(game, path, &mut installed)?;
    }
    installed.files = check.manifest.files.clone();
    installed.version = Some(check.manifest.version.clone());
    installed.save(game)?;
    progress(format!("KOTOR Co-op {} installed", check.manifest.version));
    Ok(())
}

fn remove_or_restore(game: &Path, path: &str, installed: &mut Installed) -> anyhow::Result<()> {
    let rel = safe_relative(path)?;
    let dest = game.join(&rel);
    if let Some(pos) = installed.backups.iter().position(|b| b == path) {
        std::fs::copy(game.join(COOP_DIR).join("backup").join(&rel), &dest)?;
        installed.backups.remove(pos);
    } else if dest.exists() {
        std::fs::remove_file(&dest)?;
    }
    Ok(())
}

/// Removes every installed file and restores backed-up originals.
pub fn uninstall(game: &Path, progress: &mut dyn FnMut(String)) -> anyhow::Result<()> {
    let mut installed = Installed::load(game);
    let paths: Vec<String> = installed.files.iter().map(|f| f.path.clone()).collect();
    for p in paths {
        progress(format!("removing {p}"));
        remove_or_restore(game, &p, &mut installed)?;
    }
    installed.files.clear();
    installed.version = None;
    installed.save(game)?;
    progress("game restored to its original files".into());
    Ok(())
}

/// Builds a manifest for a release folder laid out like the game folder.
/// Writes `manifest.json`, copies files under `out/files/`, and signs it if a key is given.
pub fn build_release(
    release_dir: &Path,
    out: &Path,
    version: &str,
    signing_key_hex: Option<&str>,
) -> anyhow::Result<Manifest> {
    let mut files = Vec::new();
    let mut stack = vec![release_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(release_dir)?;
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            safe_relative(&rel_str)?;
            let dest = out.join("files").join(rel);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::copy(&path, &dest)?;
            files.push(FileEntry {
                path: rel_str,
                sha256: sha256_file(&path)?,
                size: std::fs::metadata(&path)?.len(),
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = Manifest {
        version: version.into(),
        protocol_version: kotor_protocol::PROTOCOL_VERSION,
        notes: String::new(),
        files,
    };
    let json = serde_json::to_vec_pretty(&manifest)?;
    std::fs::create_dir_all(out)?;
    std::fs::write(out.join("manifest.json"), &json)?;
    if let Some(k) = signing_key_hex {
        let key: [u8; 32] = hex::decode(k.trim())?
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad signing key"))?;
        let sig = SigningKey::from_bytes(&key).sign(&json);
        std::fs::write(out.join("manifest.json.sig"), hex::encode(sig.to_bytes()))?;
    }
    Ok(manifest)
}

/// Returns (secret_hex, public_hex).
pub fn generate_keypair() -> (String, String) {
    let secret: [u8; 32] = rand::random();
    let key = SigningKey::from_bytes(&secret);
    (
        hex::encode(secret),
        hex::encode(key.verifying_key().to_bytes()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kotor-upd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(safe_relative("../evil.dll").is_err());
        assert!(safe_relative("/etc/passwd").is_err());
        assert!(safe_relative("C:/x").is_err());
        assert!(safe_relative("Override\\x").is_err());
        assert!(safe_relative("Override/coop.2da").is_ok());
    }

    #[test]
    fn install_update_and_uninstall_restores_original() {
        let (release, out, game) = (tmp("rel"), tmp("out"), tmp("game"));
        std::fs::create_dir_all(release.join("Override")).unwrap();
        std::fs::create_dir_all(release.join("kotor-coop")).unwrap();
        std::fs::write(release.join("Override/appearance.2da"), b"patched").unwrap();
        std::fs::write(release.join("kotor-coop/kotor_coop.dll"), b"dll v1").unwrap();
        std::fs::create_dir_all(game.join("Override")).unwrap();
        std::fs::write(game.join("Override/appearance.2da"), b"vanilla").unwrap();

        let (secret, public) = generate_keypair();
        build_release(&release, &out, "0.1.0", Some(&secret)).unwrap();
        let src = DirSource(out.clone());

        let c = check(&src, &game, Some(&public)).unwrap();
        assert_eq!(c.needed.len(), 2);
        apply(&src, &game, &c, &mut |_| {}).unwrap();
        assert_eq!(
            std::fs::read(game.join("Override/appearance.2da")).unwrap(),
            b"patched"
        );
        assert!(check(&src, &game, Some(&public)).unwrap().needed.is_empty());

        // v2 drops the 2da: it must be restored from backup, not deleted.
        std::fs::remove_file(release.join("Override/appearance.2da")).unwrap();
        std::fs::write(release.join("kotor-coop/kotor_coop.dll"), b"dll v2").unwrap();
        let out2 = tmp("out2");
        build_release(&release, &out2, "0.2.0", Some(&secret)).unwrap();
        let src2 = DirSource(out2.clone());
        let c2 = check(&src2, &game, Some(&public)).unwrap();
        assert_eq!(c2.stale, vec!["Override/appearance.2da".to_string()]);
        apply(&src2, &game, &c2, &mut |_| {}).unwrap();
        assert_eq!(
            std::fs::read(game.join("Override/appearance.2da")).unwrap(),
            b"vanilla"
        );
        assert_eq!(Installed::load(&game).version.as_deref(), Some("0.2.0"));

        uninstall(&game, &mut |_| {}).unwrap();
        assert!(!game.join("kotor-coop/kotor_coop.dll").exists());
        assert_eq!(
            std::fs::read(game.join("Override/appearance.2da")).unwrap(),
            b"vanilla"
        );

        // Tampered manifest is refused.
        let mut m = std::fs::read(out2.join("manifest.json")).unwrap();
        m[10] ^= 1;
        std::fs::write(out2.join("manifest.json"), m).unwrap();
        assert!(check(&src2, &game, Some(&public)).is_err());

        for d in [release, out, out2, game] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }
}
