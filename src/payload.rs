use anyhow::Result;
use log::{debug, info};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use payload_extract::extract::{ExtractConfig, extract_partitions};
use payload_extract::input::{open, open_for_extract};
use payload_extract::ota_metadata::DeviceState;
use payload_extract::proto::PartitionUpdate;

pub struct PartitionInfo {
    pub name: String,
    pub size: u64,
    pub hash: Option<String>,
    pub path: PathBuf,
}

pub async fn dump_partition(
    url: String,
    partition: String,
) -> Result<(Vec<PartitionInfo>, PathBuf)> {
    let mut partitions: Vec<String> = partition
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    partitions.sort();
    partitions.dedup();
    tokio::task::spawn_blocking(move || {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let temp_dir = PathBuf::from("tmp").join(ts.to_string());
        fs::create_dir_all(&temp_dir)?;
        info!("Dumping partitions to {}", temp_dir.display());

        let payload = open_for_extract(&url, &partitions, false)?;

        let files = payload
            .partitions()
            .iter()
            .filter(|p| partitions.iter().any(|name| name == &p.partition_name))
            .map(|partition_update| PartitionInfo {
                name: partition_update.partition_name.clone(),
                size: partition_size_bytes(partition_update),
                hash: partition_hash(partition_update),
                path: temp_dir.join(format!("{}.img", partition_update.partition_name)),
            })
            .collect::<Vec<_>>();

        let config = ExtractConfig {
            verify_ops: false,
            threads: 0,
            quiet: true,
            source_dir: None,
            out_config: None,
        };

        extract_partitions(&payload, &temp_dir, &partitions, &config)?;

        Ok((files, temp_dir))
    })
    .await?
}

pub async fn read_ota_metadata(url: String) -> Result<String> {
    info!("Reading OTA metadata: {url}");
    let data = tokio::task::spawn_blocking(move || {
        payload_extract::input::read_ota_metadata(&url, false)
    })
    .await??;

    if data.is_empty() {
        return Ok("No META-INF/com/android/metadata entries found.".to_string());
    }

    let mut out = String::new();

    if let Some(text) = &data.text {
        out.push_str("OTA Metadata (text):\n");
        for (k, v) in &text.entries {
            out.push_str(&format!("  {k}: {v}\n"));
        }
    }

    if let Some(pb) = &data.pb {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("OTA Metadata (protobuf):\n");
        out.push_str(&format!("  Type: {}\n", pb.r#type));
        out.push_str(&format!("  Wipe: {}\n", pb.wipe));
        out.push_str(&format!("  Downgrade: {}\n", pb.downgrade));
        out.push_str(&format!("  SPL downgrade: {}\n", pb.spl_downgrade));
        out.push_str(&format!(
            "  Retrofit dynamic partitions: {}\n",
            pb.retrofit_dynamic_partitions
        ));
        out.push_str(&format!("  Required cache: {}\n", pb.required_cache));

        if let Some(pre) = &pb.precondition {
            out.push_str("\n  Precondition:\n");
            push_device_state(&mut out, pre, "    ");
        }
        if let Some(post) = &pb.postcondition {
            out.push_str("\n  Postcondition:\n");
            push_device_state(&mut out, post, "    ");
        }
        if !pb.property_files.is_empty() {
            out.push_str("\n  Property files:\n");
            for (k, v) in &pb.property_files {
                out.push_str(&format!("    {k}: {v}\n"));
            }
        }
    }

    Ok(out)
}

fn push_device_state(out: &mut String, d: &DeviceState, prefix: &str) {
    if !d.device.is_empty() {
        out.push_str(&format!("{}Devices: {}\n", prefix, d.device.join(", ")));
    }
    if !d.build.is_empty() {
        out.push_str(&format!("{}Builds: {}\n", prefix, d.build.join(", ")));
    }
    if !d.build_incremental.is_empty() {
        out.push_str(&format!("{}Build incremental: {}\n", prefix, d.build_incremental));
    }
    if d.timestamp != 0 {
        out.push_str(&format!("{}Timestamp: {}\n", prefix, d.timestamp));
    }
    if !d.sdk_level.is_empty() {
        out.push_str(&format!("{}SDK level: {}\n", prefix, d.sdk_level));
    }
    if !d.security_patch_level.is_empty() {
        out.push_str(&format!(
            "{}Security patch level: {}\n",
            prefix, d.security_patch_level
        ));
    }
    for ps in &d.partition_state {
        let detail = if !ps.version.is_empty() {
            format!(" version={}", ps.version)
        } else {
            String::new()
        };
        out.push_str(&format!("{}Partition: {}{detail}\n", prefix, ps.partition_name));
        if !ps.build.is_empty() {
            out.push_str(&format!("{}  build: {}\n", prefix, ps.build.join(", ")));
        }
    }
}

pub async fn list_image(url: String) -> Result<String> {
    info!("Listing image: {url}");
    let payload = tokio::task::spawn_blocking(move || open(&url, false)).await??;
    let partitions = payload.partitions();
    let partitions_str = partitions
        .iter()
        .map(|p| {
            format!(
                "  - {}: {}",
                p.partition_name,
                format_size(partition_size_bytes(p))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let total = partitions.len() as u64;
    let size = format_size(partitions.iter().map(partition_size_bytes).sum());
    let security_patch = payload
        .manifest()
        .security_patch_level
        .as_deref()
        .unwrap_or("N/A");
    let ret = format!(
        "Total size: {size}\nSecurity patch level: {security_patch}\nTotal partitions: {total}\nPartitions:\n{partitions_str}"
    );
    debug!("{ret}");
    Ok(ret)
}

fn partition_size_bytes(partition: &PartitionUpdate) -> u64 {
    partition
        .new_partition_info
        .as_ref()
        .and_then(|info| info.size)
        .unwrap_or(0)
}

fn partition_hash(partition: &PartitionUpdate) -> Option<String> {
    partition
        .new_partition_info
        .as_ref()
        .and_then(|info| info.hash.as_ref())
        .map(|hash_bytes| {
            hash_bytes
                .iter()
                .map(|byte| format!("{:02x}", byte))
                .collect()
        })
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit_index = 0usize;
    while value >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit_index])
    }
}
