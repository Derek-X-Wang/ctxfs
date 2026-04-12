use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ctxfs_core::Backend;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountStateEntry {
    pub source: String,
    pub volume_path: String,
    pub symlink_paths: Vec<String>,
    pub backend: Backend,
    pub tcp_port: Option<u16>,
    pub auth_token: Option<String>,
}

#[derive(Debug)]
pub struct MountStateFile {
    path: PathBuf,
}

impl MountStateFile {
    /// Create a new `MountStateFile` handle. The actual file is at `base_dir/mounts.json`.
    pub fn new(base_dir: &Path) -> Self {
        Self {
            path: base_dir.join("mounts.json"),
        }
    }

    /// Return the path to the state file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read all entries. Returns an empty `Vec` when the file is absent or corrupt.
    pub fn read(&self) -> Vec<MountStateEntry> {
        let Ok(data) = fs::read(&self.path) else {
            return Vec::new();
        };
        serde_json::from_slice(&data).unwrap_or_default()
    }

    /// Atomically write `entries` to disk: write a sibling `.tmp` file, fsync, then rename.
    pub fn write(&self, entries: &[MountStateEntry]) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(entries)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Ensure parent directory exists.
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp_path = self.path.with_extension("json.tmp");
        {
            use std::io::Write as _;
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(&data)?;
            file.sync_all()?;
        }
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    /// Append a new entry to the state file.
    pub fn add(&self, entry: MountStateEntry) -> io::Result<()> {
        let mut entries = self.read();
        entries.push(entry);
        self.write(&entries)
    }

    /// Add a symlink path to the entry identified by `volume_path`.
    pub fn add_symlink(&self, volume_path: &str, symlink: &str) -> io::Result<()> {
        let mut entries = self.read();
        for entry in &mut entries {
            if entry.volume_path == volume_path {
                if !entry.symlink_paths.contains(&symlink.to_string()) {
                    entry.symlink_paths.push(symlink.to_string());
                }
                return self.write(&entries);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no mount entry for volume_path '{volume_path}'"),
        ))
    }

    /// Remove a symlink from whichever entry owns it.
    ///
    /// Returns `true` when the owning entry now has no remaining symlinks (caller
    /// may choose to clean it up with [`remove_volume`]).
    pub fn remove_symlink(&self, symlink: &str) -> io::Result<bool> {
        let mut entries = self.read();
        let mut empty = false;
        let mut found = false;
        for entry in &mut entries {
            if let Some(pos) = entry.symlink_paths.iter().position(|s| s == symlink) {
                drop(entry.symlink_paths.remove(pos));
                empty = entry.symlink_paths.is_empty();
                found = true;
                break;
            }
        }
        if !found {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("symlink '{symlink}' not found in any mount entry"),
            ));
        }
        self.write(&entries)?;
        Ok(empty)
    }

    /// Remove the entry whose `volume_path` matches the given value.
    pub fn remove_volume(&self, volume_path: &str) -> io::Result<()> {
        let mut entries = self.read();
        let before = entries.len();
        entries.retain(|e| e.volume_path != volume_path);
        if entries.len() == before {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no mount entry for volume_path '{volume_path}'"),
            ));
        }
        self.write(&entries)
    }

    /// Remove all entries from the state file.
    pub fn clear(&self) -> io::Result<()> {
        self.write(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

    fn tmp_state() -> (TempDir, MountStateFile) {
        let dir = TempDir::new().unwrap();
        let msf = MountStateFile::new(dir.path());
        (dir, msf)
    }

    fn entry(source: &str, volume_path: &str, symlinks: &[&str]) -> MountStateEntry {
        MountStateEntry {
            source: source.to_string(),
            volume_path: volume_path.to_string(),
            symlink_paths: symlinks.iter().map(|s| (*s).to_string()).collect(),
            backend: Backend::Nfs,
            tcp_port: Some(2049),
            auth_token: None,
        }
    }

    #[test]
    fn read_nonexistent_returns_empty() {
        let (_dir, msf) = tmp_state();
        let entries = msf.read();
        assert!(entries.is_empty());
    }

    #[test]
    fn write_and_read_roundtrip() {
        let (_dir, msf) = tmp_state();
        let e = entry("github:owner/repo@main", "/tmp/vol1", &["/usr/local/lib/repo"]);
        msf.write(&[e.clone()]).unwrap();
        let got = msf.read();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "github:owner/repo@main");
        assert_eq!(got[0].volume_path, "/tmp/vol1");
        assert_eq!(got[0].symlink_paths, vec!["/usr/local/lib/repo"]);
        assert_eq!(got[0].backend, Backend::Nfs);
        assert_eq!(got[0].tcp_port, Some(2049));
        assert!(got[0].auth_token.is_none());
    }

    #[test]
    fn add_symlink_to_entry() {
        let (_dir, msf) = tmp_state();
        let e = entry("github:owner/repo@main", "/tmp/vol2", &["/link/a"]);
        msf.add(e).unwrap();
        msf.add_symlink("/tmp/vol2", "/link/b").unwrap();
        let got = msf.read();
        assert_eq!(got[0].symlink_paths, vec!["/link/a", "/link/b"]);
    }

    #[test]
    fn remove_symlink_returns_empty_flag() {
        let (_dir, msf) = tmp_state();
        // Two symlinks — removing one should return false (entry still has links).
        let e = entry("github:owner/repo@v1", "/tmp/vol3", &["/link/x", "/link/y"]);
        msf.add(e).unwrap();

        let empty = msf.remove_symlink("/link/x").unwrap();
        assert!(!empty);
        let got = msf.read();
        assert_eq!(got[0].symlink_paths, vec!["/link/y"]);

        // Remove last symlink — should return true.
        let empty = msf.remove_symlink("/link/y").unwrap();
        assert!(empty);
        let got = msf.read();
        assert!(got[0].symlink_paths.is_empty());
    }

    #[test]
    fn remove_volume() {
        let (_dir, msf) = tmp_state();
        msf.add(entry("github:a/b@main", "/tmp/vA", &["/la"])).unwrap();
        msf.add(entry("github:c/d@main", "/tmp/vB", &["/lb"])).unwrap();
        msf.remove_volume("/tmp/vA").unwrap();
        let got = msf.read();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].volume_path, "/tmp/vB");
    }

    #[test]
    fn atomic_write_survives_corruption() {
        let (_dir, msf) = tmp_state();

        // Write valid state first so the file exists.
        msf.write(&[entry("github:e/f@main", "/tmp/vC", &[])]).unwrap();

        // Corrupt the file in-place.
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(msf.path())
                .unwrap();
            f.write_all(b"NOT VALID JSON !!!").unwrap();
        }

        // read() must return empty, not panic.
        assert!(msf.read().is_empty());

        // write() must succeed on a corrupted file.
        let fresh = entry("github:g/h@v2", "/tmp/vD", &["/ld"]);
        msf.write(&[fresh]).unwrap();
        let got = msf.read();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "github:g/h@v2");
    }

    #[test]
    fn clear_removes_all() {
        let (_dir, msf) = tmp_state();
        msf.add(entry("github:i/j@main", "/tmp/vE", &["/le"])).unwrap();
        msf.add(entry("github:k/l@main", "/tmp/vF", &["/lf"])).unwrap();
        msf.clear().unwrap();
        assert!(msf.read().is_empty());
    }
}
