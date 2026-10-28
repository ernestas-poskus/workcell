//! Bounded reads and atomic, no-clobber writes.

use std::path::Path;

use workcell::error::Error;
use workcell::fs::{publish_once, read_bounded, write_atomic};

fn write(dir: &Path, name: &str, body: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write");
    path
}

#[tokio::test]
async fn a_bounded_read_caps_the_bytes_and_is_utf8_safe() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "doc.md", "ééé".as_bytes());
    assert_eq!(
        read_bounded(dir.path(), Path::new("doc.md"), 3)
            .await
            .expect("read"),
        "é"
    );
    assert_eq!(
        read_bounded(dir.path(), Path::new("doc.md"), 1_024)
            .await
            .expect("read"),
        "ééé"
    );
    assert_eq!(
        read_bounded(dir.path(), Path::new("doc.md"), 0)
            .await
            .expect("read"),
        ""
    );
}

#[tokio::test]
async fn a_read_outside_the_root_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside");
    write(outside.path(), "secret.md", b"secret\n");
    std::fs::write(dir.path().join("ok.md"), b"ok\n").expect("write");
    assert!(
        read_bounded(dir.path(), Path::new("ok.md"), 1_024)
            .await
            .is_ok()
    );
    for relative in ["../secret.md", "/etc/passwd", "nested/../../secret.md"] {
        let error = read_bounded(dir.path(), Path::new(relative), 1_024)
            .await
            .expect_err("refused");
        assert!(matches!(error, Error::Path { .. }), "{relative}: {error}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_the_root_is_refused_and_one_inside_is_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside");
    let secret = write(outside.path(), "secret.md", b"secret\n");
    std::os::unix::fs::symlink(&secret, dir.path().join("escape.md")).expect("symlink");
    let error = read_bounded(dir.path(), Path::new("escape.md"), 1_024)
        .await
        .expect_err("refused");
    assert!(matches!(error, Error::Path { .. }), "{error}");

    std::fs::write(dir.path().join("inside.md"), b"inside\n").expect("write");
    std::os::unix::fs::symlink("inside.md", dir.path().join("alias.md")).expect("symlink");
    assert_eq!(
        read_bounded(dir.path(), Path::new("alias.md"), 1_024)
            .await
            .expect("read"),
        "inside\n"
    );
}

#[tokio::test]
async fn read_bounded_refuses_a_directory_and_a_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("sub")).expect("mkdir");
    let error = read_bounded(dir.path(), Path::new("sub"), 1_024)
        .await
        .expect_err("directory");
    assert!(matches!(error, Error::Path { .. }), "{error}");
    let error = read_bounded(dir.path(), Path::new("missing"), 1_024)
        .await
        .expect_err("missing");
    assert!(matches!(error, Error::Io { .. }), "{error}");
}

#[tokio::test]
async fn publish_once_creates_the_file_once_and_never_clobbers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deep/nested/published.txt");
    assert!(publish_once(&path, b"first").await.expect("publish"));
    assert_eq!(std::fs::read(&path).expect("read"), b"first");
    assert!(
        !publish_once(&path, b"first")
            .await
            .expect("identical reuse")
    );
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"first",
        "an existing publish is never clobbered"
    );
    // Different bytes at the same no-clobber path are not "already published": they are content
    // the caller did not write (a tampered input or profile), so the publish fails closed.
    let error = publish_once(&path, b"second")
        .await
        .expect_err("different bytes must be refused");
    assert!(matches!(error, Error::Path { .. }), "{error}");
    assert_eq!(std::fs::read(&path).expect("read"), b"first");
    let leftovers: Vec<String> = std::fs::read_dir(path.parent().expect("dir"))
        .expect("dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn write_atomic_replaces_the_target_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested/file.txt");
    write_atomic(&path, b"first", None).await.expect("write");
    assert_eq!(std::fs::read(&path).expect("read"), b"first");
    write_atomic(&path, b"second", None).await.expect("replace");
    assert_eq!(std::fs::read(&path).expect("read"), b"second");
    let leftovers: Vec<String> = std::fs::read_dir(path.parent().expect("dir"))
        .expect("dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn write_atomic_applies_the_requested_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("executable.sh");
    write_atomic(&path, b"#!/bin/sh\n", Some(0o755))
        .await
        .expect("write");
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    write_atomic(&path, b"#!/bin/sh\n", Some(0o600))
        .await
        .expect("write");
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn a_path_without_a_file_name_is_refused() {
    let error = publish_once(Path::new("/"), b"x")
        .await
        .expect_err("no file name");
    assert!(matches!(error, Error::Path { .. }), "{error}");
    let error = write_atomic(Path::new("/"), b"x", None)
        .await
        .expect_err("no file name");
    assert!(matches!(error, Error::Path { .. }), "{error}");
}
