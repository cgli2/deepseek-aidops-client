//! Filesystem boundaries for untrusted candidate names and payload paths.
use std::{fs, io, path::{Component, Path, PathBuf}};

pub fn safe_path(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "unsafe candidate path");
    if relative.is_empty() || relative.contains(':') || relative.contains('\\') {
        return Err(invalid());
    }
    let rel = Path::new(relative);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(invalid());
    }
    for component in rel.components() {
        let name = component.as_os_str().to_string_lossy();
        let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
        if name.ends_with(['.', ' ']) || name.chars().any(|c| c.is_control() || "<>|\"*?".contains(c))
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit()) {
            return Err(invalid());
        }
    }
    // Reject symlinks/reparse points inside the sandbox. Stop at `root`: a symlinked
    // ancestor above it (e.g. macOS `/var` → `private/var`, where `temp_dir()` lives)
    // belongs to the host environment and is not an escape of the boundary we own.
    let target = root.join(rel);
    for ancestor in target.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                #[cfg(windows)]
                let reparse = {
                    use std::os::windows::fs::MetadataExt;
                    meta.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let reparse = false;
                if meta.file_type().is_symlink() || reparse { return Err(invalid()); }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if ancestor == root { break; }
    }
    Ok(target)
}

pub fn validate_id(id: &str) -> io::Result<()> {
    if id.is_empty() || id.len() > 100 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid candidate id"));
    }
    Ok(())
}

pub fn lock(root: &Path) -> io::Result<fs::File> {
    fs::create_dir_all(root)?;
    let file = fs::OpenOptions::new().create(true).truncate(false).write(true)
        .open(safe_path(root, "state.lock")?)?;
    file.try_lock().map_err(io::Error::other)?;
    Ok(file)
}

pub fn atomic_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| io::Error::other("missing parent"))?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new().create_new(true).write(true).open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    drop(file);
    let result = fs::rename(&tmp, path);
    if result.is_err() { let _ = fs::remove_file(&tmp); }
    result
}
