use crate::payload::dump_partition;
use crate::tool::*;
use anyhow::{Context, Result, bail};
use log::info;
use regex::Regex;
use std::fmt;
use std::fs;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

enum PatchMethod {
    KernelSU,
    Magisk,
}

impl PatchMethod {
    fn from(s: &str) -> Result<Self> {
        match s {
            "kernelsu" | "ksu" | "k" => Ok(Self::KernelSU),
            "magisk" | "m" => Ok(Self::Magisk),
            _ => Err(anyhow::anyhow!("Invalid patch method: {}", s)),
        }
    }
}

impl fmt::Display for PatchMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::KernelSU => "kernelsu",
            Self::Magisk => "magisk",
        };
        f.write_str(value)
    }
}

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
    method: PatchMethod,
    partition: PatchPartition,
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
        let mut patched_name = format!(
            "{}_patched_{}",
            self.method,
            self.partition.get_partition_name()
        );

        match &self.method {
            PatchMethod::KernelSU => {
                let ksud = self.tm.get_ksud().get();
                let magiskboot = self.tm.get_magiskboot().get();
                let (kmi, kernel_version) = get_kmi(magiskboot.clone(), dir.clone(), "boot.img")?;

                patched_name = format!("{patched_name}-{kmi}.img");

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
            PatchMethod::Magisk => {
                let magiskboot = self.tm.get_magiskboot();
                let image_name = format!("{}.img", self.partition.get_partition_name());
                let patch_version = self.tm.get_magisk_version();

                prepare_magisk_patch_dir(magiskboot.clone(), dir.clone())?;

                let output = Command::new("bash")
                    .current_dir(dir.clone())
                    .arg("-lc")
                    .arg(format!(
                        "set -e; export BOOTMODE=true SOURCEDMODE=true ABI='{}' API='34'; . ./util_functions.sh; . ./boot_patch.sh '{}'",
                        magiskboot.get_android_abi(),
                        image_name
                    ))
                    .output()?;

                if !output.status.success() {
                    bail!(
                        "magisk boot_patch failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }

                let new_image = find_magisk_patched_image(&dir, &image_name)?;
                patched_name = format!("{patched_name}.img");
                let patched_path = dir.join(&patched_name);
                fs::rename(new_image, &patched_path)?;

                let (kmi, kernel_version) = if matches!(self.partition, PatchPartition::Boot) {
                    get_kmi(magiskboot.get(), dir.clone(), "boot.img")
                        .unwrap_or_else(|_| ("N/A".to_string(), "N/A".to_string()))
                } else {
                    ("N/A".to_string(), "N/A".to_string())
                };

                Ok(PatchedFile {
                    path: patched_path,
                    kmi,
                    kernel_version,
                    patch_method: "Magisk".to_string(),
                    patch_version,
                })
            }
        }
    }
}

pub async fn patch_boot(
    url: String,
    patch_partition: String,
    patch_method: String,
    tm: Arc<crate::tool::ToolManager>,
) -> Result<PatchedFile> {
    info!("Patching boot: {url} {patch_partition} {patch_method}");
    let patch = Patch {
        method: PatchMethod::from(&patch_method)?,
        partition: PatchPartition::from(&patch_partition)?,
        tm,
    };
    let mut images = Vec::new();
    images.push(patch.partition.get_partition_name().to_string());
    if let PatchMethod::KernelSU = patch.method {
        images.push("boot".to_string());
    }
    let (_, dir) = dump_partition(url.clone(), images.join(",")).await?;
    patch.patch(dir)
}

fn prepare_magisk_patch_dir(magiskboot: Magiskboot, dir: PathBuf) -> Result<()> {
    let magisk_dir = magiskboot.get_magisk_dir();
    for file in [
        ("magiskboot", magiskboot.get()),
        ("magiskinit", magisk_dir.join("magiskinit")),
        ("magisk", magisk_dir.join("magisk")),
        ("init-ld", magisk_dir.join("init-ld")),
        ("stub.apk", magisk_dir.join("stub.apk")),
        ("boot_patch.sh", magisk_dir.join("boot_patch.sh")),
        ("util_functions.sh", magisk_dir.join("util_functions.sh")),
    ] {
        fs::copy(file.1, dir.join(file.0))?;
    }

    let chromeos_dir = dir.join("chromeos");
    fs::create_dir_all(&chromeos_dir)?;
    for file in ["futility", "kernel.keyblock", "kernel_data_key.vbprivk"] {
        fs::copy(
            magisk_dir.join("chromeos").join(file),
            chromeos_dir.join(file),
        )?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [
            dir.join("magiskboot"),
            dir.join("magiskinit"),
            dir.join("magisk"),
            dir.join("init-ld"),
            dir.join("boot_patch.sh"),
            dir.join("util_functions.sh"),
            dir.join("chromeos").join("futility"),
        ] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
    }

    Ok(())
}

fn find_magisk_patched_image(dir: &PathBuf, image_name: &str) -> Result<PathBuf> {
    let candidates = [
        dir.join(format!("new-{image_name}")),
        dir.join("new-boot.img"),
        dir.join("new-init_boot.img"),
        dir.join("new-vendor_boot.img"),
    ];

    candidates
        .into_iter()
        .find(|path| path.exists())
        .context("Magisk patched image not found")
}

fn get_kmi(magiskboot: PathBuf, dir: PathBuf, image_name: &str) -> Result<(String, String)> {
    info!(
        "Getting kmi from {} in {}, tool: {}",
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

    let kmi_re = Regex::new(r"(?:.* )?(\d+\.\d+)(?:\S+)?(android\d+)")?;
    let kernel_version_re = Regex::new(r"Linux version (.*)")?;

    let mut kmi: Option<String> = None;
    let mut kernel_version: Option<String> = None;

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

        if kmi.is_some() && kernel_version.is_some() {
            break;
        }
    }

    match (kmi, kernel_version) {
        (Some(k), Some(v)) => Ok((k, v)),
        (Some(_), None) => Err(anyhow::anyhow!("Can't parse kernel version from kernel")),
        (None, Some(_)) => Err(anyhow::anyhow!("Can't parse kmi from boot.img")),
        (None, None) => Err(anyhow::anyhow!(
            "Can't parse kmi and kernel version from boot.img"
        )),
    }
}
