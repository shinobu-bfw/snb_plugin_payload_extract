use anyhow::{Context, Result, bail};
use bytes::Bytes;
use log::{debug, error, info};
use regex::Regex;
use serde::Deserialize;
use std::env::consts::{ARCH, OS};
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::process::exit;
use zip::ZipArchive;

#[derive(Clone)]
pub struct Basis {
    os: &'static str,
    arch: &'static str,
    suffix: &'static str,
}

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
            self.get_latest().await
        }
    }
    async fn get_latest(&self) -> Result<()>;
}

#[derive(Clone)]
pub struct BaseTool {
    basis: Basis,
    name: String,
    path: PathBuf,
}

#[derive(Clone)]
pub struct Ksud(BaseTool);

#[derive(Clone)]
pub struct Magiskboot(BaseTool);

impl Tool for Ksud {
    fn from(basis: Basis) -> Self {
        let current_dir = std::env::current_dir().unwrap();

        let mut bin = current_dir.join("bin").join(basis.os).join(basis.arch);
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

    async fn get_latest(&self) -> Result<()> {
        info!("Getting latest ksud");
        self.0
            .download_and_extract(
                "tiann",
                "KernelSU",
                r"^KernelSU_v.+\.apk$",
                &format!("lib/{}/libksud.so", self.0.basis.android_abi()),
            )
            .await
    }
}

impl Tool for Magiskboot {
    fn from(basis: Basis) -> Self {
        let current_dir = std::env::current_dir().unwrap();

        let mut bin = current_dir.join("bin").join(basis.os).join(basis.arch);
        bin.push(format!("{}{}", "magiskboot", basis.suffix));
        Self(BaseTool {
            basis: basis.clone(),
            name: "magiskboot".to_string(),
            path: bin,
        })
    }

    fn get_name(&self) -> String {
        self.0.name.clone()
    }

    fn get(&self) -> PathBuf {
        self.0.path.clone()
    }

    async fn get_latest(&self) -> Result<()> {
        info!("Getting latest magiskboot");
        self.0
            .download_and_extract(
                "topjohnwu",
                "Magisk",
                r"^Magisk-v.+\.apk$",
                &format!("lib/{}/libmagiskboot.so", self.0.basis.android_abi()),
            )
            .await
    }
}

fn github_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(crate::utils::USER_AGENT)
        .build()?)
}

async fn download_bytes(url: &str) -> Result<Bytes> {
    let client = github_client()?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        bail!("Failed to download asset: {}", resp.status());
    }
    Ok(resp.bytes().await?)
}

#[derive(Deserialize)]
struct GithubRelease {
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

async fn find_latest_asset_url(owner: &str, repo: &str, asset_pattern: &Regex) -> Result<String> {
    let client = github_client()?;
    let release_api = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    let resp = client.get(&release_api).send().await?;
    if !resp.status().is_success() {
        bail!("Failed to fetch latest release metadata: {}", resp.status());
    }

    let release: GithubRelease = resp
        .json()
        .await
        .with_context(|| format!("Failed to decode latest release metadata for {owner}/{repo}"))?;

    release
        .assets
        .into_iter()
        .find(|asset| asset_pattern.is_match(&asset.name))
        .map(|asset| asset.browser_download_url)
        .with_context(|| {
            format!(
                "Asset matching `{}` not found for {owner}/{repo}",
                asset_pattern.as_str()
            )
        })
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
    async fn download_and_extract(
        &self,
        owner: &str,
        repo: &str,
        asset_pattern: &str,
        entry_name: &str,
    ) -> Result<()> {
        let asset_pattern = Regex::new(asset_pattern)?;
        let asset_url = find_latest_asset_url(owner, repo, &asset_pattern).await?;
        info!("Downloading latest {} package...", self.name);
        let body = download_bytes(&asset_url).await?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        info!("Successfully downloaded, extracting {}...", self.name);
        write_zip_entry(body, entry_name, self.path.clone())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o755))?;
        }
        info!("Download latest {} success", self.name);
        Ok(())
    }
}

#[derive(Clone)]
pub struct ToolManager {
    ksud: Ksud,
    magiskboot: Magiskboot,
}

impl Default for ToolManager {
    fn default() -> Self {
        let basis = Basis::default();
        let ksud = <Ksud as Tool>::from(basis.clone());
        let magiskboot = <Magiskboot as Tool>::from(basis.clone());
        Self { ksud, magiskboot }
    }
}

impl ToolManager {
    pub async fn init(&self) -> Result<()> {
        debug!("Initializing tools");
        self.ksud.init().await?;
        self.magiskboot.init().await?;
        Ok(())
    }

    pub async fn update(&self) -> Result<()> {
        debug!("Updating tools");
        self.ksud.get_latest().await?;
        self.magiskboot.get_latest().await?;
        Ok(())
    }

    pub fn get_magiskboot(&self) -> Magiskboot {
        self.magiskboot.clone()
    }
    pub fn get_ksud(&self) -> Ksud {
        self.ksud.clone()
    }
}

impl Default for Basis {
    fn default() -> Self {
        let os = match OS {
            "linux" => "linux",
            "android" => "android",
            _ => "Unknown",
        };
        let arch = match ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            _ => "Unknown",
        };
        if os == "Unknown" || arch == "Unknown" {
            let msg = format!("Unsupported platform and arch {OS}/{ARCH}");
            println!("{msg}");
            error!("{msg}");
            exit(1);
        }
        let suffix = if os == "Windows" { ".exe" } else { "" };

        Self { os, arch, suffix }
    }
}

impl Basis {
    fn android_abi(&self) -> &'static str {
        match self.arch {
            "x86_64" => "x86_64",
            "aarch64" => "arm64-v8a",
            _ => unreachable!("Unsupported arch: {}", self.arch),
        }
    }
}
