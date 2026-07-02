use anyhow::{Context, Result, bail};
use bytes::Bytes;
use log::{debug, info};
use std::env::consts::{ARCH, OS};
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use zip::ZipArchive;

const KSU_OWNER_REPO: &str = "tiann/KernelSU";

/// Outcome of a ksud update check, so `/update` can tell "fetched a new
/// version" apart from "nothing to do".
pub enum UpdateOutcome {
    Updated(String),
    AlreadyLatest(String),
}

#[derive(Clone)]
pub struct Basis {
    os: &'static str,
    arch: &'static str,
    suffix: &'static str,
    bin_root: PathBuf,
}

#[allow(async_fn_in_trait)]
pub trait Tool {
    fn from(basis: Basis) -> Self;
    fn get_name(&self) -> String;
    fn get(&self) -> PathBuf;
    async fn init(&self) -> Result<()> {
        info!("Initializing tool: {}", self.get_name());
        if self.get().exists() {
            info!("Done");
            Ok(())
        } else {
            self.get_latest().await.map(|_| ())
        }
    }
    async fn get_latest(&self) -> Result<UpdateOutcome>;
}

#[derive(Clone)]
pub struct BaseTool {
    basis: Basis,
    name: String,
    path: PathBuf,
}

#[derive(Clone)]
pub struct Ksud(BaseTool);

impl Tool for Ksud {
    fn from(basis: Basis) -> Self {
        let mut bin = basis.bin_root.join(basis.os).join(basis.arch);
        bin.push(format!("{}{}", "ksud", basis.suffix));
        Self(BaseTool {
            basis: basis.clone(),
            name: "ksud".to_string(),
            path: bin,
        })
    }

    fn get_name(&self) -> String {
        self.0.name.clone()
    }

    fn get(&self) -> PathBuf {
        self.0.path.clone()
    }

    async fn get_latest(&self) -> Result<UpdateOutcome> {
        info!("Getting latest ksud");
        self.0.download_latest_ksud().await
    }
}

fn download_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(crate::utils::USER_AGENT)
        .build()?)
}

async fn download_bytes(url: &str) -> Result<Bytes> {
    let client = download_client()?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        bail!("Failed to download asset: {}", resp.status());
    }
    Ok(resp.bytes().await?)
}

/// Resolve the latest KernelSU release tag WITHOUT GitHub API quota:
/// `releases/latest` answers with a 302 whose Location ends in
/// `/releases/tag/<TAG>`.
async fn latest_release_tag() -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent(crate::utils::USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let url = format!("https://github.com/{KSU_OWNER_REPO}/releases/latest");
    let resp = client.get(&url).send().await?;
    if !resp.status().is_redirection() {
        bail!("expected a redirect from {url}, got {}", resp.status());
    }
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .context("releases/latest redirect carries no Location header")?;
    tag_from_location(location)
}

/// Extract `<TAG>` from a `…/releases/tag/<TAG>` Location value. Pure so it is
/// unit-testable; strips trailing slash, query, and fragment.
fn tag_from_location(location: &str) -> Result<String> {
    let tag = location
        .split_once("/releases/tag/")
        .map(|(_, rest)| rest)
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if tag.is_empty() {
        bail!("cannot extract a release tag from Location: {location:?}");
    }
    Ok(tag.to_string())
}

/// nightly.link's tag-addressed artifact form for the Release workflow —
/// verified live 2026-07-02; no run-id lookup and no API auth needed.
fn artifact_url(tag: &str, target: &str) -> String {
    format!("https://nightly.link/{KSU_OWNER_REPO}/workflows/release/{tag}/ksud-{target}.zip")
}

fn write_zip_entry(bytes: Bytes, entry_name: &str, output_path: PathBuf) -> Result<()> {
    let reader = Cursor::new(bytes);
    let mut archive = ZipArchive::new(reader)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        if file.name() == entry_name {
            let mut content = Vec::new();
            file.read_to_end(&mut content)?;
            fs::write(output_path, content)?;
            return Ok(());
        }
    }

    Err(anyhow::anyhow!("Zip entry not found: {entry_name}"))
}

impl BaseTool {
    fn version_path(&self) -> PathBuf {
        self.path.with_file_name(format!("{}.version", self.name))
    }

    fn read_version(&self) -> Option<String> {
        fs::read_to_string(self.version_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn write_version(&self, version: &str) -> Result<()> {
        fs::write(self.version_path(), version)?;
        Ok(())
    }

    async fn download_latest_ksud(&self) -> Result<UpdateOutcome> {
        let tag = latest_release_tag().await?;
        if self.path.exists() && self.read_version().as_deref() == Some(tag.as_str()) {
            info!("{} already at {tag}", self.name);
            return Ok(UpdateOutcome::AlreadyLatest(tag));
        }

        let url = artifact_url(&tag, self.basis.ksud_target());
        info!(
            "Downloading {} {tag} from the release workflow artifacts...",
            self.name
        );
        // ksud ≥ v3.2.5 patches standalone on desktop (magiskboot removed
        // upstream); tag-run artifacts expire after 90 days — a 404 here means
        // waiting for the next KernelSU release (accepted, no fallback).
        let body = download_bytes(&url).await.with_context(|| {
            format!("download {url} failed (expired 90-day artifact retention for {tag}?)")
        })?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        info!("Successfully downloaded, extracting {}...", self.name);
        write_zip_entry(
            body,
            &format!("ksud{}", self.basis.suffix),
            self.path.clone(),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o755))?;
        }
        self.write_version(&tag)?;
        info!("{} updated to {tag}", self.name);
        Ok(UpdateOutcome::Updated(tag))
    }
}

#[derive(Clone)]
pub struct ToolManager {
    ksud: Ksud,
}

impl ToolManager {
    pub fn try_with_bin_root(bin_root: PathBuf) -> Result<Self> {
        let basis = Basis::try_with_bin_root(bin_root)?;
        let ksud = <Ksud as Tool>::from(basis);
        Ok(Self { ksud })
    }

    pub async fn init(&self) -> Result<()> {
        debug!("Initializing tools");
        self.ksud.init().await?;
        Ok(())
    }

    pub async fn update(&self) -> Result<UpdateOutcome> {
        debug!("Updating tools");
        self.ksud.get_latest().await
    }

    pub fn get_ksud(&self) -> Ksud {
        self.ksud.clone()
    }

    pub fn get_ksud_version(&self) -> String {
        self.ksud
            .0
            .read_version()
            .unwrap_or_else(|| "unknown".to_string())
    }
}

impl Basis {
    pub fn try_with_bin_root(bin_root: PathBuf) -> Result<Self> {
        let os = match OS {
            "linux" => "linux",
            "android" => "android",
            "macos" => "macos",
            "windows" => "windows",
            other => bail!("Unsupported platform: {other}"),
        };
        let arch = match ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            other => bail!("Unsupported architecture: {other}"),
        };
        // KernelSU CI only publishes a Windows ksud for x86_64
        // (`x86_64-pc-windows-gnu`); there is no aarch64 Windows build.
        if os == "windows" && arch != "x86_64" {
            bail!("KernelSU provides no Windows ksud build for {arch}");
        }
        // Windows executables need the `.exe` extension; this is also the name
        // of the binary inside the downloaded artifact zip (`ksud.exe`).
        let suffix = if os == "windows" { ".exe" } else { "" };

        Ok(Self {
            os,
            arch,
            suffix,
            bin_root,
        })
    }

    fn ksud_target(&self) -> &'static str {
        match (self.os, self.arch) {
            ("linux", "x86_64") => "x86_64-unknown-linux-musl",
            ("linux", "aarch64") => "aarch64-unknown-linux-musl",
            ("android", "x86_64") => "x86_64-linux-android",
            ("android", "aarch64") => "aarch64-linux-android",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("windows", "x86_64") => "x86_64-pc-windows-gnu",
            _ => unreachable!("Unsupported platform and arch {}/{}", self.os, self.arch),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/tool_tests.rs"]
mod tool_tests;
