// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::package::Package;
use libssam::ssam_package::ssam_pkg_info::BrokenPackageInfo;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub(crate) type PackagesMapType<T> = HashMap<String, Arc<T>>;
pub(crate) type BrokenPackagesMapType = HashMap<String, BrokenPackageInfo>;
pub(crate) type BundledPathsMapType = HashMap<String, PathBuf>;

/// Marker trait for testing purposes.
///
/// This trait provides no real behavior and is used in unit/integration tests
/// to identify dummy package types and to satisfy generic bounds.
pub(crate) trait PackageBound {}

impl PackageBound for Package {}

#[async_trait::async_trait]
pub(crate) trait PackageStoreBackend: Send + Sync {
    type Item: PackageBound + Send + Sync;

    async fn get(&self, name: &str) -> Option<Arc<Self::Item>>;
    async fn get_all(&self) -> PackagesMapType<Self::Item>;
    async fn insert(&self, name: String, package: Arc<Self::Item>) -> Option<Arc<Self::Item>>;
    async fn remove(&self, name: &str) -> Option<Arc<Self::Item>>;
    async fn clear(&self);
    async fn get_broken(&self, key: &str) -> Option<BrokenPackageInfo>;
    async fn get_broken_all(&self) -> BrokenPackagesMapType;
    async fn insert_broken(&self, name: String, info: BrokenPackageInfo);
    async fn remove_broken(&self, name: &str) -> Option<BrokenPackageInfo>;
    async fn clear_broken(&self);
    async fn get_bundled_path(&self, name: &str) -> Option<PathBuf>;
    async fn insert_bundled_path(&self, name: String, path: PathBuf);
    async fn clear_bundled_paths(&self);
}

pub(crate) struct HashMapPackageStore<T: PackageBound + Send + Sync> {
    packages: RwLock<PackagesMapType<T>>,
    broken_packages: RwLock<BrokenPackagesMapType>,
    bundled_packages: RwLock<BundledPathsMapType>,
}

impl<T: PackageBound + Send + Sync> HashMapPackageStore<T> {
    pub(crate) fn new() -> Self {
        Self {
            packages: RwLock::new(HashMap::new()),
            broken_packages: RwLock::new(HashMap::new()),
            bundled_packages: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl<T> PackageStoreBackend for HashMapPackageStore<T>
where
    T: PackageBound + Send + Sync,
{
    type Item = T;
    async fn get(&self, key: &str) -> Option<Arc<T>> {
        self.packages.read().await.get(key).cloned()
    }

    async fn get_all(&self) -> PackagesMapType<T> {
        self.packages.read().await.clone()
    }

    async fn insert(&self, key: String, package: Arc<T>) -> Option<Arc<T>> {
        self.packages.write().await.insert(key, package)
    }

    async fn remove(&self, key: &str) -> Option<Arc<T>> {
        self.packages.write().await.remove(key)
    }

    async fn clear(&self) {
        self.packages.write().await.clear();
    }

    async fn get_broken(&self, key: &str) -> Option<BrokenPackageInfo> {
        self.broken_packages.read().await.get(key).cloned()
    }

    async fn get_broken_all(&self) -> BrokenPackagesMapType {
        self.broken_packages.read().await.clone()
    }

    async fn insert_broken(&self, key: String, info: BrokenPackageInfo) {
        self.broken_packages.write().await.insert(key, info);
    }

    async fn remove_broken(&self, key: &str) -> Option<BrokenPackageInfo> {
        self.broken_packages.write().await.remove(key)
    }

    async fn clear_broken(&self) {
        self.broken_packages.write().await.clear();
    }

    async fn get_bundled_path(&self, name: &str) -> Option<PathBuf> {
        self.bundled_packages.read().await.get(name).cloned()
    }

    async fn insert_bundled_path(&self, name: String, path: PathBuf) {
        self.bundled_packages.write().await.insert(name, path);
    }

    async fn clear_bundled_paths(&self) {
        self.bundled_packages.write().await.clear();
    }
}

#[derive(derive_more::Deref)]
pub(crate) struct PackageStore<T: PackageStoreBackend> {
    datastore: T,
}

impl<T: PackageStoreBackend> PackageStore<T> {
    pub(crate) fn new(datastore: T) -> Self {
        Self { datastore }
    }

    pub(crate) async fn clear_all(&self) {
        self.datastore.clear().await;
        self.datastore.clear_broken().await;
        self.datastore.clear_bundled_paths().await;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;

    pub(crate) struct MockPackage;
    impl PackageBound for MockPackage {}

    #[tokio::test]
    async fn hashmap_package_store_basic() {
        let store = HashMapPackageStore::<MockPackage>::new();

        let pkg = Arc::new(MockPackage);

        // initially empty
        assert!(store.get("a").await.is_none());

        // insert
        let prev = store.insert("a".to_string(), pkg.clone()).await;
        assert!(prev.is_none());
        assert!(store.get("a").await.is_some());

        // get
        let got = store.get("a").await.expect("package should exist");
        assert!(Arc::ptr_eq(&got, &pkg));

        // get_all
        let all = store.get_all().await;
        assert_eq!(all.len(), 1);

        // remove
        let removed = store.remove("a").await;
        assert!(removed.is_some());
        assert!(store.get("a").await.is_none());

        // clear (no-op) and verify empty
        store.clear().await;
        assert_eq!(store.get_all().await.len(), 0);
    }

    #[tokio::test]
    async fn broken_package_flow() {
        let store = HashMapPackageStore::<MockPackage>::new();

        // insert broken
        store
            .insert_broken(
                "b".to_string(),
                BrokenPackageInfo {
                    package_path: "/broken/b.ssam".to_string(),
                    broken_info: libssam::ssam_package::ssam_pkg_info::BrokenReason::new(
                        "reason",
                        "detailed reason",
                    ),
                },
            )
            .await;

        // get_broken with key
        let bp = store.get_broken("b").await;
        assert_eq!(
            bp.as_ref().map(|i| i.broken_info.summary.as_str()),
            Some("reason")
        );
        assert_eq!(
            bp.as_ref().map(|i| i.package_path.as_str()),
            Some("/broken/b.ssam")
        );

        // get_broken_all
        let all_broken = store.get_broken_all().await;
        assert_eq!(all_broken.len(), 1);
        assert_eq!(
            all_broken.get("b").map(|i| i.broken_info.summary.as_str()),
            Some("reason")
        );

        // remove_broken
        let removed = store.remove_broken("b").await;
        assert_eq!(
            removed.as_ref().map(|i| i.broken_info.summary.as_str()),
            Some("reason")
        );

        // now empty
        assert!(store.get_broken("b").await.is_none());
        assert!(store.get_broken_all().await.is_empty());

        // clear_broken
        store
            .insert_broken(
                "x".to_string(),
                BrokenPackageInfo {
                    package_path: "/broken/x.ssam".to_string(),
                    broken_info: libssam::ssam_package::ssam_pkg_info::BrokenReason::new("r", "r"),
                },
            )
            .await;
        store.clear_broken().await;
        assert!(store.get_broken("x").await.is_none());
        assert!(store.get_broken_all().await.is_empty());
    }

    #[tokio::test]
    async fn package_store_clear_all() {
        let backend = HashMapPackageStore::<MockPackage>::new();
        backend.insert("p".to_string(), Arc::new(MockPackage)).await;
        backend
            .insert_broken(
                "p2".to_string(),
                BrokenPackageInfo {
                    package_path: "/broken/p2.ssam".to_string(),
                    broken_info: libssam::ssam_package::ssam_pkg_info::BrokenReason::new("r", "r"),
                },
            )
            .await;
        backend
            .insert_bundled_path("p3".to_string(), PathBuf::from("/bundled/p3.ssam"))
            .await;

        let store = PackageStore::new(backend);

        store.clear_all().await;

        assert!(store.get_all().await.is_empty());
        assert!(store.get_broken_all().await.is_empty());
        assert!(store.get_bundled_path("p3").await.is_none());
    }

    #[tokio::test]
    async fn bundled_path_flow() {
        let store = HashMapPackageStore::<MockPackage>::new();

        assert!(store.get_bundled_path("a").await.is_none());

        store
            .insert_bundled_path("a".to_string(), PathBuf::from("/bundled/a.ssam"))
            .await;
        let path = store.get_bundled_path("a").await;
        assert_eq!(
            path.as_deref(),
            Some(std::path::Path::new("/bundled/a.ssam"))
        );

        store.clear_bundled_paths().await;
        assert!(store.get_bundled_path("a").await.is_none());
    }
}
