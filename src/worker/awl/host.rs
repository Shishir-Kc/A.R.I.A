//! Live state a script can read (`hour()`, `battery()`, `disk_free()`...).
//! It is behind a trait so the evaluator stays testable with fixed values.

use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Local};

use crate::paths::home;

pub trait Host {
    fn now(&self) -> DateTime<Local>;
    fn env(&self, name: &str) -> Option<String>;
    fn path_exists(&self, path: &str) -> bool;
    /// At most 64 KiB.
    fn read_file(&self, path: &str) -> Option<String>;
    /// Percent, or -1 without a battery.
    fn battery(&self) -> i64;
    fn on_ac(&self) -> bool;
    fn load(&self) -> f64;
    fn cpus(&self) -> i64;
    /// Percent of memory in use.
    fn mem_used(&self) -> i64;
    /// Percent of the filesystem holding `path` that is free.
    fn disk_free(&self, path: &str) -> i64;
    /// Seconds since boot.
    fn uptime(&self) -> i64;
}

/// The real machine.
pub struct SystemHost;

pub fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest).display().to_string(),
        None if path == "~" => home().display().to_string(),
        None => path.to_owned(),
    }
}

const POWER: &str = "/sys/class/power_supply";

fn supplies() -> Vec<(String, std::path::PathBuf)> {
    fs::read_dir(POWER)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let kind = fs::read_to_string(e.path().join("type")).ok()?;
                    Some((kind.trim().to_owned(), e.path()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

impl Host for SystemHost {
    fn now(&self) -> DateTime<Local> {
        Local::now()
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn path_exists(&self, path: &str) -> bool {
        Path::new(&expand_home(path)).exists()
    }

    fn read_file(&self, path: &str) -> Option<String> {
        let mut bytes = Vec::new();
        fs::File::open(expand_home(path)).ok()?.take(64 * 1024).read_to_end(&mut bytes).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn battery(&self) -> i64 {
        supplies()
            .into_iter()
            .filter(|(kind, _)| kind == "Battery")
            .find_map(|(_, path)| read_trim(&path.join("capacity"))?.parse().ok())
            .unwrap_or(-1)
    }

    fn on_ac(&self) -> bool {
        let all = supplies();
        let mut mains = all.iter().filter(|(kind, _)| kind != "Battery").peekable();
        if mains.peek().is_none() {
            // A desktop with no power-supply info is on mains power.
            return !all.iter().any(|(kind, _)| kind == "Battery")
                || all.iter().any(|(_, p)| read_trim(&p.join("status")).is_some_and(|s| s != "Discharging"));
        }
        mains.any(|(_, p)| read_trim(&p.join("online")).as_deref() == Some("1"))
    }

    fn load(&self) -> f64 {
        read_trim(Path::new("/proc/loadavg"))
            .and_then(|s| s.split_whitespace().next()?.parse().ok())
            .unwrap_or(0.0)
    }

    fn cpus(&self) -> i64 {
        std::thread::available_parallelism().map_or(1, |n| n.get() as i64)
    }

    fn mem_used(&self) -> i64 {
        let Some(info) = read_trim(Path::new("/proc/meminfo")) else { return 0 };
        let field = |name: &str| -> Option<f64> {
            info.lines().find(|l| l.starts_with(name))?.split_whitespace().nth(1)?.parse().ok()
        };
        match (field("MemTotal:"), field("MemAvailable:")) {
            (Some(total), Some(avail)) if total > 0.0 => ((total - avail) / total * 100.0).round() as i64,
            _ => 0,
        }
    }

    fn disk_free(&self, path: &str) -> i64 {
        let Ok(c_path) = CString::new(expand_home(path)) else { return 0 };
        // SAFETY: statvfs writes into the zeroed struct we own; the path is a valid C string.
        unsafe {
            let mut st: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c_path.as_ptr(), &mut st) != 0 || st.f_blocks == 0 {
                return 0;
            }
            (st.f_bavail as f64 / st.f_blocks as f64 * 100.0).round() as i64
        }
    }

    fn uptime(&self) -> i64 {
        read_trim(Path::new("/proc/uptime"))
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .map_or(0, |s| s as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_probes_return_sane_values() {
        let host = SystemHost;
        assert!((-1..=100).contains(&host.battery()));
        assert!((0..=100).contains(&host.mem_used()));
        assert!((0..=100).contains(&host.disk_free("/")));
        assert!(host.cpus() >= 1);
        assert!(host.load() >= 0.0);
        assert!(host.path_exists("/"));
    }
}
