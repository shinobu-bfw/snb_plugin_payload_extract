use crate::payload::dump_partition;
use crate::tool::*;
use anyhow::{Context, Result, bail};
use log::info;
use regex::Regex;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
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

struct Patch {
    tm: Arc<crate::tool::ToolManager>,
    partition: PatchPartition,
    manual_kmi: Option<String>,
}

pub struct PatchedFile {
    pub(crate) path: PathBuf,
    pub(crate) kmi: String,
    pub(crate) kernel_version: String,
    pub(crate) patch_method: String,
    pub(crate) patch_version: String,
}

impl Patch {
    fn patch(&self, dir: PathBuf) -> Result<PatchedFile> {
        let patched_name = format!(
            "kernelsu_patched_{}",
            self.partition.get_partition_name()
        );

        let ksud = self.tm.get_ksud().get();
        let magiskboot = self.tm.get_magiskboot().get();
        let (kmi, kernel_version) = match &self.manual_kmi {
            Some(kmi) => {
                info!("Using manual kmi: {}", kmi);
                (kmi.clone(), "N/A".to_string())
            }
            None => get_kmi(magiskboot.clone(), dir.clone(), "boot.img")?,
        };

        let patched_name = format!("{patched_name}-{kmi}.img");

        info!(
            "patching {} with kmi: {}, tool: {}",
            self.partition.get_partition_name(),
            kmi,
            self.tm.get_ksud().get().display()
        );

        let output = Command::new(ksud)
            .current_dir(dir.clone())
            .args([
                "boot-patch",
                "-b",
                format!("{}.img", self.partition.get_partition_name()).as_str(),
                "--magiskboot",
                magiskboot.as_path().to_str().unwrap(),
                "--kmi",
                kmi.as_str(),
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
        let mut file = dir;
        file.push(&patched_name);
        Ok(PatchedFile {
            path: file,
            kmi,
            kernel_version,
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
) -> Result<PatchedFile> {
    info!("Patching boot: {url} {patch_partition}");
    let patch = Patch {
        partition: PatchPartition::from(&patch_partition)?,
        manual_kmi,
        tm,
    };
    let mut images = vec![patch.partition.get_partition_name().to_string()];
    if patch.manual_kmi.is_none() {
        images.push("boot".to_string());
    }
    let (_, dir) = dump_partition(url.clone(), images.join(",")).await?;
    patch.patch(dir)
}

fn get_kmi(magiskboot: PathBuf, dir: PathBuf, image_name: &str) -> Result<(String, String)> {
    let kernel_version = get_kernel_version(magiskboot.clone(), dir.clone(), image_name)?;
    let file = File::open(dir.join("kernel")).context("Failed to open unpacked kernel")?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();

    reader.read_to_end(&mut buffer)?;

    let kmi_re = Regex::new(r"(?:.* )?(\d+\.\d+)(?:\S+)?(android\d+)")?;
    let mut kmi: Option<String> = None;

    let printable_strings = buffer
        .split(|&b| b == 0)
        .filter_map(|slice| std::str::from_utf8(slice).ok());

    for s in printable_strings {
        if kmi.is_none()
            && s.chars().all(|c| c.is_ascii_graphic() || c == ' ')
            && let Some(caps) = kmi_re.captures(s)
            && let (Some(kernel_version_part), Some(android_version)) = (caps.get(1), caps.get(2))
        {
            let kmi_str = format!(
                "{}-{}",
                android_version.as_str(),
                kernel_version_part.as_str()
            );
            info!("Found kmi: {}", kmi_str);
            kmi = Some(kmi_str);
        }

        if kmi.is_some() {
            break;
        }
    }

    match kmi {
        Some(k) => Ok((k, kernel_version)),
        None => Err(anyhow::anyhow!("Can't parse kmi from boot.img")),
    }
}

fn get_kernel_version(magiskboot: PathBuf, dir: PathBuf, image_name: &str) -> Result<String> {
    info!(
        "Getting kernel info from {} in {}, tool: {}",
        image_name,
        std::env::current_dir()?.display(),
        magiskboot.display()
    );
    let output = Command::new(magiskboot)
        .current_dir(&dir)
        .args(["unpack", "-n", image_name])
        .output()?;
    if !output.status.success() {
        bail!(
            "magiskboot unpack failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let file = File::open(dir.join("kernel")).context("Failed to open unpacked kernel")?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();

    reader.read_to_end(&mut buffer)?;

    let kernel_version_re = Regex::new(r"Linux version (.*)")?;

    let mut kernel_version: Option<String> = None;

    let printable_strings = buffer
        .split(|&b| b == 0)
        .filter_map(|slice| std::str::from_utf8(slice).ok());

    for s in printable_strings {
        if kernel_version.is_none()
            && let Some(caps) = kernel_version_re.captures(s)
            && let Some(version) = caps.get(1)
        {
            let kv_str = version.as_str().trim();
            if !kv_str.is_empty() {
                info!("Found kernel version: {}", kv_str);
                kernel_version = Some(kv_str.to_string());
            }
        }

        if kernel_version.is_some() {
            break;
        }
    }

    match kernel_version {
        Some(v) => Ok(v),
        None => Err(anyhow::anyhow!("Can't parse kernel version from kernel")),
    }
}
