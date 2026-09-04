//! D1 P0 filesystem transaction: directory snapshot plus recoverable two-phase swap.

use std::fs;
use std::path::{Path, PathBuf};

pub struct SandboxTx {
    root: PathBuf,
    shadow: PathBuf,
    backup: PathBuf,
    finished: bool,
}

#[derive(Debug)]
pub enum SandboxError {
    InvalidRoot(PathBuf),
    StageFailed(std::io::Error),
    CommitFailed(std::io::Error),
    RollbackFailed(std::io::Error),
    RecoveryFailed(std::io::Error),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRoot(path) => write!(f, "invalid sandbox root: {}", path.display()),
            Self::StageFailed(error) => write!(f, "sandbox stage failed: {error}"),
            Self::CommitFailed(error) => write!(f, "sandbox commit failed: {error}"),
            Self::RollbackFailed(error) => write!(f, "sandbox rollback failed: {error}"),
            Self::RecoveryFailed(error) => write!(f, "sandbox recovery failed: {error}"),
        }
    }
}

impl std::error::Error for SandboxError {}

impl SandboxTx {
    pub fn stage(root: impl AsRef<Path>) -> Result<Self, SandboxError> {
        let root = root.as_ref().to_path_buf();
        if !root.is_dir() || root.parent().is_none() {
            return Err(SandboxError::InvalidRoot(root));
        }
        Self::recover(&root)?;
        let shadow = sibling(&root, "lha-shadow")?;
        let backup = sibling(&root, "lha-backup")?;
        if shadow.exists() {
            fs::remove_dir_all(&shadow).map_err(SandboxError::StageFailed)?;
        }
        copy_tree(&root, &shadow).map_err(SandboxError::StageFailed)?;
        Ok(Self {
            root,
            shadow,
            backup,
            finished: false,
        })
    }

    pub fn shadow_path(&self) -> &Path {
        &self.shadow
    }

    /// If a process died between the two renames, restore the old root. If both root and
    /// backup exist, the swap completed and only cleanup was interrupted.
    pub fn recover(root: impl AsRef<Path>) -> Result<(), SandboxError> {
        let root = root.as_ref();
        let shadow = sibling(root, "lha-shadow")?;
        let backup = sibling(root, "lha-backup")?;
        if !root.exists() && backup.exists() {
            fs::rename(&backup, root).map_err(SandboxError::RecoveryFailed)?;
        } else if root.exists() && backup.exists() {
            fs::remove_dir_all(&backup).map_err(SandboxError::RecoveryFailed)?;
        }
        if shadow.exists() {
            fs::remove_dir_all(shadow).map_err(SandboxError::RecoveryFailed)?;
        }
        Ok(())
    }

    pub fn commit(mut self) -> Result<(), SandboxError> {
        fs::rename(&self.root, &self.backup).map_err(SandboxError::CommitFailed)?;
        if let Err(error) = fs::rename(&self.shadow, &self.root) {
            let restore = fs::rename(&self.backup, &self.root);
            return match restore {
                Ok(()) => Err(SandboxError::CommitFailed(error)),
                Err(restore_error) => Err(SandboxError::RecoveryFailed(restore_error)),
            };
        }
        self.finished = true;
        if self.backup.exists() {
            // The new root is already authoritative. A cleanup interruption is recovered
            // idempotently by the next `stage`/`recover` and must not report a false failed
            // commit that could cause the caller to repeat external work.
            let _ = fs::remove_dir_all(&self.backup);
        }
        Ok(())
    }

    pub fn rollback(mut self) -> Result<(), SandboxError> {
        if self.shadow.exists() {
            fs::remove_dir_all(&self.shadow).map_err(SandboxError::RollbackFailed)?;
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for SandboxTx {
    fn drop(&mut self) {
        if !self.finished && self.shadow.exists() {
            let _ = fs::remove_dir_all(&self.shadow);
        }
    }
}

fn sibling(root: &Path, suffix: &str) -> Result<PathBuf, SandboxError> {
    let Some(name) = root.file_name() else {
        return Err(SandboxError::InvalidRoot(root.to_path_buf()));
    };
    let mut sibling_name = name.to_os_string();
    sibling_name.push(format!(".{suffix}"));
    Ok(root.with_file_name(sibling_name))
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".lha-shadow") || name.ends_with(".lha-backup") || name == "target" {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!(
                    "P0 snapshot does not follow symlink {}",
                    source_path.display()
                ),
            ));
        }
        if metadata.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lha_sandbox_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.txt"), "original").unwrap();
        root
    }

    #[test]
    fn commit_swaps_complete_snapshot() {
        let root = fixture("commit");
        let tx = SandboxTx::stage(&root).unwrap();
        fs::write(tx.shadow_path().join("src/a.txt"), "updated").unwrap();
        fs::write(tx.shadow_path().join("src/b.txt"), "new").unwrap();
        tx.commit().unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/a.txt")).unwrap(),
            "updated"
        );
        assert!(root.join("src/b.txt").exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn drop_and_rollback_leave_original_untouched() {
        let root = fixture("rollback");
        let shadow = {
            let tx = SandboxTx::stage(&root).unwrap();
            let shadow = tx.shadow_path().to_path_buf();
            fs::write(shadow.join("src/a.txt"), "updated").unwrap();
            shadow
        };
        assert!(!shadow.exists());
        assert_eq!(
            fs::read_to_string(root.join("src/a.txt")).unwrap(),
            "original"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recovery_restores_root_after_first_rename() {
        let root = fixture("recover");
        let backup = sibling(&root, "lha-backup").unwrap();
        fs::rename(&root, &backup).unwrap();
        SandboxTx::recover(&root).unwrap();
        assert!(root.join("src/a.txt").exists());
        assert!(!backup.exists());
        fs::remove_dir_all(root).ok();
    }
}
