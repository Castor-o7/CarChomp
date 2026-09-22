//! Host health for the dashboard. Everything is read from /proc and /sys, so
//! on anything but Linux the fields are simply null.

use serde_json::{Value, json};
use std::{fs, path::Path};

pub fn snapshot(data_dir: &Path) -> Value {
    let read = |path: &str| fs::read_to_string(path).ok();
    let first_number = |path: &str| read(path)?.split_whitespace().next()?.parse::<f64>().ok();
    let meminfo = read("/proc/meminfo").unwrap_or_default();
    let mem_kb = |key: &str| meminfo.lines().find_map(|l| l.strip_prefix(key)?.split_whitespace().next()?.parse::<u64>().ok());
    let os = read("/etc/os-release").and_then(|s| Some(s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME="))?.trim_matches('"').to_owned()));
    let disk = rustix::fs::statvfs(data_dir).ok();

    json!({
        "os": os,
        "kernel": read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_owned()),
        "uptime_s": first_number("/proc/uptime"),
        "load_1m": first_number("/proc/loadavg"),
        "cpu_temp_c": first_number("/sys/class/thermal/thermal_zone0/temp").map(|milli| milli / 1000.0),
        "mem_total_bytes": mem_kb("MemTotal:").map(|kb| kb * 1024),
        "mem_available_bytes": mem_kb("MemAvailable:").map(|kb| kb * 1024),
        "disk_total_bytes": disk.as_ref().map(|d| d.f_blocks * d.f_frsize),
        "disk_free_bytes": disk.as_ref().map(|d| d.f_bavail * d.f_frsize),
    })
}
