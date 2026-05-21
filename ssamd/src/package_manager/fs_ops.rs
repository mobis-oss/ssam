// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

pub(crate) trait PackageFileBackend: Send + Sync {
    fn copy(&self, from: &Path, to: &Path) -> anyhow::Result<u64>;
    fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<()>;
    fn remove_file(&self, path: &Path) -> anyhow::Result<()>;
}

pub(crate) struct DefaultPackageFileBackend;

impl PackageFileBackend for DefaultPackageFileBackend {
    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    fn copy(&self, from: &Path, to: &Path) -> anyhow::Result<u64> {
        let dest_dir = to
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| anyhow::anyhow!("Destination has no parent directory: {to:?}"))?;

        // new_in: same directory as dest so rename stays on one filesystem (atomic).
        // If copy fails, NamedTempFile drops and auto-deletes the partial file.
        let tmp = tempfile::NamedTempFile::new_in(dest_dir)
            .map_err(|e| anyhow::anyhow!("Cannot create temp file in {dest_dir:?}: {e}"))?;

        let bytes = std::fs::copy(from, tmp.path())
            .map_err(|e| anyhow::anyhow!("Cannot copy {from:?} to {:?}: {e}", tmp.path()))?;

        // persist: rename temp -> dest atomically; dest is never partially written.
        tmp.persist(to)
            .map_err(|e| anyhow::anyhow!("Cannot rename temp file to {to:?}: {e}"))?;

        Ok(bytes)
    }

    fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<()> {
        std::fs::rename(from, to).map_err(Into::into)
    }

    fn remove_file(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::remove_file(path).map_err(Into::into)
    }
}
