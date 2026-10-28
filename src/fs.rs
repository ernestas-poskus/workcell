//! Bounded reads and atomic, no-clobber writes.
//!
//! A path handed to this module is untrusted: [`read_bounded`] canonicalizes it and refuses one
//! that resolves outside `root` (including through a symlink). Writes never leave a
//! partially-written file behind: they land through a sibling temporary file, are synced, and are
//! then published either by an atomic rename ([`write_atomic`]) or by a no-clobber hard link
//! ([`publish_once`]).
//!
//! Every operation is asynchronous and runs on Tokio's filesystem thread pool. Blocking
//! filesystem calls do not belong on a multi-threaded runtime's worker threads, so the crate never
//! issues them from an async task.

use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{Error, Result};

/// Open a fresh sibling temporary file for writing.
///
/// `create_new` refuses an existing entry, so a symlink planted at the predictable temporary name
/// cannot redirect the write.
async fn create_temporary(path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
}

/// Read `relative` under `root`, refusing a path that resolves outside it and capping the bytes.
///
/// # Errors
///
/// Returns [`Error::Path`] for an absolute or non-plain relative path, and for a path whose
/// canonical target is missing or outside `root`; [`Error::Io`] when the file cannot be read.
pub async fn read_bounded(root: &Path, relative: &Path, max: usize) -> Result<String> {
    let relative_error = || Error::Path {
        detail: format!(
            "refusing `{}`: it is not a plain relative path",
            relative.display()
        ),
    };
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(relative_error());
    }
    let canonical_root = tokio::fs::canonicalize(root)
        .await
        .map_err(|error| Error::io(&root.display().to_string(), error))?;
    let candidate = root.join(relative);
    let canonical = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|error| Error::io(&candidate.display().to_string(), error))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(Error::Path {
            detail: format!(
                "refusing `{}`: it resolves outside {}",
                relative.display(),
                root.display()
            ),
        });
    }
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|error| Error::io(&canonical.display().to_string(), error))?;
    if !metadata.is_file() {
        return Err(Error::Path {
            detail: format!("refusing `{}`: not a regular file", relative.display()),
        });
    }
    let mut raw = Vec::new();
    tokio::fs::File::open(&canonical)
        .await
        .map_err(|error| Error::io(&canonical.display().to_string(), error))?
        .take(max.saturating_add(1) as u64)
        .read_to_end(&mut raw)
        .await
        .map_err(|error| Error::io(&canonical.display().to_string(), error))?;
    let lossy = String::from_utf8_lossy(&raw);
    let (text, _cut) = truncate_bytes(&lossy, max);
    Ok(text)
}

/// Publish `bytes` at `path` at most once: true when this call created the file, false when an
/// identical publish already existed.
///
/// The bytes are written and synced to a sibling temporary file and then hard-linked into place, so
/// a crash or a concurrent writer leaves either the complete file or only a temporary file — never a
/// truncated file that a later reader would trust. The temporary file is created with
/// `create_new`/`0600`, so a symlink planted at its predictable name cannot redirect the write.
///
/// # Errors
///
/// Returns [`Error::Path`] when the target already holds **different** bytes: a no-clobber publish
/// must never silently serve content another writer produced (a tampered brief or profile is
/// exactly that). Returns [`Error::Io`] when the parent directory or temporary file cannot be
/// created, the write cannot be synced, or the link fails for any other reason.
pub async fn publish_once(path: &Path, bytes: &[u8]) -> Result<bool> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| Error::io(&parent.display().to_string(), error))?;
    }
    let temp = temporary_path(path)?;
    let written = write_and_sync(&temp, bytes, Some(0o600)).await;
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(Error::io(&path.display().to_string(), error));
    }
    let published = match tokio::fs::hard_link(&temp, path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = tokio::fs::read(path)
                .await
                .map_err(|error| Error::io(&path.display().to_string(), error))?;
            if existing == bytes {
                Ok(false)
            } else {
                Err(Error::Path {
                    detail: format!(
                        "`{}` already holds different bytes; refusing to reuse a publish the caller did not make",
                        path.display()
                    ),
                })
            }
        }
        Err(error) => Err(Error::io(&path.display().to_string(), error)),
    };
    let _ = tokio::fs::remove_file(&temp).await;
    published
}

/// Write `bytes` to `path` atomically, optionally with `mode` (Unix permission bits).
///
/// The bytes go to a sibling temporary file, which is synced and then renamed over `path`; a
/// concurrent reader sees either the old file or the new one. `mode`, when given, is applied to the
/// temporary file before the rename so the published file has it.
///
/// # Errors
///
/// Returns [`Error::Io`] when the parent directory or temporary file cannot be created, the write
/// cannot be synced, the mode cannot be set, or the rename fails.
pub async fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| Error::io(&parent.display().to_string(), error))?;
    }
    let temp = temporary_path(path)?;
    let written = (|| async {
        write_and_sync(&temp, bytes, mode).await?;
        tokio::fs::rename(&temp, path).await
    })()
    .await;
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(Error::io(&path.display().to_string(), error));
    }
    Ok(())
}

/// Create `temp`, apply `mode` when given, write the bytes, and sync them to disk.
async fn write_and_sync(temp: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    let mut file = create_temporary(temp).await?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(temp, std::fs::Permissions::from_mode(mode)).await?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    file.write_all(bytes).await?;
    file.sync_all().await
}

/// The next unused temporary path beside `path`.
fn temporary_path(path: &Path) -> Result<std::path::PathBuf> {
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let file_name = path.file_name().ok_or_else(|| Error::Path {
        detail: format!("path has no file name: {}", path.display()),
    })?;
    let mut temp_name = file_name.to_os_string();
    temp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    Ok(path.with_file_name(temp_name))
}

/// Truncate a UTF-8 string to at most `max` bytes without splitting a character.
fn truncate_bytes(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_the_boundary() {
        assert_eq!(truncate_bytes("ééé", 3), ("é".to_owned(), true));
        assert_eq!(truncate_bytes("abc", 3), ("abc".to_owned(), false));
        assert_eq!(truncate_bytes("abc", 0), (String::new(), true));
    }

    #[test]
    fn a_path_without_a_file_name_has_no_temporary_sibling() {
        assert!(temporary_path(Path::new("/")).is_err());
    }
}
