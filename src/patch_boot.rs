use crate::payload::dump_partition;
use crate::tool::*;
use anyhow::{Context, Result, bail};
use log::info;
use regex::Regex;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

enum PatchPartition {
    Boot,
    InitBoot,
    VendorBoot,
}

impl PatchPartition {
    fn from(s: &str) -> Result<Self> {
        match s {
            "boot" | "b" => Ok(Self::Boot),
            "init_boot" | "ib" => Ok(Self::InitBoot),
            "vendor_boot" | "vb" => Ok(Self::VendorBoot),
            _ => Err(anyhow::anyhow!("Invalid patch partition: {}", s)),
        }
    }

    fn get_partition_name(&self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::InitBoot => "init_boot",
            Self::VendorBoot => "vendor_boot",
        }
    }
}

/// Reports coarse-grained patch progress (one message per phase). Cloneable and
/// thread-safe so it can be handed to the blocking patch step.
pub type ProgressFn = Arc<dyn Fn(&str) + Send + Sync>;

struct Patch {
    tm: Arc<crate::tool::ToolManager>,
    partition: PatchPartition,
    kmi: String,
    kernel_version: String,
    progress: ProgressFn,
}

pub struct PatchedFile {
    pub(crate) path: PathBuf,
    pub(crate) kmi: String,
    pub(crate) kernel_version: String,
    pub(crate) patch_method: String,
    pub(crate) patch_version: String,
}

impl PatchedFile {
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[allow(dead_code)]
    pub fn kmi(&self) -> &str {
        &self.kmi
    }

    #[allow(dead_code)]
    pub fn kernel_version(&self) -> &str {
        &self.kernel_version
    }

    #[allow(dead_code)]
    pub fn patch_method(&self) -> &str {
        &self.patch_method
    }

    #[allow(dead_code)]
    pub fn patch_version(&self) -> &str {
        &self.patch_version
    }
}

impl Patch {
    fn patch(&self, dir: &Path) -> Result<PatchedFile> {
        let ksud = self.tm.get_ksud().get();
        let partition = self.partition.get_partition_name();
        let patched_name = format!("kernelsu_patched_{partition}-{}.img", self.kmi);
        let boot = dir.join(format!("{partition}.img"));

        (self.progress)(&format!(
            "Patching {partition} with KernelSU (KMI: {})...",
            self.kmi
        ));
        info!(
            "patching {partition} with kmi: {}, tool: {}",
            self.kmi,
            ksud.display()
        );

        // ksud >= v3.2.5 boot-patches via a pure-Rust bootimg crate (magiskboot
        // was removed upstream), so no external magiskboot is needed.
        let output = Command::new(&ksud)
            .args([
                "boot-patch",
                "--boot",
                boot.to_str()
                    .context("boot image path is not valid UTF-8")?,
                "--kmi",
                self.kmi.as_str(),
                "--out",
                dir.to_str().context("temp dir path is not valid UTF-8")?,
                "--out-name",
                patched_name.as_str(),
            ])
            .output()?;
        if !output.status.success() {
            bail!(
                "ksud boot-patch failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let file = dir.join(&patched_name);
        if !file.exists() {
            bail!(
                "ksud reported success but {} was not produced",
                file.display()
            );
        }
        Ok(PatchedFile {
            path: file,
            kmi: self.kmi.clone(),
            kernel_version: self.kernel_version.clone(),
            patch_method: "KernelSU".to_string(),
            patch_version: self.tm.get_ksud_version(),
        })
    }
}

pub async fn patch_boot(
    url: String,
    patch_partition: String,
    manual_kmi: Option<String>,
    tm: Arc<crate::tool::ToolManager>,
    progress: ProgressFn,
) -> Result<PatchedFile> {
    info!("Patching boot: {url} {patch_partition}");
    let partition = PatchPartition::from(&patch_partition)?;
    let target = partition.get_partition_name().to_string();

    // init_boot/vendor_boot carry no kernel, so the KMI is read from boot.img
    // (ksud can't auto-detect it for those). Pull boot.img too unless given a KMI.
    let mut images = vec![target];
    if manual_kmi.is_none() {
        images.push("boot".to_string());
    }
    (progress)("Downloading & extracting partitions...");
    let (_, dir) = dump_partition(url.clone(), images.join(",")).await?;

    // Everything below (KMI detection + the ksud subprocess) shares one error
    // path: on any failure, the caller must still clean up `dir`. Run it as a
    // single sub-future so `?` inside stays local to this block instead of
    // bypassing the `cleanup_temp_dir` below.
    let patched = async {
        let (kmi, kernel_version) = match manual_kmi {
            Some(kmi) => (kmi, "N/A".to_string()),
            None => {
                (progress)("Reading KMI from boot image...");
                detect_kmi(&dir.join("boot.img"))?
            }
        };

        // ksud shells out to a real subprocess (`Command::output()`), which
        // blocks the calling thread until it exits. Commands now run as async
        // tasks on snb_core's shared runtime, so blocking there would starve
        // other tasks and can't be cancelled by its shutdown drain. Move the
        // blocking call onto tokio's blocking pool instead.
        let dir_for_patch = dir.clone();
        let progress_for_patch = progress.clone();
        tokio::task::spawn_blocking(move || {
            Patch {
                tm,
                partition,
                kmi,
                kernel_version,
                progress: progress_for_patch,
            }
            .patch(&dir_for_patch)
        })
        .await
        .context("patch task panicked")?
    }
    .await;

    patched.map_err(|e| {
        cleanup_temp_dir(&dir);
        e
    })
}

fn cleanup_temp_dir(dir: &PathBuf) {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("failed to clean up {}: {e}", dir.display()),
    }
}

/// Read the kernel out of a boot image and derive (KMI, kernel version), the way
/// ksud does (same `android-bootimg` crate). Needed for init_boot/vendor_boot,
/// which carry no kernel of their own.
fn detect_kmi(boot_img: &Path) -> Result<(String, String)> {
    let data = fs::read(boot_img).with_context(|| format!("read {}", boot_img.display()))?;
    let boot = android_bootimg::parser::BootImage::parse(&data).context("parse boot image")?;
    let kernel = boot
        .get_blocks()
        .get_kernel()
        .context("no kernel in boot image; pass the KMI as the 3rd argument")?;
    let mut raw = Vec::new();
    kernel.dump(&mut raw, false).context("decompress kernel")?;

    let kmi = parse_kmi(&raw)?;
    let kernel_version = parse_kernel_version(&raw).unwrap_or_else(|| "N/A".to_string());
    Ok((kmi, kernel_version))
}

/// e.g. kernel `5.10` on `android13` -> `android13-5.10`.
fn parse_kmi(kernel: &[u8]) -> Result<String> {
    let re = Regex::new(r"(\d+\.\d+)(?:\S+)?(android\d+)")?;
    kernel
        .split(|&b| b == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .find_map(|s| {
            let caps = re.captures(s)?;
            Some(format!(
                "{}-{}",
                caps.get(2)?.as_str(),
                caps.get(1)?.as_str()
            ))
        })
        .context("could not find KMI in kernel; pass the KMI as the 3rd argument")
}

fn parse_kernel_version(kernel: &[u8]) -> Option<String> {
    let re = Regex::new(r"Linux version (.*)").ok()?;
    kernel
        .split(|&b| b == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .find_map(|s| Some(re.captures(s)?.get(1)?.as_str().trim().to_string()))
        .filter(|v| !v.is_empty())
}
