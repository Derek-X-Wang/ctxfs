use ctxfs_core::Digest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub source: String,
    pub commit_sha: String,
    pub root_directory: Digest,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Directory {
    pub digest: Digest,
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DirEntry {
    File(FileEntry),
    Directory(DirectoryEntry),
    Symlink(SymlinkEntry),
}

impl DirEntry {
    pub fn name(&self) -> &str {
        match self {
            DirEntry::File(f) => &f.name,
            DirEntry::Directory(d) => &d.name,
            DirEntry::Symlink(s) => &s.name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub digest: Digest,
    pub size: u64,
    pub executable: bool,
    /// Inline content for small files (<=4KB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub digest: Digest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymlinkEntry {
    pub name: String,
    pub target: String,
}

impl Directory {
    /// Compute and return the digest for this directory by serializing to JSON + SHA-256.
    pub fn compute_digest(entries: &[DirEntry]) -> Digest {
        let json = serde_json::to_vec(entries).expect("DirEntry serialization should not fail");
        Digest::sha256(&json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxfs_core::Digest;

    #[test]
    fn dir_entry_name() {
        let f = DirEntry::File(FileEntry {
            name: "hello.txt".to_string(),
            digest: Digest::sha256(b"content"),
            size: 7,
            executable: false,
            inline_content: None,
        });
        assert_eq!(f.name(), "hello.txt");
    }

    #[test]
    fn directory_digest_deterministic() {
        let entries = vec![DirEntry::File(FileEntry {
            name: "a.txt".to_string(),
            digest: Digest::sha256(b"aaa"),
            size: 3,
            executable: false,
            inline_content: Some(b"aaa".to_vec()),
        })];
        let d1 = Directory::compute_digest(&entries);
        let d2 = Directory::compute_digest(&entries);
        assert_eq!(d1, d2);
    }

    #[test]
    fn snapshot_serde_roundtrip() {
        let snap = Snapshot {
            source: "github:octocat/Hello-World@master".to_string(),
            commit_sha: "abc123".to_string(),
            root_directory: Digest::sha256(b"root"),
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let snap2: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap.commit_sha, snap2.commit_sha);
        assert_eq!(snap.source, snap2.source);
        assert_eq!(snap.created_at, snap2.created_at);
        assert_eq!(snap.root_directory, snap2.root_directory);
    }

    #[test]
    fn dir_entry_name_for_directory() {
        let d = DirEntry::Directory(DirectoryEntry {
            name: "src".to_string(),
            digest: Digest::sha256(b"dir"),
        });
        assert_eq!(d.name(), "src");
    }

    #[test]
    fn dir_entry_name_for_symlink() {
        let s = DirEntry::Symlink(SymlinkEntry {
            name: "link".to_string(),
            target: "/usr/bin/foo".to_string(),
        });
        assert_eq!(s.name(), "link");
    }

    #[test]
    fn directory_serde_roundtrip() {
        let dir = Directory {
            digest: Digest::sha256(b"dir"),
            entries: vec![
                DirEntry::File(FileEntry {
                    name: "readme.md".to_string(),
                    digest: Digest::sha256(b"readme"),
                    size: 100,
                    executable: false,
                    inline_content: Some(b"# Hello".to_vec()),
                }),
                DirEntry::Directory(DirectoryEntry {
                    name: "src".to_string(),
                    digest: Digest::sha256(b"src"),
                }),
                DirEntry::Symlink(SymlinkEntry {
                    name: "link".to_string(),
                    target: "readme.md".to_string(),
                }),
            ],
        };

        let json = serde_json::to_string(&dir).unwrap();
        let dir2: Directory = serde_json::from_str(&json).unwrap();

        assert_eq!(dir.digest, dir2.digest);
        assert_eq!(dir.entries.len(), dir2.entries.len());
        assert_eq!(dir.entries[0].name(), "readme.md");
        assert_eq!(dir.entries[1].name(), "src");
        assert_eq!(dir.entries[2].name(), "link");
    }

    #[test]
    fn different_entries_different_digests() {
        let entries1 = vec![DirEntry::File(FileEntry {
            name: "a.txt".to_string(),
            digest: Digest::sha256(b"aaa"),
            size: 3,
            executable: false,
            inline_content: None,
        })];
        let entries2 = vec![DirEntry::File(FileEntry {
            name: "b.txt".to_string(),
            digest: Digest::sha256(b"bbb"),
            size: 3,
            executable: false,
            inline_content: None,
        })];

        let d1 = Directory::compute_digest(&entries1);
        let d2 = Directory::compute_digest(&entries2);
        assert_ne!(d1, d2);
    }

    #[test]
    fn empty_directory_digest() {
        let entries: Vec<DirEntry> = vec![];
        let d = Directory::compute_digest(&entries);
        // Should be a valid digest even for empty dirs
        assert!(!d.hex.is_empty());
        assert_eq!(d.hex.len(), 64); // SHA-256 hex length
    }

    #[test]
    fn file_entry_inline_content_optional() {
        let entry = FileEntry {
            name: "big.bin".to_string(),
            digest: Digest::sha256(b"big"),
            size: 1_000_000,
            executable: false,
            inline_content: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        // inline_content should be skipped when None
        assert!(!json.contains("inline_content"));

        let entry2: FileEntry = serde_json::from_str(&json).unwrap();
        assert!(entry2.inline_content.is_none());
    }

    #[test]
    fn dir_entry_tagged_serde() {
        // Verify the tagged enum serializes with "type" field
        let entry = DirEntry::File(FileEntry {
            name: "test".to_string(),
            digest: Digest::sha256(b"t"),
            size: 1,
            executable: false,
            inline_content: None,
        });
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"file\""));

        let dir_entry = DirEntry::Directory(DirectoryEntry {
            name: "dir".to_string(),
            digest: Digest::sha256(b"d"),
        });
        let json = serde_json::to_string(&dir_entry).unwrap();
        assert!(json.contains("\"type\":\"directory\""));

        let sym_entry = DirEntry::Symlink(SymlinkEntry {
            name: "sym".to_string(),
            target: "target".to_string(),
        });
        let json = serde_json::to_string(&sym_entry).unwrap();
        assert!(json.contains("\"type\":\"symlink\""));
    }
}
