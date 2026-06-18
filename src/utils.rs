use humantime::format_duration;
use std::fmt::Display;
use std::time::Duration;
use sysinfo::{Disks, System};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36";

pub struct Sysinfo {
    os: String,
    arch: String,
    cpu: String,
    memory: String,
    swap: String,
    disk: String,
    uptime: String,
}

impl Display for Sysinfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "OS: {} ({})\nCPU: {}\nMemory: {}\nSwap: {}\nDisk: {}\nUptime: {}",
            self.os, self.arch, self.cpu, self.memory, self.swap, self.disk, self.uptime
        )
    }
}

pub fn get_sysinfo() -> Sysinfo {
    let mut sys = System::new_all();
    let os_ver = System::long_os_version().unwrap_or_else(|| "<unknown>".to_owned());
    sys.refresh_cpu_usage();
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let disks = Disks::new_with_refreshed_list();
    let (mut disk_used, mut disk_total) = (0u64, 0u64);
    for disk in disks.list() {
        if os_ver.to_lowercase().contains("android") && disk.mount_point() != "/data" {
            continue;
        }
        disk_used += disk.total_space() - disk.available_space();
        disk_total += disk.total_space();
    }

    Sysinfo {
        os: os_ver,
        arch: System::cpu_arch(),
        cpu: format!("{}% ({})", System::load_average().one, sys.cpus().len()),
        memory: format!(
            "{}/{}G({:.2}% used)",
            sys.used_memory().clone() / 1_000_000_000,
            sys.total_memory().clone() / 1_000_000_000,
            cac_per(sys.used_memory(), sys.total_memory())
        ),
        swap: format!(
            "{}/{}G({:.2}% used)",
            sys.used_swap().clone() / 1_000_000_000,
            sys.total_swap().clone() / 1_000_000_000,
            cac_per(sys.used_swap(), sys.total_swap())
        ),
        disk: format!(
            "{}/{}G({:.2}% used)",
            disk_used / 1_000_000_000,
            disk_total / 1_000_000_000,
            cac_per(disk_used, disk_total)
        ),
        uptime: format!("{}", format_duration(Duration::from_secs(System::uptime()))),
    }
}

fn cac_per(i1: u64, i2: u64) -> f64 {
    (i1 as f64 / (i2 + 1) as f64) * 100.0
}
