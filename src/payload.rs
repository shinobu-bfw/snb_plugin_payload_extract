use crate::utils;
use anyhow::Result;
use log::{debug, info};
use payload_dumper::payload::payload_dumper::{dump_partition as payload_dump_partition, NoOpReporter};
use payload_dumper::payload::payload_parser::parse_remote_payload;
use payload_dumper::readers::remote_zip_reader::RemoteAsyncZipPayloadReader;
use payload_dumper::structs::PartitionUpdate;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use std::fs;

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
    let mut partitions: Vec<String> = partition.split(',').map(|s| s.to_string()).collect();
    partitions.sort();
    partitions.dedup();
    let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let temp_dir = PathBuf::from("tmp").join(ts.to_string());
    fs::create_dir_all(&temp_dir)?;
    info!("Dumping partitions to {}", temp_dir.display());

    let (manifest, data_offset, _) = parse_remote_payload(
        url.clone(),
        Some(utils::USER_AGENT),
        None,
        None,
    )
    .await?;
    let block_size = u64::from(manifest.block_size.unwrap_or(4096));

    let mut files = Vec::new();
    let mut tasks = Vec::new();

    for p_name in partitions {
        let out_put = temp_dir.join(format!("{p_name}.img"));
        if let Some(partition_update) = manifest
            .partitions
            .iter()
            .find(|p| p.partition_name == p_name)
            .cloned()
        {
            let info = PartitionInfo {
                name: p_name.clone(),
                size: partition_size_bytes(&partition_update),
                hash: partition_hash(&partition_update),
                path: out_put.clone(),
            };
            files.push(info);

            let url_clone = url.clone();
            tasks.push(tokio::spawn(async move {
                let reader = RemoteAsyncZipPayloadReader::new(
                    url_clone,
                    Some(utils::USER_AGENT),
                    None,
                    None,
                )
                .await?;
                let reporter = NoOpReporter;
                payload_dump_partition(
                    &partition_update,
                    data_offset,
                    block_size,
                    out_put,
                    &reader,
                    &reporter,
                    None,
                )
                .await
            }));
        }
    }

    for task in tasks {
        task.await??;
    }

    Ok((files, temp_dir))
}

pub async fn list_image(url: String) -> Result<String> {
    info!("Listing image: {url}");
    let (manifest, _, _) = parse_remote_payload(url, Some(utils::USER_AGENT), None, None).await?;
    let partitions = &manifest.partitions;
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
    let security_patch = manifest.security_patch_level.as_deref().unwrap_or("N/A");
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
        .map(|hash_bytes| hash_bytes.iter().map(|byte| format!("{:02x}", byte)).collect())
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
