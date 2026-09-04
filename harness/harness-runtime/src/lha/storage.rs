//! Crash-recoverable local file primitives shared by DRLA stores.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use uuid::Uuid;

pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "file has no parent")
    })?;
    fs::create_dir_all(parent)?;
    recover_atomic(path)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "file name is not UTF-8")
        })?;
    let temporary = parent.join(format!(".{name}.{}.tmp", Uuid::new_v4()));
    let backup = backup_path(path)?;
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);

    if path.exists() {
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() && !path.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if backup.exists() {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

pub fn recover_atomic(path: &Path) -> std::io::Result<()> {
    let backup = backup_path(path)?;
    if !path.exists() && backup.exists() {
        fs::rename(&backup, path)?;
    } else if path.exists() && backup.exists() {
        fs::remove_file(backup)?;
    }
    cleanup_temporary_siblings(path)
}

/// Remove the one incomplete JSONL record left by a crash so future appends start on a
/// clean line. A complete final record always ends in `\n` because writers sync it as one unit.
pub fn repair_jsonl_tail(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let bytes = fs::read(path)?;
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        return Ok(());
    }
    let valid_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    let file = fs::OpenOptions::new().write(true).open(path)?;
    file.set_len(valid_len as u64)?;
    file.sync_all()
}

fn backup_path(path: &Path) -> std::io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "file has no name"))?;
    let mut backup = name.to_os_string();
    backup.push(".lha-backup");
    Ok(path.with_file_name(backup))
}

fn cleanup_temporary_siblings(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return Ok(());
    };
    let prefix = format!(".{name}.");
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with(&prefix) && file_name.ends_with(".tmp") {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_backup_and_truncates_jsonl_tail() {
        let root = std::env::temp_dir().join(format!("lha_storage_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.json");
        let backup = backup_path(&path).unwrap();
        fs::write(&backup, "old").unwrap();
        recover_atomic(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "old");
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");

        let wal = root.join("events.jsonl");
        fs::write(&wal, b"{\"ok\":1}\n{\"partial\"").unwrap();
        repair_jsonl_tail(&wal).unwrap();
        assert_eq!(fs::read(&wal).unwrap(), b"{\"ok\":1}\n");
        fs::remove_dir_all(root).ok();
    }
}
