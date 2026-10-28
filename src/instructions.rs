//! Bounded, root-confined reads of a checkout's instruction files.
//!
//! A coding agent is conditioned by the repository's own rules (`AGENTS.md` and its siblings).
//! Reading them for a prompt is a trust boundary: the paths are relative to the checkout, resolve
//! through [`tokio::fs::canonicalize`], and must stay under the checkout root so a repository
//! symlink cannot pull a host secret into a model prompt. Files are capped per file and in total,
//! truncated on a UTF-8 character boundary, and a missing file is reported rather than fatal.
//!
//! The read is deliberately conservative in the same way the rest of the crate is: unsafe
//! spellings (an absolute path, a parent traversal, a `.` component) are refused even though the
//! loader that normally supplies the list validates them, because the loader is public.

use std::path::{Component, Path};

use tokio::io::AsyncReadExt;

/// How to read the checkout's instruction files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSpec {
    /// The configured paths, relative to the checkout root.
    pub files: Vec<String>,
    /// Per-file byte cap.
    pub max_file_bytes: usize,
    /// Total byte cap.
    pub max_bytes: usize,
}

/// One instruction file as a prompt will render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    /// The configured path, exactly as the prompt should name it.
    pub path: String,
    /// The (possibly truncated) UTF-8 body.
    pub body: String,
    /// The file's size before any cap was applied.
    pub original_bytes: u64,
    /// Whether the per-file or total cap shortened this body.
    pub truncated: bool,
}

/// Every configured instruction file, plus what could not be read or did not fit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bundle {
    /// The configured paths, in order, whether or not each one loaded.
    pub looked_for: Vec<String>,
    /// The files that loaded, in configured order.
    pub files: Vec<InstructionFile>,
    /// The configured paths that could not be read safely.
    pub missing: Vec<String>,
    /// The configured paths the total cap left no room for.
    pub omitted: Vec<String>,
    /// Whether the total cap cut a file short or omitted one.
    pub total_truncated: bool,
}

impl Bundle {
    /// Whether the specification asked for any files at all.
    #[must_use]
    pub fn configured(&self) -> bool {
        !self.looked_for.is_empty()
    }
}

/// Read the configured instruction files under `root`.
///
/// No error is returned: a missing or unreadable file is reported in the bundle, never fatal.
#[must_use]
pub async fn load(root: &Path, spec: &InstructionSpec) -> Bundle {
    let mut seen = std::collections::HashSet::new();
    let mut looked_for: Vec<String> = Vec::new();
    for configured in &spec.files {
        let path = configured.trim();
        let relative = Path::new(path);
        // Unsafe spellings must still reach read_one and be reported missing.
        let safe = !relative.is_absolute()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
        let key = if safe {
            tokio::fs::canonicalize(root.join(relative))
                .await
                .unwrap_or_else(|_| root.join(relative))
        } else {
            root.join(relative)
        };
        if seen.insert((safe, key)) {
            looked_for.push(path.to_owned());
        }
    }
    if looked_for.is_empty() {
        return Bundle::default();
    }

    let canonical_root = tokio::fs::canonicalize(root).await.ok();
    let mut files = Vec::new();
    let mut missing = Vec::new();
    let mut omitted = Vec::new();
    let mut used = 0usize;
    let mut total_truncated = false;

    for configured in &looked_for {
        if used >= spec.max_bytes {
            omitted.push(configured.clone());
            total_truncated = true;
            continue;
        }
        match read_one(root, canonical_root.as_deref(), configured, spec, used).await {
            Ok((file, consumed, total_cut)) => {
                used = used.saturating_add(consumed);
                total_truncated |= total_cut;
                files.push(file);
            }
            Err(()) => missing.push(configured.clone()),
        }
    }

    Bundle {
        looked_for,
        files,
        missing,
        omitted,
        total_truncated,
    }
}

/// Read one file.
///
/// Returns the file, the bytes it consumed from the total cap, and whether the total cap (rather
/// than the per-file cap) is what shortened it. `Err(())` means the path could not be read safely.
async fn read_one(
    root: &Path,
    canonical_root: Option<&Path>,
    configured: &str,
    spec: &InstructionSpec,
    used: usize,
) -> Result<(InstructionFile, usize, bool), ()> {
    // Defence in depth: the configuration loader validates these, but the loader is public.
    let relative = Path::new(configured);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(());
    }

    let canonical = tokio::fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| ())?;
    let root = canonical_root.ok_or(())?;
    if !canonical.starts_with(root) {
        return Err(());
    }
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|_| ())?;
    if !metadata.is_file() {
        return Err(());
    }

    let original_bytes = metadata.len();
    let remaining = spec.max_bytes.saturating_sub(used);
    if remaining == 0 {
        return Err(());
    }
    let cap = spec.max_file_bytes.min(remaining);
    let mut raw = Vec::new();
    tokio::fs::File::open(&canonical)
        .await
        .map_err(|_| ())?
        .take(cap.saturating_add(1) as u64)
        .read_to_end(&mut raw)
        .await
        .map_err(|_| ())?;
    let read_all = raw.len() <= cap;
    let lossy = String::from_utf8_lossy(&raw);
    let (text, cut_at_boundary) = truncate_bytes(&lossy, cap);
    let consumed = text.len();
    let truncated = !read_all || cut_at_boundary || (original_bytes as usize) > consumed;
    // The total cap is the limiting factor only when it is below the per-file cap (otherwise the
    // per-file cap would have shortened the file anyway).
    let total_cut = (original_bytes as usize) > remaining && remaining < spec.max_file_bytes;

    Ok((
        InstructionFile {
            path: configured.to_owned(),
            body: text,
            original_bytes,
            truncated,
        },
        consumed,
        total_cut,
    ))
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

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, body).expect("write");
        path
    }

    fn spec(files: &[&str], max_bytes: usize, max_file_bytes: usize) -> InstructionSpec {
        InstructionSpec {
            files: files.iter().map(|file| (*file).to_owned()).collect(),
            max_bytes,
            max_file_bytes,
        }
    }

    #[tokio::test]
    async fn duplicate_instruction_paths_collapse_to_one_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "AGENTS.md", "rules\n");
        let single = load(dir.path(), &spec(&["AGENTS.md"], 1024, 1024)).await;
        let duplicated = load(dir.path(), &spec(&["AGENTS.md", "AGENTS.md"], 1024, 1024)).await;
        assert_eq!(duplicated, single);
    }

    #[tokio::test]
    async fn surrounding_whitespace_in_a_configured_path_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "AGENTS.md", "rules  \n\n");
        let single = load(dir.path(), &spec(&["AGENTS.md"], 1024, 1024)).await;
        let padded = load(dir.path(), &spec(&[" \tAGENTS.md\r\n"], 1024, 1024)).await;
        assert_eq!(padded, single);
        assert_eq!(padded.files[0].body, "rules  \n\n");
    }

    #[tokio::test]
    async fn configured_paths_are_trimmed_and_duplicates_do_not_consume_budget() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "A.md", "aaa");
        write(dir.path(), "B.md", "bbb");
        let bundle = load(dir.path(), &spec(&[" A.md ", "A.md", "B.md"], 6, 6)).await;
        assert_eq!(bundle.looked_for, vec!["A.md", "B.md"]);
        assert_eq!(bundle.files.len(), 2);
        assert_eq!(bundle.files[1].body, "bbb");
        assert!(!bundle.total_truncated);
    }

    #[tokio::test]
    async fn missing_paths_are_reported_and_resolved_aliases_deduplicate() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "A.md", "rules");
        #[cfg(unix)]
        std::os::unix::fs::symlink("A.md", dir.path().join("alias.md")).expect("symlink");
        #[cfg(not(unix))]
        write(dir.path(), "alias.md", "rules");
        let bundle = load(
            dir.path(),
            &spec(&["missing.md", "A.md", "alias.md"], 1024, 1024),
        )
        .await;
        assert_eq!(bundle.missing, vec!["missing.md"]);
        #[cfg(unix)]
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.files[0].path, "A.md");
    }

    #[tokio::test]
    async fn files_load_in_configured_order_with_their_exact_bodies() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "AGENTS.md", "root rules\n");
        write(dir.path(), "crate/AGENTS.md", "crate rules\n");
        let bundle = load(
            dir.path(),
            &spec(&["AGENTS.md", "crate/AGENTS.md"], 1024, 1024),
        )
        .await;
        assert_eq!(bundle.looked_for.len(), 2);
        assert!(bundle.missing.is_empty());
        assert_eq!(bundle.files.len(), 2);
        assert_eq!(bundle.files[0].path, "AGENTS.md");
        assert_eq!(bundle.files[0].body, "root rules\n");
        assert_eq!(bundle.files[1].path, "crate/AGENTS.md");
        assert_eq!(bundle.files[1].body, "crate rules\n");
        assert!(!bundle.total_truncated);
    }

    #[tokio::test]
    async fn a_missing_file_is_reported_and_the_rest_still_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "AGENTS.md", "root rules\n");
        let bundle = load(dir.path(), &spec(&["AGENTS.md", "missing.md"], 1024, 1024)).await;
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.missing, vec!["missing.md".to_owned()]);
    }

    #[tokio::test]
    async fn an_empty_file_list_is_no_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle = load(dir.path(), &spec(&[], 1024, 1024)).await;
        assert!(!bundle.configured());
        assert!(bundle.files.is_empty());
    }

    #[tokio::test]
    async fn unsafe_paths_are_refused_even_when_the_loader_is_called_directly() {
        let dir = tempfile::tempdir().expect("tempdir");
        for path in ["../outside.md", "/etc/passwd", "./inside.md"] {
            let bundle = load(dir.path(), &spec(&[path], 1024, 1024)).await;
            assert!(bundle.files.is_empty(), "{path} must not load");
            assert_eq!(bundle.missing, vec![path.to_owned()]);
        }
    }

    #[tokio::test]
    async fn a_symlink_that_resolves_outside_the_checkout_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        let secret = write(outside.path(), "secret.md", "do not send me\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, dir.path().join("AGENTS.md")).expect("symlink");
        #[cfg(not(unix))]
        std::fs::copy(&secret, dir.path().join("AGENTS.md")).expect("copy");
        let bundle = load(dir.path(), &spec(&["AGENTS.md"], 1024, 1024)).await;
        #[cfg(unix)]
        {
            assert!(bundle.files.is_empty());
            assert_eq!(bundle.missing, vec!["AGENTS.md".to_owned()]);
        }
        #[cfg(not(unix))]
        {
            assert_eq!(bundle.files.len(), 1, "the copy is inside the root");
        }
    }

    #[tokio::test]
    async fn a_per_file_cap_truncates_on_a_character_boundary() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "AGENTS.md", "ééé");
        let bundle = load(dir.path(), &spec(&["AGENTS.md"], 1024, 3)).await;
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.files[0].body, "é");
        assert!(bundle.files[0].truncated);
    }

    #[tokio::test]
    async fn the_total_cap_is_shared_across_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "AGENTS.md", "123456");
        write(dir.path(), "SECOND.md", "abcdef");
        let bundle = load(dir.path(), &spec(&["AGENTS.md", "SECOND.md"], 8, 8)).await;
        assert_eq!(bundle.files.len(), 2);
        assert_eq!(bundle.files[0].body, "123456");
        assert_eq!(bundle.files[1].body, "ab");
        assert!(bundle.files[1].truncated);
        assert!(bundle.total_truncated);
    }

    #[tokio::test]
    async fn a_file_exactly_at_the_cap_is_not_marked_truncated() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "AGENTS.md", "1234");
        let bundle = load(dir.path(), &spec(&["AGENTS.md"], 4, 4)).await;
        assert_eq!(bundle.files[0].body, "1234");
        assert!(!bundle.files[0].truncated);
    }
}
