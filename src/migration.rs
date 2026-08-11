//! First-run save-data migration from other DragonFable launchers.
//!
//! Flash stores SharedObjects as `#SharedObjects/<id>/<domain>/<swf-path>/<name>.sol`;
//! Ruffle's DiskStorageBackend stores the same layout under `<save_dir>/<domain>/...`
//! (no `#SharedObjects/<id>` prefix) and parses Flash's .sol format natively, so
//! migration is a straight directory copy per domain.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const DOMAINS: &[&str] = &[
    "play.dragonfable.com",
    "dragonlord.battleon.com",
    "dragonfable.battleon.com",
];

/// Host that the Evolved DragonFable Launcher stores data under (its local
/// proxy); it maps back to the real game domain.
const PROXY_HOST: &str = "127.0.0.1";

pub struct MigrationSource {
    pub id: &'static str,
    pub name: &'static str,
    /// Whether `127.0.0.1` dirs count as data (and map to `play.dragonfable.com`).
    pub include_proxy_host: bool,
    pub roots: Vec<PathBuf>,
}

/// Candidate source definitions. Reads the environment for platform dirs.
pub fn sources() -> Vec<MigrationSource> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);

    let mut flash_roots = Vec::new();
    if let Some(home) = &home {
        flash_roots.push(home.join(".macromedia").join("Flash_Player"));
    }
    if let Some(appdata) = &appdata {
        flash_roots.push(appdata.join("Macromedia").join("Flash Player"));
    }

    let launcher_roots = |app_name: &str| {
        let mut roots = Vec::new();
        if let Some(xdg_config) = &xdg_config {
            roots.push(
                xdg_config
                    .join(app_name)
                    .join("Pepper Data/Shockwave Flash/WritableRoot"),
            );
        }
        if let Some(appdata) = &appdata {
            roots.push(
                appdata
                    .join(app_name)
                    .join("Pepper Data/Shockwave Flash/WritableRoot"),
            );
        }
        roots
    };

    vec![
        MigrationSource {
            id: "flash-player",
            name: "Adobe Flash Player",
            include_proxy_host: false,
            roots: flash_roots,
        },
        MigrationSource {
            id: "artix-game-launcher",
            name: "Artix Game Launcher",
            include_proxy_host: false,
            roots: launcher_roots("Artix Game Launcher"),
        },
        MigrationSource {
            id: "evolved-dragonfable-launcher",
            name: "Evolved DragonFable Launcher",
            include_proxy_host: true,
            roots: launcher_roots("evolved-dragonfable-launcher"),
        },
    ]
}

/// Scans one source root's `#SharedObjects` tree and returns
/// `(mapped_domain, source_dir)` pairs for every domain that has data.
pub fn detect(root: &Path, include_proxy_host: bool) -> Vec<(String, PathBuf)> {
    let shared_objects = match fs::read_dir(root.join("#SharedObjects")) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut wanted: Vec<(&str, String)> = DOMAINS
        .iter()
        .map(|domain| (*domain, (*domain).to_string()))
        .collect();
    if include_proxy_host {
        wanted.push((PROXY_HOST, "play.dragonfable.com".to_string()));
    }

    let mut found = Vec::new();
    'sources: for entry in shared_objects.flatten() {
        if !entry.file_type().is_ok_and(|ty| ty.is_dir()) {
            continue;
        }
        let mut i = 0;
        while i < wanted.len() {
            let (source_host, mapped_domain) = &wanted[i];
            let domain_dir = entry.path().join(source_host);
            if has_data(&domain_dir) {
                found.push((mapped_domain.clone(), domain_dir));
                wanted.remove(i);
                if wanted.is_empty() {
                    break 'sources;
                }
            } else {
                i += 1;
            }
        }
    }
    found
}

fn has_data(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_type().is_ok_and(|ty| {
            ty.is_file() || (ty.is_dir() && has_data(&entry.path()))
        })
    })
}

/// Recursively copies each domain dir into `<save_dir>/<mapped_domain>/...`,
/// skipping any files that already exist. Returns the number of files copied.
pub fn copy_source(domain_dirs: &[(String, PathBuf)], save_dir: &Path) -> io::Result<usize> {
    let mut copied = 0;
    for (domain, source_dir) in domain_dirs {
        copied += copy_tree(source_dir, &save_dir.join(domain))?;
    }
    Ok(copied)
}

fn copy_tree(source: &Path, target: &Path) -> io::Result<usize> {
    let mut copied = 0;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copied += copy_tree(&from, &to)?;
        } else if !to.exists() {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&from, &to)?;
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &std::path::Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn flash_like_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("#SharedObjects/A4B2/play.dragonfable.com/some.sol"),
            "sol-data",
        );
        write(
            &dir.path().join("#SharedObjects/A4B2/dragonlord.battleon.com/other.sol"),
            "sol-data",
        );
        dir
    }

    #[test]
    fn detects_real_domains() {
        let dir = flash_like_root();
        let detected = detect(dir.path(), false);
        assert_eq!(detected.len(), 2);
        assert!(detected.iter().any(|(domain, _)| domain == "play.dragonfable.com"));
        assert!(detected.iter().any(|(domain, _)| domain == "dragonlord.battleon.com"));
    }

    #[test]
    fn detects_proxy_host_and_maps_it_to_play_domain() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("#SharedObjects/X1/127.0.0.1/some.sol"),
            "sol-data",
        );
        let detected = detect(dir.path(), true);
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].0, "play.dragonfable.com");
        let without_proxy = detect(dir.path(), false);
        assert!(without_proxy.is_empty());
    }

    #[test]
    fn ignores_empty_domain_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("#SharedObjects/A/play.dragonfable.com")).unwrap();
        assert!(detect(dir.path(), false).is_empty());
    }

    #[test]
    fn no_shared_objects_dir_means_no_data() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect(dir.path(), false).is_empty());
    }

    #[test]
    fn copy_strips_prefix_and_never_overwrites() {
        let src = flash_like_root();
        let save_dir = tempfile::tempdir().unwrap();
        let detected = detect(src.path(), false);

        // Pre-create one target to prove it is not overwritten.
        let target = save_dir.path().join("play.dragonfable.com/some.sol");
        write(&target, "precious");

        let copied = copy_source(&detected, save_dir.path()).unwrap();
        assert!(copied >= 1);
        assert_eq!(
            fs::read_to_string(target).unwrap(),
            "precious",
            "existing files must never be overwritten"
        );
        assert_eq!(
            fs::read_to_string(save_dir.path().join("dragonlord.battleon.com/other.sol"))
                .unwrap(),
            "sol-data"
        );
    }
}
