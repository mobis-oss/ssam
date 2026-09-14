// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::package::PackagePhase;
use crate::utils::{elapsed_ns, timeline_complete_at, timeline_start_at};
use anyhow::Context as _;
use futures_util::StreamExt;
use libssam::ssam_package::PackageFile;
use std::io::Result as IoResult;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio_stream::wrappers::ReadDirStream;

pub(crate) fn has_package_extension(path: &Path, packages_ext: &str) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(packages_ext))
}

pub(crate) fn is_package_file(path: impl AsRef<Path>, packages_ext: &str) -> bool {
    let path = path.as_ref();
    path.is_file() && has_package_extension(path, packages_ext)
}

async fn dir_stream(dir: &Path) -> Option<ReadDirStream> {
    match fs::read_dir(dir).await {
        Ok(read_dir) => {
            log::debug!("Reading directory: {}", dir.display());
            Some(ReadDirStream::new(read_dir))
        }
        Err(e) => {
            log::warn!("Failed to read directory {}: {}", dir.display(), e);
            None
        }
    }
}

fn entry_to_opt_path(entry: IoResult<tokio::fs::DirEntry>) -> Option<PathBuf> {
    entry
        .inspect_err(|e| {
            log::warn!("Failed to read entry: {e}");
        })
        .ok()?
        .path()
        .into()
}

pub struct PackageParseResult {
    pub package_name: String,
    pub path: PathBuf,
    pub package_file: anyhow::Result<PackageFile>,
}

/// Parses a package file and returns the result without recording timeline events.
///
/// # Errors
///
/// Returns an error when the provided path cannot be converted into a valid
/// UTF-8 file name.
pub(crate) fn parse_package_file(
    path: PathBuf,
    public_key_file_path: impl AsRef<Path>,
    verify_signature: bool,
) -> anyhow::Result<PackageParseResult> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Failed to get filename from path: {}", path.display()))?;

    let package_file = PackageFile::from_file(&path, public_key_file_path, verify_signature)
        .context("Failed to parse package file");

    let package_name = package_file.as_ref().map_or_else(
        |_| filename,
        |parsed| parsed.metadata().get_package_name().to_owned(),
    );

    Ok(PackageParseResult {
        package_name,
        path,
        package_file,
    })
}

/// Parse a package while selecting signature verification at the parser boundary.
///
/// # Errors
///
/// Returns an error if the blocking parser task fails to start or returns a
/// package filename/path error.
pub async fn parse_package(
    path: impl AsRef<Path>,
    public_key_file_path: impl AsRef<Path>,
    verify_signature: bool,
) -> anyhow::Result<PackageParseResult> {
    let path = path.as_ref().to_path_buf();
    let public_key_file_path = public_key_file_path.as_ref().to_path_buf();
    let start_ns = elapsed_ns();
    let result = tokio::task::spawn_blocking(move || {
        parse_package_file(path, &public_key_file_path, verify_signature)
    })
    .await
    .context("parse_package task panicked")??;
    let complete_ns = elapsed_ns();

    timeline_start_at(&result.package_name, PackagePhase::Parse, start_ns);
    timeline_complete_at(&result.package_name, PackagePhase::Parse, complete_ns);

    Ok(result)
}

pub(crate) async fn parse_packages_from_dir_with_verification(
    package_dir: impl AsRef<Path>,
    packages_ext: &str,
    public_key_file_path: impl AsRef<Path>,
    verify_signature: bool,
) -> Vec<anyhow::Result<PackageParseResult>> {
    let Some(dir_entries) = dir_stream(package_dir.as_ref()).await else {
        return Vec::new();
    };
    let paths: Vec<PathBuf> = dir_entries
        .filter_map(|entry| async {
            let path = entry_to_opt_path(entry)?;
            is_package_file(&path, packages_ext).then_some(path)
        })
        .collect()
        .await;
    futures_util::future::join_all(
        paths
            .into_iter()
            .map(|path| parse_package(path, &public_key_file_path, verify_signature)),
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::io::Error;

    use super::*;
    use tokio::fs;

    /// Compile-time assertion that `PackageParseResult` is `Send`.
    /// Required for `spawn_blocking` return value to cross await boundary.
    const _: () = {
        const fn assert_send<T: Send>() {}
        assert_send::<super::PackageParseResult>();
    };

    async fn parse_packages_from_dir(
        package_dir: impl AsRef<Path>,
        packages_ext: &str,
        public_key_file_path: impl AsRef<Path>,
    ) -> Vec<anyhow::Result<PackageParseResult>> {
        super::parse_packages_from_dir_with_verification(
            package_dir,
            packages_ext,
            public_key_file_path,
            true,
        )
        .await
    }

    #[tokio::test]
    async fn test_is_package_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = "ssam";
        let pkg = tmp.path().join(format!("foo.{ext}"));
        fs::write(&pkg, b"x").await.unwrap();
        assert!(is_package_file(&pkg, ext));

        let other = tmp.path().join("bar.txt");
        fs::write(&other, b"x").await.unwrap();
        assert!(!is_package_file(&other, ext));
    }

    #[tokio::test]
    async fn test_is_package_file_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = "ssam";
        let pkg = tmp.path().join(format!("foo.{}", ext.to_uppercase()));
        fs::write(&pkg, b"x").await.unwrap();
        assert!(is_package_file(&pkg, ext));
    }

    #[tokio::test]
    async fn test_parse_package_uses_from_path() {
        let path = tempfile::NamedTempFile::new().unwrap();
        let p = path.path().to_path_buf();

        let res = parse_package(&p, "/nonexistent/test.pub.key", true)
            .await
            .expect("parse should return wrapper result");
        assert_eq!(res.path, p);
        assert!(!res.package_name.is_empty());
        assert!(res.package_file.is_err());
    }

    #[tokio::test]
    async fn test_parse_packages_from_dir_filters_non_package_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let ext = "ssam";
        let pkg = dir.join(format!("a.{ext}"));
        let other = dir.join("b.txt");
        fs::write(&pkg, b"x").await.unwrap();
        fs::write(&other, b"x").await.unwrap();

        let results =
            parse_packages_from_dir(&dir, ext, Path::new("/nonexistent/test.pub.key")).await;
        assert_eq!(results.len(), 1);
        let parsed = results[0].as_ref().expect("result should be present");
        assert!(parsed.path.ends_with(format!("a.{ext}")));
        assert!(parsed.package_file.is_err());
    }

    #[tokio::test]
    async fn test_dir_stream_returns_none_on_read_dir_error() {
        // Create a temp dir then remove it so read_dir will fail.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        // Remove directory to cause read_dir error
        std::fs::remove_dir_all(&dir).unwrap();

        let results =
            parse_packages_from_dir(&dir, "ssam", Path::new("/nonexistent/test.pub.key")).await;
        // No packages should be found because read_dir failed
        assert!(results.is_empty());
    }

    #[test]
    fn test_entry_result_to_path_logs_and_returns_none_on_err() {
        // Create an artificial io::Error and pass as Err to the helper
        let err = Error::other("boom");
        let r: IoResult<tokio::fs::DirEntry> = Err(err);
        let res = entry_to_opt_path(r);
        assert!(res.is_none());
    }
}
