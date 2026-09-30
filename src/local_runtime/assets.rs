use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Deserialize)]
pub(super) struct Voice {
    pub language: String,
    pub model: Asset,
    pub config: Asset,
    pub license: Asset,
}
#[derive(Deserialize)]
pub(super) struct Catalog {
    pub whisper: BTreeMap<String, Asset>,
    pub translation: BTreeMap<String, Asset>,
    pub voices: BTreeMap<String, Voice>,
}
impl Catalog {
    pub fn read() -> Self {
        serde_json::from_str(include_str!("models.json")).expect("bundled model catalog")
    }
    pub fn voice(&self, requested: &str, language: &str) -> Result<String> {
        if !requested.is_empty() && requested != "auto" {
            ensure!(
                self.voices.contains_key(requested),
                "Voice is not available in the bundled Piper catalog: {requested}"
            );
            return Ok(requested.into());
        }
        let language = language
            .split(['-', '_'])
            .next()
            .unwrap_or(language)
            .to_ascii_lowercase();
        self.voices.iter().find(|(_,voice)| voice.language == language).map(|(id,_)|id.clone())
            .with_context(|| format!("No bundled Piper voice for language {language}; choose a supported target language or an external Piper service"))
    }
}

pub(super) fn cache_directory(custom: &str) -> Result<PathBuf> {
    if !custom.is_empty() {
        let path = PathBuf::from(custom);
        ensure!(
            path.is_absolute(),
            "Local model directory must be an absolute path"
        );
        return Ok(path);
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("Babel"));
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join("Library/Application Support/Babel"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("babel"))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".local/share/babel"))
        });
    let path = base.context("Cannot determine the local model directory; configure an absolute directory in Settings")?.join("models");
    ensure!(
        path.is_absolute(),
        "Local model directory must be an absolute path"
    );
    Ok(path)
}

pub(super) async fn verified(path: &Path, size: u64, hash: &str) -> Result<bool> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if file.metadata().await?.len() != size {
        return Ok(false);
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()) == hash)
}

pub(super) async fn acquire(
    asset: &Asset,
    cache: &Path,
    bundled: &Path,
    cancel: &CancellationToken,
    progress: impl Fn(u64, u64),
) -> Result<PathBuf> {
    ensure!(
        asset.size > 0
            && asset.sha256.len() == 64
            && asset.sha256.bytes().all(|v| v.is_ascii_hexdigit()),
        "Invalid bundled model digest"
    );
    ensure!(
        Path::new(&asset.name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
            && !asset.name.contains(['/', '\\']),
        "Invalid bundled model name"
    );
    tokio::fs::create_dir_all(cache).await?;
    let name = format!("{}-{}", &asset.sha256[..16], asset.name);
    let destination = cache.join(&name);
    if tokio::select! { _=cancel.cancelled()=>bail!("Local model preparation cancelled"), r=verified(&destination,asset.size,&asset.sha256)=>r? }
    {
        return Ok(destination);
    }
    let temp = tempfile::NamedTempFile::new_in(cache)?;
    let mut output = tokio::fs::File::from_std(temp.reopen()?);
    let bundled = bundled.join("models").join(&name);
    if tokio::select! {_=cancel.cancelled()=>bail!("Local model preparation cancelled"),r=verified(&bundled,asset.size,&asset.sha256)=>r?}
    {
        let mut input = tokio::fs::File::open(bundled).await?;
        tokio::select! {_=cancel.cancelled()=>bail!("Local model preparation cancelled"),r=tokio::io::copy(&mut input,&mut output)=>{r?;}}
    } else {
        let url = reqwest::Url::parse(&asset.url)?;
        ensure!(
            url.scheme() == "https" && url.host_str() == Some("huggingface.co"),
            "Invalid bundled model download URL"
        );
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(1800))
            .build()?;
        let mut response=tokio::select! {_=cancel.cancelled()=>bail!("Local model preparation cancelled"),r=client.get(url).send()=>r.context("Could not download the selected local model; check connectivity and try again")?}.error_for_status()?;
        if let Some(length) = response.content_length() {
            ensure!(
                length == asset.size,
                "Local model download size does not match its catalog"
            );
        }
        let mut received = 0;
        let mut hash = Sha256::new();
        let mut last = std::time::Instant::now();
        progress(0, asset.size);
        loop {
            let chunk = tokio::select! {_=cancel.cancelled()=>bail!("Local model preparation cancelled"),r=response.chunk()=>r?};
            let Some(chunk) = chunk else { break };
            received += chunk.len() as u64;
            ensure!(
                received <= asset.size,
                "Local model download exceeds its expected size"
            );
            hash.update(&chunk);
            output.write_all(&chunk).await?;
            if last.elapsed() > Duration::from_millis(250) {
                progress(received, asset.size);
                last = std::time::Instant::now();
            }
        }
        ensure!(
            received == asset.size && format!("{:x}", hash.finalize()) == asset.sha256,
            "Local model integrity verification failed; retry the download"
        );
        progress(received, asset.size);
    }
    output.flush().await?;
    output.sync_all().await?;
    drop(output);
    // NamedTempFile persists atomically and removes incomplete downloads on cancellation.
    temp.persist(&destination).map_err(|e| e.error)?;
    Ok(destination)
}

#[derive(Deserialize)]
pub(super) struct Service {
    pub executable: String,
    pub data: Option<String>,
}
#[derive(Deserialize)]
struct FileEntry {
    path: String,
    sha256: String,
    size: u64,
}
#[derive(Deserialize)]
struct Manifest {
    version: u32,
    platform: String,
    arch: String,
    services: BTreeMap<String, Service>,
    files: Vec<FileEntry>,
}
pub(super) struct Bundle {
    pub root: PathBuf,
    pub services: BTreeMap<String, Service>,
}
impl Bundle {
    /// Locate a bundle without touching native components. Asset-only work
    /// verifies model bytes against the pinned catalog and never executes code.
    pub fn locate() -> Result<PathBuf> {
        let system = if cfg!(target_os = "macos") {
            "macos"
        } else {
            std::env::consts::OS
        };
        let arch = std::env::consts::ARCH;
        let platform = format!("{system}-{arch}");
        let exe = std::env::current_exe()?;
        let parent = exe.parent().context("Executable has no parent directory")?;
        let candidates = [
            parent.join("local-runtime").join(&platform),
            parent.join("../Resources/local-runtime").join(&platform),
            parent.join("../share/babel/local-runtime").join(&platform),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("artifacts/local-runtime")
                .join(&platform),
        ];
        Self::locate_in(candidates)
    }
    fn locate_in(candidates: impl IntoIterator<Item = PathBuf>) -> Result<PathBuf> {
        candidates.into_iter().find(|p| p.join("manifest.json").is_file())
            .context("Bundled local inference components are missing; reinstall Babel with its local-runtime package")?
            .canonicalize().context("Could not resolve the local inference bundle")
    }
    pub async fn discover() -> Result<Self> {
        let root = Self::locate()?;
        let system = if cfg!(target_os = "macos") {
            "macos"
        } else {
            std::env::consts::OS
        };
        Self::load(&root, system, std::env::consts::ARCH).await
    }
    async fn load(root: &Path, system: &str, arch: &str) -> Result<Self> {
        let root = root.canonicalize()?;
        let file = root.join("manifest.json");
        ensure!(
            tokio::fs::metadata(&file).await?.len() <= 2 * 1024 * 1024,
            "Local runtime manifest is too large"
        );
        let manifest: Manifest = serde_json::from_slice(&tokio::fs::read(file).await?)?;
        ensure!(
            manifest.version == 1 && manifest.platform == system && manifest.arch == arch,
            "Local runtime bundle does not match this operating system and architecture"
        );
        let mut seen = std::collections::BTreeSet::new();
        for item in &manifest.files {
            ensure!(
                seen.insert(item.path.clone()),
                "Duplicate local runtime manifest path"
            );
            let path = safe_path(&root, &item.path)?;
            ensure!(
                verified(&path, item.size, &item.sha256).await?,
                "Bundled local inference component failed its integrity check: {}",
                item.path
            );
        }
        for service in manifest.services.values() {
            ensure!(
                seen.contains(&service.executable),
                "Local runtime executable is missing from its integrity manifest"
            );
            safe_path(&root, &service.executable)?;
            if let Some(data) = &service.data {
                safe_path(&root, data)?;
            }
        }
        Ok(Self {
            root,
            services: manifest.services,
        })
    }
    pub fn executable(&self, name: &str) -> Result<PathBuf> {
        let service = self
            .services
            .get(name)
            .with_context(|| format!("Bundled local component is missing: {name}"))?;
        safe_path(&self.root, &service.executable)
    }
}
fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !relative.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_))),
        "Invalid path in local runtime manifest"
    );
    let canonical = root.join(path).canonicalize()?;
    ensure!(
        canonical.starts_with(root),
        "Local runtime path escapes its bundle"
    );
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_is_pinned_and_language_specific() {
        let c = Catalog::read();
        assert!(c.whisper.contains_key(crate::config::DEFAULT_WHISPER_MODEL));
        assert!(
            c.whisper
                .keys()
                .all(|model| crate::config::is_managed_whisper_model(model))
        );
        for original in ["tiny", "base", "small"] {
            let compact = &c.whisper[&format!("{original}-q5_1")];
            let unquantized = &c.whisper[original];
            assert!(compact.size < unquantized.size / 2);
            // Old and compact models must coexist in offline caches; choosing
            // the compact form must never overwrite an explicitly chosen file.
            assert_ne!(compact.name, unquantized.name);
            assert_ne!(compact.sha256, unquantized.sha256);
        }
        assert_eq!(c.voice("auto", "pt-BR").unwrap(), "pt_BR-faber-medium");
        assert!(c.voice("auto", "xx").is_err());
        assert!(c.voice("../../voice", "en").is_err());
        for asset in c.whisper.values().chain(c.translation.values()).chain(
            c.voices
                .values()
                .flat_map(|v| [&v.model, &v.config, &v.license]),
        ) {
            assert_eq!(asset.sha256.len(), 64);
            assert!(asset.url.starts_with("https://huggingface.co/"));
            assert!(!asset.url.contains("/main/"));
        }
    }
    #[tokio::test]
    async fn verifies_content_and_rejects_modified_cache() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("model");
        tokio::fs::write(&file, b"test").await.unwrap();
        let sha = format!("{:x}", Sha256::digest(b"test"));
        assert!(verified(&file, 4, &sha).await.unwrap());
        tokio::fs::write(&file, b"bad!").await.unwrap();
        assert!(!verified(&file, 4, &sha).await.unwrap());
        assert!(safe_path(dir.path(), "../model").is_err());
    }
    #[tokio::test]
    async fn offline_models_are_verified_and_installed_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        let cache = temp.path().join("cache");
        tokio::fs::create_dir_all(bundle.join("models"))
            .await
            .unwrap();
        let asset = Asset {
            name: "model.bin".into(),
            url: "https://huggingface.co/fixture/resolve/pinned/model.bin".into(),
            size: 5,
            sha256: format!("{:x}", Sha256::digest(b"model")),
        };
        let filename = format!("{}-{}", &asset.sha256[..16], asset.name);
        tokio::fs::write(bundle.join("models").join(&filename), b"model")
            .await
            .unwrap();
        let path = acquire(
            &asset,
            &cache,
            &bundle,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"model");
        // Reuse the verified cache even when the offline bundle has gone away.
        tokio::fs::remove_dir_all(bundle.join("models"))
            .await
            .unwrap();
        assert_eq!(
            acquire(
                &asset,
                &cache,
                &bundle,
                &CancellationToken::new(),
                |_, _| {}
            )
            .await
            .unwrap(),
            path
        );
        assert_eq!(std::fs::read_dir(cache).unwrap().count(), 1);
    }
    #[tokio::test]
    async fn bundle_integrity_blocks_modified_helpers_and_wrong_architecture() {
        let temp = tempfile::tempdir().unwrap();
        let helper = temp.path().join("helper");
        tokio::fs::write(&helper, b"binary").await.unwrap();
        let manifest = serde_json::json!({"version":1,"platform":"linux","arch":"x86_64","services":{"whisper":{"executable":"helper"}},"files":[{"path":"helper","size":6,"sha256":format!("{:x}",Sha256::digest(b"binary"))}]});
        tokio::fs::write(
            temp.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .await
        .unwrap();
        assert!(Bundle::load(temp.path(), "linux", "x86_64").await.is_ok());
        assert!(Bundle::load(temp.path(), "linux", "aarch64").await.is_err());
        tokio::fs::write(helper, b"edited").await.unwrap();
        // Merely caching selected model assets must not read/hash unused native
        // helpers. Loading inference still checks all helper bytes before use.
        assert_eq!(
            Bundle::locate_in([temp.path().to_path_buf()]).unwrap(),
            temp.path().canonicalize().unwrap()
        );
        assert!(Bundle::load(temp.path(), "linux", "x86_64").await.is_err());
    }
    #[test]
    fn custom_model_path_is_absolute() {
        assert!(cache_directory("relative/path").is_err());
        assert!(cache_directory("").unwrap().is_absolute());
    }
}
