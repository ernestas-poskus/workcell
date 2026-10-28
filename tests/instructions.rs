//! Instruction-file reads at the trust boundary: caps, confinement, symlinks, and missing files.

use std::path::Path;

use workcell::instructions::{Bundle, InstructionSpec, load};

fn spec(files: &[&str], max_bytes: usize, max_file_bytes: usize) -> InstructionSpec {
    InstructionSpec {
        files: files.iter().map(|file| (*file).to_owned()).collect(),
        max_bytes,
        max_file_bytes,
    }
}

fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write");
    path
}

#[tokio::test]
async fn configured_files_load_in_order_under_their_configured_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "AGENTS.md", "root rules\n");
    write(dir.path(), "nested/AGENTS.md", "nested rules\n");
    let bundle = load(
        dir.path(),
        &spec(&["AGENTS.md", "nested/AGENTS.md"], 1_024, 1_024),
    )
    .await;
    assert!(bundle.configured());
    assert!(bundle.missing.is_empty() && bundle.omitted.is_empty());
    assert_eq!(bundle.files.len(), 2);
    assert_eq!(bundle.files[0].path, "AGENTS.md");
    assert_eq!(bundle.files[0].body, "root rules\n");
    assert_eq!(bundle.files[1].path, "nested/AGENTS.md");
    assert_eq!(bundle.files[1].body, "nested rules\n");
    assert!(!bundle.total_truncated);
}

#[tokio::test]
async fn a_missing_file_is_reported_and_never_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "AGENTS.md", "rules\n");
    let bundle = load(
        dir.path(),
        &spec(&["missing.md", "AGENTS.md"], 1_024, 1_024),
    )
    .await;
    assert_eq!(bundle.missing, vec!["missing.md".to_owned()]);
    assert_eq!(bundle.files.len(), 1);
}

#[tokio::test]
async fn an_absolute_or_traversing_path_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    for path in [
        "../secret.md",
        "/etc/passwd",
        "./AGENTS.md",
        "nested/../AGENTS.md",
    ] {
        let bundle = load(dir.path(), &spec(&[path], 1_024, 1_024)).await;
        assert!(bundle.files.is_empty(), "{path} loaded");
        assert_eq!(bundle.missing, vec![path.to_owned()]);
        assert_eq!(bundle.looked_for, vec![path.to_owned()]);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_the_checkout_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside");
    let secret = write(outside.path(), "secret.md", "host secret\n");
    std::os::unix::fs::symlink(&secret, dir.path().join("AGENTS.md")).expect("symlink");
    let bundle = load(dir.path(), &spec(&["AGENTS.md"], 1_024, 1_024)).await;
    assert!(bundle.files.is_empty(), "{bundle:?}");
    assert_eq!(bundle.missing, vec!["AGENTS.md".to_owned()]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_inside_the_checkout_is_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "rules.md", "rules\n");
    std::os::unix::fs::symlink("rules.md", dir.path().join("AGENTS.md")).expect("symlink");
    let bundle = load(dir.path(), &spec(&["AGENTS.md"], 1_024, 1_024)).await;
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.files[0].body, "rules\n");
}

#[tokio::test]
async fn the_per_file_and_total_caps_share_one_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "A.md", "123456");
    write(dir.path(), "B.md", "abcdef");
    let bundle = load(dir.path(), &spec(&["A.md", "B.md"], 8, 8)).await;
    assert_eq!(bundle.files.len(), 2);
    assert_eq!(bundle.files[0].body, "123456");
    assert_eq!(bundle.files[1].body, "ab");
    assert!(bundle.files[1].truncated);
    assert!(bundle.total_truncated);
}

#[tokio::test]
async fn a_cap_cuts_on_a_utf8_character_boundary() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "AGENTS.md", "ééé");
    let bundle = load(dir.path(), &spec(&["AGENTS.md"], 1_024, 3)).await;
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.files[0].body, "é");
    assert!(bundle.files[0].truncated);
}

#[tokio::test]
async fn a_file_beyond_the_total_cap_is_omitted() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "A.md", "12345678\n");
    write(dir.path(), "B.md", "abcdef\n");
    let mut bundle: Bundle = load(dir.path(), &spec(&["A.md", "B.md"], 8, 8)).await;
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.omitted, vec!["B.md".to_owned()]);
    assert!(bundle.total_truncated);
    // The field is public so a caller can render what happened.
    bundle.looked_for.push("C.md".to_owned());
    assert!(bundle.configured());
}

#[tokio::test]
async fn duplicate_paths_collapse_to_one_section() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "AGENTS.md", "rules\n");
    let bundle = load(
        dir.path(),
        &spec(&["AGENTS.md", " AGENTS.md "], 1_024, 1_024),
    )
    .await;
    assert_eq!(bundle.looked_for, vec!["AGENTS.md"]);
    assert_eq!(bundle.files.len(), 1);
}
