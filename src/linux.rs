//! Reading and managing the Linux host: storage, RAM, caches, installed apps.
//! Sizes are in GB unless noted.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use sysinfo::{Disks, System};

use crate::error::{AriaError, Result};
use crate::logger::Logger;
use crate::paths::{home, log_file};

static LOG: LazyLock<Logger> =
    LazyLock::new(|| Logger::new("Linux.system", Some(log_file("Linux/system.log"))));

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StorageInfo {
    pub total: u64,
    pub free: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RamInfo {
    pub total: f64,
    pub free: f64,
    pub used: f64,
    pub available: f64,
    pub swap_total: f64,
    pub swap_used: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheEntry {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheInfo {
    pub application_cache: Vec<CacheEntry>,
    pub total_bytes: u64,
}

pub struct Linux {
    pub home: PathBuf,
    pub application_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Default for Linux {
    fn default() -> Self {
        let home = home();
        Self {
            cache_dir: home.join(".cache"),
            application_dir: PathBuf::from("/usr/share/applications"),
            home,
        }
    }
}

impl Linux {
    /// Storage of the root filesystem, in whole GB.
    pub fn storage(&self) -> Option<StorageInfo> {
        let disks = Disks::new_with_refreshed_list().ok()?;
        let root = disks.iter().find(|d| d.mount_point() == Path::new("/"))?;
        let (total, free) = (root.total_space(), root.available_space());
        Some(StorageInfo {
            total: total >> 30,
            free: free >> 30,
            used: total.saturating_sub(free) >> 30,
        })
    }

    pub fn show_ram_info(&self) -> Result<RamInfo> {
        LOG.info("Getting OS ram info");
        let mut sys = System::new().map_err(|e| AriaError::Other(format!("could not read system info: {e}")))?;
        sys.refresh_memory();
        let gb = |bytes: u64| bytes as f64 / GIB;
        Ok(RamInfo {
            total: gb(sys.total_memory()),
            free: gb(sys.free_memory()),
            used: gb(sys.used_memory()),
            available: gb(sys.available_memory()),
            swap_total: gb(sys.total_swap()),
            swap_used: gb(sys.used_swap()),
        })
    }

    /// The `range_of` biggest entries of the cache directory, biggest first.
    pub fn cache_storage_usage(&self, range_of: usize) -> Vec<CacheEntry> {
        LOG.info(format!("Getting applications with cache usage of range {range_of}"));
        let mut entries: Vec<CacheEntry> = self
            .get_cache()
            .into_iter()
            .map(|path| CacheEntry { bytes: disk_usage(&path), path })
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.bytes));
        entries.truncate(range_of);
        entries
    }

    /// Installed applications (`*.desktop` files).
    pub fn get_apps(&self) -> Vec<PathBuf> {
        LOG.info("Getting applications from desktop");
        let mut apps: Vec<PathBuf> = list_dir(&self.application_dir)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
            .collect();
        apps.sort();
        apps
    }

    /// Everything in the cache directory.
    pub fn get_cache(&self) -> Vec<PathBuf> {
        list_dir(&self.cache_dir)
    }

    /// Deletes the 25 biggest cache entries. Returns what was removed; entries
    /// that could not be removed are logged and skipped.
    pub fn delete_cache(&self) -> Result<Vec<CacheEntry>> {
        LOG.info("Getting cache");
        let mut removed = Vec::new();
        for entry in self.cache_storage_usage(25) {
            LOG.info(format!(
                "Removing cache of {} | cache hold {}",
                entry.path.display(),
                human_size(entry.bytes)
            ));
            let is_dir = fs::symlink_metadata(&entry.path).map(|m| m.is_dir()).unwrap_or(false);
            let result = if is_dir { fs::remove_dir_all(&entry.path) } else { fs::remove_file(&entry.path) };
            match result {
                Ok(()) => removed.push(entry),
                Err(e) => LOG.error(format!("could not remove {}: {e}", entry.path.display())),
            }
        }
        Ok(removed)
    }

    pub fn show_cache_info(&self) -> CacheInfo {
        LOG.info("Getting storage occupied by cache");
        CacheInfo {
            application_cache: self.cache_storage_usage(25),
            total_bytes: disk_usage(&self.cache_dir),
        }
    }
}

fn list_dir(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Disk space used by `path` and everything below it, like `du -s` (allocated
/// blocks, symlinks not followed). Unreadable entries count as zero.
pub fn disk_usage(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&current) else { continue };
        total += meta.blocks() * 512;
        if meta.is_dir() {
            stack.extend(list_dir(&current));
        }
    }
    total
}

/// Formats bytes the way `du -h` does: `0`, `4.0K`, `12M`, `1.5G`.
pub fn human_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0".into();
    }
    const UNITS: [&str; 5] = ["K", "M", "G", "T", "P"];
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{:.1}{}", (value * 10.0).ceil() / 10.0, UNITS[unit])
    } else {
        format!("{}{}", value.ceil(), UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Linux) {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join(".cache");
        fs::create_dir_all(cache.join("big/nested")).unwrap();
        fs::create_dir_all(cache.join("small")).unwrap();
        fs::write(cache.join("big/nested/blob"), vec![1u8; 200_000]).unwrap();
        fs::write(cache.join("small/blob"), vec![1u8; 10_000]).unwrap();
        fs::create_dir_all(dir.path().join("apps")).unwrap();
        fs::write(dir.path().join("apps/b.desktop"), "").unwrap();
        fs::write(dir.path().join("apps/a.desktop"), "").unwrap();
        fs::write(dir.path().join("apps/readme.txt"), "").unwrap();
        let linux = Linux {
            home: dir.path().into(),
            application_dir: dir.path().join("apps"),
            cache_dir: cache,
        };
        (dir, linux)
    }

    #[test]
    fn human_sizes_match_du() {
        assert_eq!(human_size(0), "0");
        assert_eq!(human_size(4096), "4.0K");
        assert_eq!(human_size(12 * 1024 * 1024), "12M");
        assert_eq!(human_size(1536 * 1024 * 1024), "1.5G");
        assert_eq!(human_size(1025), "1.1K");
    }

    #[test]
    fn cache_usage_is_sorted_and_limited() {
        let (_dir, linux) = fixture();
        let usage = linux.cache_storage_usage(25);
        assert_eq!(usage.len(), 2);
        assert!(usage[0].path.ends_with("big"));
        assert!(usage[0].bytes >= 200_000 && usage[0].bytes > usage[1].bytes);
        assert_eq!(linux.cache_storage_usage(1).len(), 1);
        assert!(linux.show_cache_info().total_bytes >= usage[0].bytes + usage[1].bytes);
    }

    #[test]
    fn delete_cache_empties_the_cache_dir() {
        let (_dir, linux) = fixture();
        assert_eq!(linux.delete_cache().unwrap().len(), 2);
        assert!(linux.get_cache().is_empty());
        assert!(linux.cache_dir.exists());
    }

    #[test]
    fn apps_are_desktop_files_only() {
        let (_dir, linux) = fixture();
        let names: Vec<_> =
            linux.get_apps().iter().map(|p| p.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["a.desktop", "b.desktop"]);
    }

    #[test]
    fn ram_and_storage_are_sane() {
        let linux = Linux::default();
        let ram = linux.show_ram_info().unwrap();
        assert!(ram.total > 0.0 && ram.used <= ram.total);
        if let Some(storage) = linux.storage() {
            assert!(storage.used <= storage.total);
        }
    }
}
