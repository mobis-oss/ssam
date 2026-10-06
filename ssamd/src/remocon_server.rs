// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub(crate) mod auth;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt as _;

use crate::configuration::Configuration;
use crate::package::PackageStatus as InternalPackageStatus;
use crate::utils::ellipsis;
use anyhow::Context as _;
use libssam::{
    PackageActionResult, PackageInfo, PackageStatusEntry,
    remocon::{
        API_VERSION_METADATA_KEY, InspectLocalPackageResult, InstallPackageRequest,
        InstallPackageResponse, ListPackageInfoFromFilesRequest, ListPackageInfoFromFilesResponse,
        ListPackageInfoRequest, ListPackageInfoResponse, ListPackagesResponse,
        RemovePackageRequest, StartPackageRequest, StartPackageResponse, StopPackageRequest,
        StopPackageResponse, TimelineData, TimelineEvent, TimelineEventKind,
        WipePackageDataRequest, WipePackageDataResponse, api_version_string,
        remocon_server::Remocon,
    },
};

use tonic::{Request, Response, Status};

use crate::package_manager::PackageManagerService;

pub(crate) struct RemoconImpl {
    package_manager: Arc<dyn PackageManagerService>,
    config: Arc<Configuration>,
}

fn build_install_response(
    metadata: &libssam::ssam_package::ssam_pkg_metadata::PackageMetadata,
) -> InstallPackageResponse {
    InstallPackageResponse::from(metadata)
}

/// Rejects requests whose API version metadata is missing, duplicated, or mismatched.
pub(crate) fn validate_api_version(request: Request<()>) -> Result<Request<()>, Status> {
    let expected = api_version_string();
    let mut values = request.metadata().get_all(API_VERSION_METADATA_KEY).iter();

    // Only one metadata value is allowed, so inspect two `next` results to catch duplicates.
    if let (Some(value), None) = (values.next(), values.next()) {
        if !value.to_str().is_ok_and(|value| value == expected) {
            let received = String::from_utf8_lossy(value.as_ref())
                .escape_debug()
                .to_string();
            return Err(Status::failed_precondition(format!(
                "Invalid {API_VERSION_METADATA_KEY} metadata value [{received}]; expected API version {expected}"
            )));
        }
    } else {
        return Err(Status::failed_precondition(format!(
            "{API_VERSION_METADATA_KEY} metadata must contain exactly one value; expected API version {expected}"
        )));
    }

    Ok(request)
}

impl RemoconImpl {
    pub(crate) fn new(
        package_manager: Arc<dyn PackageManagerService>,
        config: Arc<Configuration>,
    ) -> Self {
        Self {
            package_manager,
            config,
        }
    }
}

impl RemoconImpl {
    async fn resolve_package_names(&self, names: Vec<String>) -> HashSet<String> {
        if names.is_empty() {
            HashSet::from_iter(self.package_manager.get_package_names().await)
        } else {
            HashSet::from_iter(names)
        }
    }
}

fn into_package_op_result(name: String, outcome: anyhow::Result<()>) -> PackageActionResult {
    match outcome {
        Ok(()) => PackageActionResult {
            package_name: name,
            success: true,
            message: None,
        },
        Err(e) => PackageActionResult {
            package_name: name,
            success: false,
            message: Some(format!("{e}")),
        },
    }
}

fn package_info_failure(name: String, error: &anyhow::Error) -> PackageInfo {
    PackageInfo {
        success: false,
        name,
        status: String::new(),
        package_data: None,
        quota: None,
        package_path: String::new(),
        error_summary: Some(error.to_string()),
        error_details: Some(format!("{error:?}")),
    }
}

fn package_info_from_file_failure(path: &Path, error: &anyhow::Error) -> InspectLocalPackageResult {
    InspectLocalPackageResult {
        package_data: None,
        package_path: path.to_string_lossy().into_owned(),
        success: false,
        error_summary: Some(error.to_string()),
        error_details: Some(format!("{error:?}")),
    }
}

fn into_proto_timeline_event(event: ssam_log::TimelineEvent) -> TimelineEvent {
    TimelineEvent {
        pkg: event.pkg,
        phase: event.phase,
        duration_ns: event.duration_ns,
        kind: match event.kind {
            ssam_log::TimelineEventKind::Started => TimelineEventKind::Started as i32,
            ssam_log::TimelineEventKind::Completed => TimelineEventKind::Completed as i32,
        },
    }
}

#[tonic::async_trait]
impl Remocon for RemoconImpl {
    async fn start_package(
        &self,
        request: Request<StartPackageRequest>,
    ) -> Result<Response<StartPackageResponse>, Status> {
        let package_names = request.into_inner().package_names;
        log::debug!("Received request to start packages: {package_names:?}");

        let names = self.resolve_package_names(package_names).await;
        let package_manager = &self.package_manager;
        let results = futures_util::future::join_all(names.into_iter().map(|name| async move {
            let outcome = package_manager
                .start_package(&name)
                .await
                .inspect_err(|e| log::error!("Failed to start package {name}: {e:#}"));
            into_package_op_result(name, outcome)
        }))
        .await;

        Ok(Response::new(StartPackageResponse { results }))
    }

    async fn stop_package(
        &self,
        request: Request<StopPackageRequest>,
    ) -> Result<Response<StopPackageResponse>, Status> {
        let package_names = request.into_inner().package_names;
        log::debug!("Received request to stop packages: {package_names:?}");

        let names = self.resolve_package_names(package_names).await;
        let package_manager = &self.package_manager;
        let results = futures_util::future::join_all(names.into_iter().map(|name| async move {
            let outcome = package_manager
                .stop_package(&name)
                .await
                .inspect_err(|e| log::error!("Failed to stop package {name}: {e:#}"));
            into_package_op_result(name, outcome)
        }))
        .await;

        Ok(Response::new(StopPackageResponse { results }))
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    async fn install_package(
        &self,
        request: Request<InstallPackageRequest>,
    ) -> Result<Response<InstallPackageResponse>, Status> {
        log::debug!(
            "Received request to install package: {:?}",
            request.get_ref().package_path
        );
        let package_path = &request.get_ref().package_path;
        let downgrade = request.get_ref().downgrade;
        let remove_data = request.get_ref().remove_data;

        let path = PathBuf::from(package_path);
        path.is_absolute().ok_or_else(|| {
            Status::invalid_argument(format!("Package path must be absolute: {path:?}"))
        })?;

        let result_inner = self
            .package_manager
            .install_package(path, downgrade, remove_data)
            .await;

        match result_inner {
            Ok(metadata) => Ok(Response::new(build_install_response(&metadata))),
            Err(e) => {
                log::error!("Failed to install package: {e:?}");
                Err(Status::internal("Failed to install package"))
            }
        }
    }

    #[allow(clippy::redundant_closure_for_method_calls)]
    async fn remove_package(
        &self,
        request: Request<RemovePackageRequest>,
    ) -> Result<Response<()>, Status> {
        log::debug!("Received request to remove package");
        let package_name = &request.get_ref().package_name;

        let result_inner = self
            .package_manager
            .remove_package(package_name, true)
            .await;

        match result_inner {
            Ok(()) => Ok(Response::new(())),
            Err(e)
                if e.chain()
                    .any(|cause| cause.is::<crate::package_manager::PackageNotFound>()) =>
            {
                Err(Status::not_found(e.to_string()))
            }
            Err(e) => {
                log::error!("Failed to remove package {package_name}: {e:?}");
                Err(Status::internal(format!(
                    "Failed to remove package: {package_name}"
                )))
            }
        }
    }

    async fn wipe_package_data(
        &self,
        request: Request<WipePackageDataRequest>,
    ) -> Result<Response<WipePackageDataResponse>, Status> {
        let package_names = request.into_inner().package_names;
        log::debug!("Received request to wipe package data: {package_names:?}");

        let names = self.resolve_package_names(package_names).await;
        let results = futures_util::stream::iter(names)
            .fold(Vec::new(), |mut results, name| async {
                let outcome = self
                    .package_manager
                    .wipe_package_data(&name)
                    .await
                    .inspect_err(|e| log::error!("Failed to wipe data for package {name}: {e:#}"));
                results.push(into_package_op_result(name, outcome));
                results
            })
            .await;

        Ok(Response::new(WipePackageDataResponse { results }))
    }

    async fn list_packages(
        &self,
        _request: Request<()>,
    ) -> Result<Response<ListPackagesResponse>, Status> {
        log::debug!("Received request to list packages");
        let status = self.package_manager.get_packages_status().await;

        match status {
            Ok(packages) => {
                let entries: Vec<PackageStatusEntry> = packages
                    .into_iter()
                    .map(|(package_name, status)| {
                        // Truncate long broken reasons for display in list view
                        let status_str = match &status {
                            InternalPackageStatus::Broken(info) => {
                                format!("Broken({})", ellipsis(&format!("{info}")))
                            }
                            _ => status.to_string(),
                        };
                        PackageStatusEntry {
                            package_name,
                            status: status_str,
                        }
                    })
                    .collect();
                Ok(Response::new(ListPackagesResponse { entries }))
            }
            Err(e) => {
                log::error!("Failed to list packages: {e:?}");
                Err(Status::internal("Failed to list packages"))
            }
        }
    }

    async fn get_timeline_info(
        &self,
        _request: Request<()>,
    ) -> Result<Response<TimelineData>, Status> {
        log::debug!("Received request to get timeline info");
        let events = ssam_log::get_timelines()
            .into_iter()
            .map(into_proto_timeline_event)
            .collect();
        let ssamd_uptime = match (*crate::SSAMD_UPTIME).try_into() {
            Ok(uptime) => uptime,
            Err(e) => {
                log::error!("ssamd uptime does not fit in prost_types::Duration: {e:?}");
                return Err(Status::internal("Failed to get timeline info"));
            }
        };

        Ok(Response::new(TimelineData {
            events,
            ssamd_uptime: Some(ssamd_uptime),
        }))
    }

    async fn list_package_info(
        &self,
        request: Request<ListPackageInfoRequest>,
    ) -> Result<Response<ListPackageInfoResponse>, Status> {
        log::debug!("Received request to get package info");
        let package_names = request.into_inner().package_names;
        let list_all = package_names.is_empty();
        let package_names = package_names
            .into_iter()
            .filter(|name| !name.trim().is_empty())
            .collect::<Vec<_>>();
        let package_info: Result<Vec<PackageInfo>, anyhow::Error> = if list_all {
            self.package_manager
                .get_all_package_info()
                .await
                .map(|infos| infos.into_iter().map(Into::into).collect())
        } else {
            let packages = futures_util::stream::iter(package_names)
                .then(|name| async move {
                    match self.package_manager.get_package_info(&name).await {
                        Ok(info) => info.into(),
                        Err(e) => {
                            log::error!("Failed to get package info for {name}: {e:?}");
                            package_info_failure(name, &e)
                        }
                    }
                })
                .collect::<Vec<_>>()
                .await;
            Ok(packages)
        };

        match package_info {
            Ok(packages) => Ok(Response::new(ListPackageInfoResponse { packages })),
            Err(e) => {
                log::error!("Failed to get package info: {e:?}");
                Err(Status::internal("Failed to get package info"))
            }
        }
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    async fn list_package_info_from_files(
        &self,
        request: Request<ListPackageInfoFromFilesRequest>,
    ) -> Result<Response<ListPackageInfoFromFilesResponse>, Status> {
        log::debug!("Received request to inspect local package files");
        let package_paths = request.into_inner().package_paths;
        let public_key_path = self.config.public_key_path().to_owned();
        let verify_signature = !self.config.package_verify_disabled();
        let packages = futures_util::stream::iter(package_paths)
            .then(|package_path| async {
                let package_path = PathBuf::from(package_path);
                if !package_path.is_absolute() {
                    let error = anyhow::anyhow!("Package path must be absolute: {package_path:?}");
                    log::error!("{error}");
                    return package_info_from_file_failure(&package_path, &error);
                }

                // Accept regular files with the expected package extension only — a
                // mismatched extension can never install, so reject it here too.
                let has_package_ext = crate::package_manager::parser::has_package_extension(
                    &package_path,
                    self.config.packages_ext(),
                );
                if let Ok(meta) = tokio::fs::metadata(&package_path).await
                    && (!meta.is_file() || !has_package_ext)
                {
                    let error = anyhow::anyhow!(
                        "Package path is not a valid package file: {package_path:?}"
                    );
                    log::error!("{error}");
                    return package_info_from_file_failure(&package_path, &error);
                }

                let pkg_path = package_path.clone();
                let public_key_path = public_key_path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    crate::package_manager::parser::parse_package_file(
                        pkg_path,
                        Path::new(&public_key_path),
                        verify_signature,
                    )
                    .and_then(|r| r.package_file)
                })
                .await
                .context("list_package_info_from_files task panicked")
                .and_then(|r| r);

                match result {
                    Ok(package_file) => {
                        let metadata = package_file.metadata();
                        InspectLocalPackageResult {
                            package_data: Some(metadata.into()),
                            package_path: package_path.to_string_lossy().into_owned(),
                            success: true,
                            error_summary: None,
                            error_details: None,
                        }
                    }
                    Err(e) => {
                        let error =
                            e.context(format!("Failed to inspect local package {package_path:?}"));
                        log::error!("{error}");
                        package_info_from_file_failure(&package_path, &error)
                    }
                }
            })
            .collect::<Vec<_>>()
            .await;

        Ok(Response::new(ListPackageInfoFromFilesResponse { packages }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::package::PackageStatus as PackageStatusEnum;
    use crate::package_manager::PackageManagerService;

    use async_trait::async_trait;
    use libssam::PackageInfo;
    use libssam::remocon::ApiVersion;
    use libssam::ssam_package::PackageFsVerityInfo;
    use libssam::ssam_package::ssam_pkg_info::{
        BrokenPackageInfo, BrokenReason, PackageInfo as PackageStateInfo, PackageInfoResult,
        QuotaInformation,
    };
    use libssam::ssam_package::ssam_pkg_metadata::PackageMetadata;
    use libssam::superblock::FsType;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::sync::Barrier;
    use tonic::metadata::MetadataValue;
    use tonic::{Code, Request};

    type InstallResultMap = Arc<Mutex<HashMap<String, Result<PackageMetadata, anyhow::Error>>>>;

    #[derive(Debug, Clone)]
    pub(crate) struct MockPackageManagerService {
        pub packages: Arc<Mutex<HashMap<String, PackageStatusEnum>>>,
        pub package_infos: Arc<Mutex<HashMap<String, PackageStateInfo>>>,
        pub install_results: InstallResultMap,
        pub remove_results: Arc<Mutex<HashMap<String, Result<(), anyhow::Error>>>>,
        pub wipe_data_results: Arc<Mutex<HashMap<String, Result<(), anyhow::Error>>>>,
        pub start_results: Arc<Mutex<HashMap<String, Result<(), anyhow::Error>>>>,
        pub stop_results: Arc<Mutex<HashMap<String, Result<(), anyhow::Error>>>>,
        pub start_barrier: Arc<Mutex<Option<Arc<Barrier>>>>,
        pub stop_barrier: Arc<Mutex<Option<Arc<Barrier>>>>,
        pub all_package_info_error: Arc<Mutex<Option<String>>>,
        pub last_install_args: Arc<Mutex<Option<(PathBuf, bool, bool)>>>,
    }

    impl Default for MockPackageManagerService {
        fn default() -> Self {
            Self {
                packages: Arc::new(Mutex::new(HashMap::new())),
                package_infos: Arc::new(Mutex::new(HashMap::new())),
                install_results: Arc::new(Mutex::new(HashMap::new())),
                remove_results: Arc::new(Mutex::new(HashMap::new())),
                wipe_data_results: Arc::new(Mutex::new(HashMap::new())),
                start_results: Arc::new(Mutex::new(HashMap::new())),
                stop_results: Arc::new(Mutex::new(HashMap::new())),
                start_barrier: Arc::new(Mutex::new(None)),
                stop_barrier: Arc::new(Mutex::new(None)),
                all_package_info_error: Arc::new(Mutex::new(None)),
                last_install_args: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl MockPackageManagerService {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn add_package(&self, name: String, status: PackageStatusEnum) {
            self.packages.lock().unwrap().insert(name, status);
        }

        pub fn set_install_result(
            &self,
            package_name: String,
            result: Result<PackageMetadata, anyhow::Error>,
        ) {
            self.install_results
                .lock()
                .unwrap()
                .insert(package_name, result);
        }

        pub fn set_remove_result(&self, package_name: String, result: Result<(), anyhow::Error>) {
            self.remove_results
                .lock()
                .unwrap()
                .insert(package_name, result);
        }

        pub fn set_wipe_data_result(
            &self,
            package_name: String,
            result: Result<(), anyhow::Error>,
        ) {
            self.wipe_data_results
                .lock()
                .unwrap()
                .insert(package_name, result);
        }

        pub fn set_start_result(&self, package_name: String, result: Result<(), anyhow::Error>) {
            self.start_results
                .lock()
                .unwrap()
                .insert(package_name, result);
        }

        pub fn set_stop_result(&self, package_name: String, result: Result<(), anyhow::Error>) {
            self.stop_results
                .lock()
                .unwrap()
                .insert(package_name, result);
        }

        pub fn set_start_barrier(&self, barrier: Arc<Barrier>) {
            *self.start_barrier.lock().unwrap() = Some(barrier);
        }

        pub fn set_stop_barrier(&self, barrier: Arc<Barrier>) {
            *self.stop_barrier.lock().unwrap() = Some(barrier);
        }

        pub fn set_all_package_info_error(&self, error: String) {
            *self.all_package_info_error.lock().unwrap() = Some(error);
        }

        pub fn set_package_info(&self, package_name: String, package_info: PackageStateInfo) {
            self.package_infos
                .lock()
                .unwrap()
                .insert(package_name, package_info);
        }

        fn default_metadata(package_name: String) -> PackageMetadata {
            PackageMetadata::new(
                libssam::config::PackageConfigSpec {
                    package: libssam::config::Package {
                        name: package_name,
                        version: "1.0.0".to_string(),
                        description: "A test package".to_string(),
                        autostart: Some(true),
                    },
                    container: libssam::config::Container {
                        storage_limit: Some(1024),
                        data_dirs: Some("/app/data".to_string()),
                        security: libssam::config::Security {
                            seccomp: true,
                            mac: true,
                        },
                        network: Some(libssam::config::Network {
                            mode: None,
                            bridge: None,
                        }),
                    },
                    service: libssam::config::Service {
                        service_type: "notify".to_string(),
                        bus_name: None,
                        remain_after_exit: Some(false),
                    },
                },
                FsType::Erofs,
                PackageFsVerityInfo {
                    data_size: 0,
                    hash_size: 0,
                    table_params: "1 4096 4096 100 101 sha256 mock_hash_root deadbeef".to_string(),
                    hash_offset: 0,
                },
            )
            .expect("default metadata should be valid")
        }
    }

    #[async_trait]
    impl PackageManagerService for MockPackageManagerService {
        async fn get_package_names(&self) -> Vec<String> {
            self.packages.lock().unwrap().keys().cloned().collect()
        }

        async fn get_packages_status(&self) -> anyhow::Result<Vec<(String, PackageStatusEnum)>> {
            Ok(self
                .packages
                .lock()
                .unwrap()
                .iter()
                .map(|(name, status)| (name.clone(), status.clone()))
                .collect())
        }

        async fn install_package(
            &self,
            path: PathBuf,
            downgrade: bool,
            remove_data: bool,
        ) -> anyhow::Result<PackageMetadata> {
            *self.last_install_args.lock().unwrap() = Some((path.clone(), downgrade, remove_data));
            let package_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            let outcome = {
                let guard = self.install_results.lock().unwrap();
                guard.get(&package_name).map(|r| match r {
                    Ok(metadata) => Ok(metadata.clone()),
                    Err(e) => Err(e.to_string()),
                })
            };

            match outcome {
                Some(Ok(metadata)) => {
                    self.packages
                        .lock()
                        .unwrap()
                        .insert(package_name, PackageStatusEnum::Ready);
                    Ok(metadata)
                }
                Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
                None => {
                    self.packages
                        .lock()
                        .unwrap()
                        .insert(package_name.clone(), PackageStatusEnum::Ready);
                    Ok(Self::default_metadata(package_name))
                }
            }
        }

        async fn remove_package(
            &self,
            package_name: &str,
            _purge_volume: bool,
        ) -> anyhow::Result<()> {
            let outcome = {
                let guard = self.remove_results.lock().unwrap();
                guard
                    .get(package_name)
                    .map(|r| r.as_ref().copied().map_err(ToString::to_string))
            };

            match outcome {
                Some(Ok(())) | None => {
                    if self.packages.lock().unwrap().remove(package_name).is_some() {
                        Ok(())
                    } else {
                        Err(crate::package_manager::PackageNotFound(package_name.to_owned()).into())
                    }
                }
                Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
            }
        }

        async fn wipe_package_data(&self, package_name: &str) -> anyhow::Result<()> {
            let outcome = {
                let guard = self.wipe_data_results.lock().unwrap();
                guard
                    .get(package_name)
                    .map(|r| r.as_ref().copied().map_err(ToString::to_string))
            };

            match outcome {
                Some(Ok(())) => Ok(()),
                Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
                None if self.packages.lock().unwrap().contains_key(package_name) => Ok(()),
                None => Err(anyhow::anyhow!("Package not found: {package_name}")),
            }
        }

        async fn start_package(&self, package_name: &str) -> anyhow::Result<()> {
            let barrier = self.start_barrier.lock().unwrap().clone();
            if let Some(barrier) = barrier {
                barrier.wait().await;
            }

            let outcome = {
                let guard = self.start_results.lock().unwrap();
                guard
                    .get(package_name)
                    .map(|r| r.as_ref().copied().map_err(ToString::to_string))
            };

            match outcome {
                Some(Ok(())) => {
                    let mut packages = self.packages.lock().unwrap();
                    if packages.contains_key(package_name) {
                        packages.insert(
                            package_name.to_owned(),
                            PackageStatusEnum::Running(package_name.to_owned()),
                        );
                    }
                    Ok(())
                }
                Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
                None => {
                    let mut packages = self.packages.lock().unwrap();
                    if packages.contains_key(package_name) {
                        packages.insert(
                            package_name.to_owned(),
                            PackageStatusEnum::Running(package_name.to_owned()),
                        );
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!("Package not found"))
                    }
                }
            }
        }

        async fn stop_package(&self, package_name: &str) -> anyhow::Result<()> {
            let barrier = self.stop_barrier.lock().unwrap().clone();
            if let Some(barrier) = barrier {
                barrier.wait().await;
            }

            let outcome = {
                let guard = self.stop_results.lock().unwrap();
                guard
                    .get(package_name)
                    .map(|r| r.as_ref().copied().map_err(ToString::to_string))
            };

            match outcome {
                Some(Ok(())) => {
                    let mut packages = self.packages.lock().unwrap();
                    if packages.contains_key(package_name) {
                        packages.insert(package_name.to_owned(), PackageStatusEnum::Ready);
                    }
                    Ok(())
                }
                Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
                None => {
                    let mut packages = self.packages.lock().unwrap();
                    if let std::collections::hash_map::Entry::Occupied(mut e) =
                        packages.entry(package_name.to_owned())
                    {
                        e.insert(PackageStatusEnum::Ready);
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!("Package not found"))
                    }
                }
            }
        }

        async fn get_package_info(&self, package_name: &str) -> anyhow::Result<PackageInfoResult> {
            if let Some(package_info) = self.package_infos.lock().unwrap().get(package_name) {
                return Ok(PackageInfoResult::Normal(package_info.clone()));
            }

            if let Some(status) = self.packages.lock().unwrap().get(package_name).cloned() {
                if let PackageStatusEnum::Broken(broken_info) = status {
                    return Ok(PackageInfoResult::Broken {
                        filename: package_name.to_owned(),
                        info: BrokenPackageInfo {
                            package_path: format!("/mock/packages/{package_name}.ssam"),
                            broken_info,
                        },
                    });
                }

                let package_metadata = Self::default_metadata(package_name.to_owned());
                let quota_info = QuotaInformation {
                    enabled: true,
                    limit: 1024,
                };

                Ok(PackageInfoResult::Normal(PackageStateInfo {
                    package_metadata,
                    package_status: status.to_string(),
                    package_path: format!("/mock/packages/{package_name}.ssam"),
                    package_name: package_name.to_owned(),
                    quota_info,
                }))
            } else {
                Ok(PackageInfoResult::NotFound {
                    package_name: package_name.to_owned(),
                })
            }
        }

        async fn get_all_package_info(&self) -> anyhow::Result<Vec<PackageInfoResult>> {
            if let Some(error) = self.all_package_info_error.lock().unwrap().take() {
                return Err(anyhow::anyhow!(error));
            }

            let mut results = Vec::new();

            let package_entries: Vec<(String, PackageStatusEnum)> = self
                .packages
                .lock()
                .unwrap()
                .iter()
                .map(|(name, status)| (name.clone(), status.clone()))
                .collect();

            for (package_name, status) in package_entries {
                if let Some(info) = self.package_infos.lock().unwrap().get(&package_name) {
                    results.push(PackageInfoResult::Normal(info.clone()));
                    continue;
                }

                if let PackageStatusEnum::Broken(broken_info) = status {
                    results.push(PackageInfoResult::Broken {
                        filename: package_name.clone(),
                        info: BrokenPackageInfo {
                            package_path: format!("/mock/packages/{package_name}.ssam"),
                            broken_info,
                        },
                    });
                    continue;
                }

                let package_metadata = Self::default_metadata(package_name.clone());
                let quota_info = QuotaInformation {
                    enabled: true,
                    limit: 1024,
                };

                results.push(PackageInfoResult::Normal(PackageStateInfo {
                    package_metadata,
                    package_status: status.to_string(),
                    package_path: format!("/mock/packages/{package_name}.ssam"),
                    package_name,
                    quota_info,
                }));
            }

            Ok(results)
        }

        async fn teardown(&self) {}
    }

    fn create_test_remocon() -> (RemoconImpl, MockPackageManagerService) {
        let mock = MockPackageManagerService::new();
        let mock_clone = mock.clone();
        let config = crate::configuration::tests::test_configuration();
        let remocon = RemoconImpl::new(Arc::new(mock), config);
        (remocon, mock_clone)
    }

    fn request_with_value(value: &str) -> Request<()> {
        let mut request = Request::new(());
        request.metadata_mut().insert(
            API_VERSION_METADATA_KEY,
            MetadataValue::try_from(value).expect("test metadata value"),
        );
        request
    }

    #[test]
    fn matching_api_version_is_accepted() {
        let request = request_with_value(&api_version_string());

        assert!(validate_api_version(request).is_ok());
    }

    #[test]
    fn invalid_api_version_requests_are_rejected() {
        let mut duplicate = Request::new(());
        let value = MetadataValue::try_from(api_version_string()).expect("test metadata value");
        duplicate
            .metadata_mut()
            .append(API_VERSION_METADATA_KEY, value.clone());
        duplicate
            .metadata_mut()
            .append(API_VERSION_METADATA_KEY, value);

        let requests = vec![
            Request::new(()),
            request_with_value(""),
            request_with_value("not-a-number"),
            request_with_value(&format!("0{}", api_version_string())),
            request_with_value(&(ApiVersion::Unspecified as i32).to_string()),
            duplicate,
        ];

        for request in requests {
            let status =
                validate_api_version(request).expect_err("invalid API version should fail");
            assert_eq!(status.code(), Code::FailedPrecondition);
            assert!(status.message().contains(&api_version_string()));
        }
    }

    fn package_infos_request(names: &[&str]) -> Request<ListPackageInfoRequest> {
        Request::new(ListPackageInfoRequest {
            package_names: names.iter().map(|name| (*name).to_owned()).collect(),
        })
    }

    #[tokio::test]
    async fn test_start_package_success() {
        let (remocon, mock_manager) = create_test_remocon();

        // Setup: add a package and set expected start result
        mock_manager.add_package("test-package".to_string(), PackageStatusEnum::Ready);
        mock_manager.set_start_result("test-package".to_string(), Ok(()));

        let request = Request::new(StartPackageRequest {
            package_names: vec!["test-package".to_string()],
        });

        let response = remocon.start_package(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 1);
        assert!(ops[0].success);
        assert_eq!(ops[0].package_name, "test-package");
    }

    #[tokio::test]
    async fn test_start_package_failure() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.set_start_result(
            "test-package".to_string(),
            Err(anyhow::anyhow!("Start failed")),
        );

        let request = Request::new(StartPackageRequest {
            package_names: vec!["test-package".to_string()],
        });

        let response = remocon.start_package(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 1);
        assert!(!ops[0].success);
        assert_eq!(ops[0].package_name, "test-package");
        assert!(ops[0].message.as_ref().unwrap().contains("Start failed"));
    }

    #[tokio::test]
    async fn test_start_all_packages_empty_names() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("pkg-a".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package("pkg-b".to_string(), PackageStatusEnum::Ready);
        mock_manager.set_start_result("pkg-a".to_string(), Ok(()));
        mock_manager.set_start_result("pkg-b".to_string(), Ok(()));

        let request = Request::new(StartPackageRequest {
            package_names: vec![],
        });

        let response = remocon.start_package(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|op| op.success));
        let names: Vec<&str> = ops.iter().map(|op| op.package_name.as_str()).collect();
        assert!(names.contains(&"pkg-a"));
        assert!(names.contains(&"pkg-b"));
    }

    #[tokio::test]
    async fn test_start_and_stop_packages_in_one_request_are_concurrent() {
        let (remocon, mock_manager) = create_test_remocon();
        let start_barrier = Arc::new(Barrier::new(2));
        let stop_barrier = Arc::new(Barrier::new(2));

        mock_manager.add_package("pkg-a".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package("pkg-b".to_string(), PackageStatusEnum::Ready);
        mock_manager.set_start_barrier(start_barrier);
        mock_manager.set_stop_barrier(stop_barrier);

        let start_response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            remocon.start_package(Request::new(StartPackageRequest {
                package_names: vec!["pkg-a".to_string(), "pkg-b".to_string()],
            })),
        )
        .await
        .expect("start operations did not run concurrently")
        .expect("start request failed");
        assert_eq!(start_response.into_inner().results.len(), 2);

        mock_manager.add_package(
            "pkg-a".to_string(),
            PackageStatusEnum::Running("pkg-a".to_string()),
        );
        mock_manager.add_package(
            "pkg-b".to_string(),
            PackageStatusEnum::Running("pkg-b".to_string()),
        );

        let stop_response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            remocon.stop_package(Request::new(StopPackageRequest {
                package_names: vec!["pkg-a".to_string(), "pkg-b".to_string()],
            })),
        )
        .await
        .expect("stop operations did not run concurrently")
        .expect("stop request failed");
        assert_eq!(stop_response.into_inner().results.len(), 2);
    }

    #[tokio::test]
    async fn test_stop_all_packages_empty_names() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package(
            "pkg-a".to_string(),
            PackageStatusEnum::Running("pkg-a".to_string()),
        );
        mock_manager.add_package(
            "pkg-b".to_string(),
            PackageStatusEnum::Running("pkg-b".to_string()),
        );
        mock_manager.set_stop_result("pkg-a".to_string(), Ok(()));
        mock_manager.set_stop_result("pkg-b".to_string(), Ok(()));

        let request = Request::new(StopPackageRequest {
            package_names: vec![],
        });

        let response = remocon.stop_package(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|op| op.success));
        let names: Vec<&str> = ops.iter().map(|op| op.package_name.as_str()).collect();
        assert!(names.contains(&"pkg-a"));
        assert!(names.contains(&"pkg-b"));
    }

    #[tokio::test]
    async fn test_start_deduplicates_package_names() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("pkg-a".to_string(), PackageStatusEnum::Ready);
        mock_manager.set_start_result("pkg-a".to_string(), Ok(()));

        let request = Request::new(StartPackageRequest {
            package_names: vec![
                "pkg-a".to_string(),
                "pkg-a".to_string(),
                "pkg-a".to_string(),
            ],
        });

        let response = remocon.start_package(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].package_name, "pkg-a");
        assert!(ops[0].success);
    }

    #[tokio::test]
    async fn test_remove_package_not_found() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(RemovePackageRequest {
            package_name: "non-existent".to_string(),
        });

        let response = remocon.remove_package(request).await;
        let error = response.expect_err("missing package should return not_found");
        assert_eq!(error.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn test_remove_package_success() {
        let (remocon, mock_manager) = create_test_remocon();
        mock_manager.add_package("test-package".to_owned(), PackageStatusEnum::Ready);

        let request = Request::new(RemovePackageRequest {
            package_name: "test-package".to_owned(),
        });

        let response = remocon.remove_package(request).await;
        assert!(response.is_ok());
    }

    #[tokio::test]
    async fn test_remove_package_internal_error() {
        let (remocon, mock_manager) = create_test_remocon();
        mock_manager.set_remove_result(
            "test-package".to_owned(),
            Err(anyhow::anyhow!("Remove operation failed")),
        );

        let request = Request::new(RemovePackageRequest {
            package_name: "test-package".to_owned(),
        });

        let response = remocon.remove_package(request).await;
        let error = response.expect_err("internal remove failure should return an error");
        assert_eq!(error.code(), tonic::Code::Internal);
    }

    #[tokio::test]
    async fn test_wipe_package_data_not_found() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(WipePackageDataRequest {
            package_names: vec!["non-existent".to_string()],
        });

        let response = remocon.wipe_package_data(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 1);
        assert!(!ops[0].success);
        assert!(
            ops[0]
                .message
                .as_ref()
                .unwrap()
                .contains("Package not found")
        );
    }

    #[tokio::test]
    async fn test_wipe_package_data_rejected_when_running() {
        let (remocon, mock_manager) = create_test_remocon();
        mock_manager.add_package(
            "test-pkg".to_string(),
            PackageStatusEnum::Running("test-pkg".to_string()),
        );
        mock_manager.set_wipe_data_result(
            "test-pkg".to_string(),
            Err(anyhow::anyhow!(
                "Package 'test-pkg' is not in a wipeable state; stop it before wiping data"
            )),
        );

        let request = Request::new(WipePackageDataRequest {
            package_names: vec!["test-pkg".to_string()],
        });

        let response = remocon.wipe_package_data(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 1);
        assert!(!ops[0].success);
        assert!(
            ops[0]
                .message
                .as_ref()
                .unwrap()
                .contains("stop it before wiping data"),
            "per-package failure must surface the underlying reason"
        );
    }

    #[tokio::test]
    async fn test_wipe_package_data_success_keeps_package() {
        let (remocon, mock_manager) = create_test_remocon();
        mock_manager.add_package("test-pkg".to_string(), PackageStatusEnum::Ready);

        let request = Request::new(WipePackageDataRequest {
            package_names: vec!["test-pkg".to_string()],
        });

        let response = remocon.wipe_package_data(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 1);
        assert!(ops[0].success);
        assert!(
            mock_manager
                .packages
                .lock()
                .unwrap()
                .contains_key("test-pkg"),
            "package registration should remain after wipe-data"
        );
    }

    #[tokio::test]
    async fn test_wipe_all_packages_empty_names() {
        let (remocon, mock_manager) = create_test_remocon();
        mock_manager.add_package("pkg-a".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package("pkg-b".to_string(), PackageStatusEnum::Ready);

        let request = Request::new(WipePackageDataRequest {
            package_names: vec![],
        });

        let response = remocon.wipe_package_data(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|op| op.success));
        let names: Vec<&str> = ops.iter().map(|op| op.package_name.as_str()).collect();
        assert!(names.contains(&"pkg-a"));
        assert!(names.contains(&"pkg-b"));
    }

    #[tokio::test]
    async fn test_install_package_downgrade_mode() {
        let (remocon, mock_manager) = create_test_remocon();

        // Setup: configure install to succeed in downgrade mode
        mock_manager.set_install_result(
            "downgrade-package".to_string(),
            Ok(PackageMetadata::new(
                libssam::config::PackageConfigSpec {
                    package: libssam::config::Package {
                        name: "downgrade-package".to_string(),
                        version: "1.2.3".to_string(),
                        description: "A test package".to_string(),
                        autostart: Some(true),
                    },
                    container: libssam::config::Container {
                        storage_limit: Some(1024),
                        data_dirs: Some("/app/data".to_string()),
                        security: libssam::config::Security {
                            seccomp: true,
                            mac: true,
                        },
                        network: Some(libssam::config::Network {
                            mode: None,
                            bridge: None,
                        }),
                    },
                    service: libssam::config::Service {
                        service_type: "notify".to_string(),
                        bus_name: None,
                        remain_after_exit: Some(false),
                    },
                },
                FsType::Erofs,
                PackageFsVerityInfo {
                    data_size: 0,
                    hash_size: 0,
                    table_params: "1 4096 4096 100 101 sha256 mock_hash_root deadbeef".to_string(),
                    hash_offset: 0,
                },
            )
            .expect("install metadata should be valid")),
        );

        let request = Request::new(InstallPackageRequest {
            package_path: "/tmp/downgrade-package.ssam".to_string(),
            downgrade: true,
            remove_data: false,
        });

        let response = remocon.install_package(request).await;
        assert!(response.is_ok());
        let install_response = response.unwrap().into_inner();
        let package_data = install_response
            .package_data
            .expect("package data should be present");
        assert_eq!(package_data.package_name, "downgrade-package");
        assert_eq!(package_data.version, "1.2.3");

        let args = mock_manager
            .last_install_args
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(args.1, "downgrade should be true");
        assert!(!args.2, "remove_data should be false");
    }

    #[tokio::test]
    async fn test_install_package_remove_data_mode() {
        let (remocon, mock_manager) = create_test_remocon();

        // Setup: configure install to succeed in remove_data mode
        mock_manager.set_install_result(
            "remove-data-package".to_string(),
            Ok(PackageMetadata::new(
                libssam::config::PackageConfigSpec {
                    package: libssam::config::Package {
                        name: "remove-data-package".to_string(),
                        version: "2.0.0".to_string(),
                        description: "A test package".to_string(),
                        autostart: Some(true),
                    },
                    container: libssam::config::Container {
                        storage_limit: Some(1024),
                        data_dirs: Some("/app/data".to_string()),
                        security: libssam::config::Security {
                            seccomp: true,
                            mac: true,
                        },
                        network: Some(libssam::config::Network {
                            mode: None,
                            bridge: None,
                        }),
                    },
                    service: libssam::config::Service {
                        service_type: "notify".to_string(),
                        bus_name: None,
                        remain_after_exit: Some(false),
                    },
                },
                FsType::Erofs,
                PackageFsVerityInfo {
                    data_size: 0,
                    hash_size: 0,
                    table_params: "1 4096 4096 100 101 sha256 mock_hash_root deadbeef".to_string(),
                    hash_offset: 0,
                },
            )
            .expect("install metadata should be valid")),
        );

        let request = Request::new(InstallPackageRequest {
            package_path: "/tmp/remove-data-package.ssam".to_string(),
            downgrade: false,
            remove_data: true,
        });

        let response = remocon.install_package(request).await;
        assert!(response.is_ok());
        let install_response = response.unwrap().into_inner();
        let package_data = install_response
            .package_data
            .expect("package data should be present");
        assert_eq!(package_data.package_name, "remove-data-package");
        assert_eq!(package_data.version, "2.0.0");

        let args = mock_manager
            .last_install_args
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(!args.1, "downgrade should be false");
        assert!(args.2, "remove_data should be true");
    }

    #[tokio::test]
    async fn test_install_package_downgrade_and_remove_data() {
        let (remocon, mock_manager) = create_test_remocon();

        // Setup: configure install to succeed with both downgrade and remove_data options
        mock_manager.set_install_result(
            "downgrade-remove-package".to_string(),
            Ok(PackageMetadata::new(
                libssam::config::PackageConfigSpec {
                    package: libssam::config::Package {
                        name: "downgrade-remove-package".to_string(),
                        version: "3.1.4".to_string(),
                        description: "A test package".to_string(),
                        autostart: Some(true),
                    },
                    container: libssam::config::Container {
                        storage_limit: Some(1024),
                        data_dirs: Some("/app/data".to_string()),
                        security: libssam::config::Security {
                            seccomp: true,
                            mac: true,
                        },
                        network: Some(libssam::config::Network {
                            mode: None,
                            bridge: None,
                        }),
                    },
                    service: libssam::config::Service {
                        service_type: "notify".to_string(),
                        bus_name: None,
                        remain_after_exit: Some(false),
                    },
                },
                FsType::Erofs,
                PackageFsVerityInfo {
                    data_size: 0,
                    hash_size: 0,
                    table_params: "1 4096 4096 100 101 sha256 mock_hash_root deadbeef".to_string(),
                    hash_offset: 0,
                },
            )
            .expect("install metadata should be valid")),
        );

        let request = Request::new(InstallPackageRequest {
            package_path: "/tmp/downgrade-remove-package.ssam".to_string(),
            downgrade: true,
            remove_data: true,
        });

        let response = remocon.install_package(request).await;
        assert!(response.is_ok());
        let install_response = response.unwrap().into_inner();
        let package_data = install_response
            .package_data
            .expect("package data should be present");
        assert_eq!(package_data.package_name, "downgrade-remove-package");
        assert_eq!(package_data.version, "3.1.4");

        let args = mock_manager
            .last_install_args
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(args.1, "downgrade should be true");
        assert!(args.2, "remove_data should be true");
    }

    #[tokio::test]
    async fn test_list_package_info_from_files_nonexistent_file() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(ListPackageInfoFromFilesRequest {
            package_paths: vec!["/nonexistent/path/package.ssam".to_string()],
        });

        let response = remocon.list_package_info_from_files(request).await;
        let packages = response
            .expect("response should be Ok")
            .into_inner()
            .packages;
        assert_eq!(packages.len(), 1);
        let package = &packages[0];
        assert!(!package.success);
        assert_eq!(package.package_path, "/nonexistent/path/package.ssam");
        assert!(package.package_data.is_none());
        assert!(
            package
                .error_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("/nonexistent/path/package.ssam"))
        );
    }

    #[tokio::test]
    async fn test_list_package_info_from_files_invalid_file() {
        let (remocon, _mock_manager) = create_test_remocon();

        let tmp = tempfile::Builder::new()
            .suffix(".ssam")
            .tempfile()
            .expect("temp file creation should succeed");
        std::fs::write(tmp.path(), b"not a valid ssam package")
            .expect("temp file write should succeed");

        let request = Request::new(ListPackageInfoFromFilesRequest {
            package_paths: vec![tmp.path().to_str().unwrap().to_string()],
        });

        let response = remocon.list_package_info_from_files(request).await;
        let packages = response
            .expect("response should be Ok")
            .into_inner()
            .packages;
        assert_eq!(packages.len(), 1);
        let package = &packages[0];
        assert!(!package.success);
        assert_eq!(
            package.package_path,
            tmp.path().to_str().unwrap().to_owned()
        );
        assert!(
            package
                .error_summary
                .as_deref()
                .is_some_and(|summary| summary.contains(tmp.path().to_str().unwrap()))
        );
    }

    #[tokio::test]
    async fn test_list_package_info_from_files_rejects_wrong_extension() {
        let (remocon, _mock_manager) = create_test_remocon();

        let tmp = tempfile::Builder::new()
            .suffix(".bin")
            .tempfile()
            .expect("temp file creation should succeed");
        std::fs::write(tmp.path(), b"not a valid ssam package")
            .expect("temp file write should succeed");

        let request = Request::new(ListPackageInfoFromFilesRequest {
            package_paths: vec![tmp.path().to_str().unwrap().to_string()],
        });

        let response = remocon
            .list_package_info_from_files(request)
            .await
            .expect("response should be Ok")
            .into_inner();
        assert_eq!(response.packages.len(), 1);
        let package = &response.packages[0];
        assert!(!package.success);
        assert!(
            package
                .error_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("is not a valid package file"))
        );
    }

    #[tokio::test]
    async fn test_list_package_info_from_files_rejects_fifo() {
        let (remocon, _mock_manager) = create_test_remocon();

        let temp_dir = tempfile::tempdir().expect("temp dir creation should succeed");
        let fifo_path = temp_dir.path().join("test.fifo");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &fifo_path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .expect("mkfifo should succeed");

        let request = Request::new(ListPackageInfoFromFilesRequest {
            package_paths: vec![fifo_path.to_str().unwrap().to_string()],
        });

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            remocon.list_package_info_from_files(request),
        )
        .await
        .expect("list_package_info_from_files should return immediately, not block")
        .expect("response should be Ok")
        .into_inner();
        assert_eq!(response.packages.len(), 1);
        let package = &response.packages[0];
        assert!(!package.success);
        assert!(
            package
                .error_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("is not a valid package file"))
        );
    }

    #[tokio::test]
    async fn test_list_package_info_from_files_rejects_relative_path() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(ListPackageInfoFromFilesRequest {
            package_paths: vec!["relative/path/package.ssam".to_string()],
        });

        let response = remocon.list_package_info_from_files(request).await;
        let packages = response
            .expect("response should be Ok")
            .into_inner()
            .packages;
        assert_eq!(packages.len(), 1);
        let package = &packages[0];
        assert!(!package.success);
        assert_eq!(package.package_path, "relative/path/package.ssam");
        assert!(
            package
                .error_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("Package path must be absolute"))
        );
    }

    #[tokio::test]
    async fn test_list_package_info_from_files_empty() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(ListPackageInfoFromFilesRequest {
            package_paths: vec![],
        });

        let response = remocon
            .list_package_info_from_files(request)
            .await
            .expect("empty file list should succeed");
        assert!(response.into_inner().packages.is_empty());
    }

    #[tokio::test]
    async fn test_list_package_info_empty() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(ListPackageInfoRequest {
            package_names: vec![],
        });
        let response = remocon.list_package_info(request).await;
        assert!(response.is_ok());
        let packages = response.unwrap().into_inner().packages;
        assert!(packages.is_empty());
    }

    #[tokio::test]
    async fn test_list_package_info_mixed_states() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("ready-pkg".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package(
            "running-pkg".to_string(),
            PackageStatusEnum::Running("running-pkg".to_string()),
        );
        mock_manager.add_package(
            "broken-pkg".to_string(),
            PackageStatusEnum::Broken(BrokenReason::new("broken-pkg", "parse failure")),
        );

        let request = Request::new(ListPackageInfoRequest {
            package_names: vec![],
        });
        let response = remocon.list_package_info(request).await;
        assert!(response.is_ok());
        let packages = response.unwrap().into_inner().packages;
        assert_eq!(packages.len(), 3);

        let by_name: std::collections::HashMap<String, &PackageInfo> =
            packages.iter().map(|p| (p.name.clone(), p)).collect();

        let ready = by_name.get("ready-pkg").expect("ready-pkg should exist");
        assert!(ready.success);
        assert_eq!(ready.status, "Ready");

        let running = by_name
            .get("running-pkg")
            .expect("running-pkg should exist");
        assert!(running.success);
        assert!(running.status.contains("Running"));

        let broken = by_name.get("broken-pkg").expect("broken-pkg should exist");
        assert!(broken.success);
        assert_eq!(broken.status, "Broken");
        assert!(broken.error_summary.is_some());
        assert!(broken.error_details.is_some());
    }

    #[tokio::test]
    async fn test_list_package_info_with_predefined_info() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("custom-pkg".to_string(), PackageStatusEnum::Ready);

        let custom_config = libssam::config::PackageConfigSpec {
            package: libssam::config::Package {
                name: "custom-pkg".to_string(),
                version: "2.5.0".to_string(),
                description: "Custom package".to_string(),
                autostart: Some(false),
            },
            container: libssam::config::Container {
                storage_limit: Some(512),
                data_dirs: Some("/custom/data".to_string()),
                security: libssam::config::Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(libssam::config::Network {
                    mode: None,
                    bridge: None,
                }),
            },
            service: libssam::config::Service {
                service_type: "simple".to_string(),
                bus_name: None,
                remain_after_exit: Some(false),
            },
        };
        let metadata = PackageMetadata::new(
            custom_config,
            FsType::Erofs,
            PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                table_params: "1 4096 4096 100 101 sha256 custom_hash deadbeef".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let custom_info = PackageStateInfo {
            package_metadata: metadata,
            package_status: PackageStatusEnum::Ready.to_string(),
            package_path: "/mock/packages/custom-pkg.ssam".to_string(),
            package_name: "custom-pkg".to_string(),
            quota_info: QuotaInformation {
                enabled: true,
                limit: 512,
            },
        };
        mock_manager.set_package_info("custom-pkg".to_string(), custom_info);

        let request = Request::new(ListPackageInfoRequest {
            package_names: vec![],
        });
        let response = remocon.list_package_info(request).await;
        assert!(response.is_ok());
        let packages = response.unwrap().into_inner().packages;
        assert_eq!(packages.len(), 1);

        let pkg = &packages[0];
        assert_eq!(pkg.name, "custom-pkg");
        assert_eq!(pkg.package_path, "/mock/packages/custom-pkg.ssam");
        assert_eq!(pkg.quota.as_ref().map(|q| q.limit), Some(512));
    }

    #[tokio::test]
    async fn test_list_package_info_failure() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.set_all_package_info_error("Package info retrieval failed".to_string());

        let request = Request::new(ListPackageInfoRequest {
            package_names: vec![],
        });
        let response = remocon.list_package_info(request).await;
        let error = response.expect_err("package info failure should be internal");
        assert_eq!(error.code(), tonic::Code::Internal);
    }

    #[tokio::test]
    async fn test_install_package_failure() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.set_install_result(
            "failed-package".to_string(),
            Err(anyhow::anyhow!("Install failed")),
        );

        let request = Request::new(InstallPackageRequest {
            package_path: "/tmp/failed-package.ssam".to_string(),
            downgrade: false,
            remove_data: false,
        });

        let response = remocon.install_package(request).await;
        let error = response.expect_err("install failure should be internal");
        assert_eq!(error.code(), tonic::Code::Internal);
    }

    #[tokio::test]
    async fn test_install_package_rejects_relative_path() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(InstallPackageRequest {
            package_path: "relative/path/package.ssam".to_string(),
            downgrade: false,
            remove_data: false,
        });

        let response = remocon.install_package(request).await;
        let error = response.expect_err("relative path should be invalid_argument");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_list_packages_mixed_states() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package(
            "running-pkg".to_string(),
            PackageStatusEnum::Running("running-pkg".to_string()),
        );
        mock_manager.add_package("ready-pkg".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package(
            "broken-pkg".to_string(),
            PackageStatusEnum::Broken(BrokenReason::new("broken-pkg", "detailed error")),
        );

        let request = Request::new(());
        let response = remocon.list_packages(request).await;

        assert!(response.is_ok());
        let packages_status = response.unwrap().into_inner().entries;
        assert_eq!(packages_status.len(), 3);

        let status_map: std::collections::HashMap<String, String> = packages_status
            .into_iter()
            .map(|p| (p.package_name, p.status))
            .collect();

        assert!(status_map.contains_key("running-pkg"));
        assert!(status_map.contains_key("ready-pkg"));
        assert!(status_map.contains_key("broken-pkg"));
    }

    #[tokio::test]
    async fn test_get_timeline_info_returns_valid_response() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(());
        let response = remocon.get_timeline_info(request).await;

        assert!(response.is_ok());
        let timeline_response = response.unwrap().into_inner();
        assert!(
            timeline_response
                .ssamd_uptime
                .is_some_and(|uptime| uptime.seconds > 0 || uptime.nanos > 0)
        );
        assert!(timeline_response.events.is_empty());
    }

    #[test]
    fn test_timeline_event_mapping() {
        let event = into_proto_timeline_event(ssam_log::TimelineEvent {
            pkg: "pkg-a".to_owned(),
            phase: "mount".to_owned(),
            duration_ns: 42,
            kind: ssam_log::TimelineEventKind::Completed,
        });

        assert_eq!(event.pkg, "pkg-a");
        assert_eq!(event.phase, "mount");
        assert_eq!(event.duration_ns, 42);
        assert_eq!(
            TimelineEventKind::try_from(event.kind),
            Ok(TimelineEventKind::Completed)
        );
    }

    #[tokio::test]
    async fn test_concurrent_package_operations() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("pkg1".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package("pkg2".to_string(), PackageStatusEnum::Ready);
        mock_manager.set_start_result("pkg1".to_string(), Ok(()));
        mock_manager.set_start_result("pkg2".to_string(), Ok(()));

        let start_req1 = Request::new(StartPackageRequest {
            package_names: vec!["pkg1".to_string()],
        });
        let start_req2 = Request::new(StartPackageRequest {
            package_names: vec!["pkg2".to_string()],
        });

        let (result1, result2) = tokio::join!(
            remocon.start_package(start_req1),
            remocon.start_package(start_req2)
        );

        assert!(result1.is_ok());
        assert!(result2.is_ok());
    }

    #[tokio::test]
    async fn test_package_status_transitions() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("transition-pkg".to_string(), PackageStatusEnum::Ready);
        mock_manager.set_start_result("transition-pkg".to_string(), Ok(()));
        mock_manager.set_stop_result("transition-pkg".to_string(), Ok(()));

        let start_request = Request::new(StartPackageRequest {
            package_names: vec!["transition-pkg".to_string()],
        });
        let start_response = remocon.start_package(start_request).await;
        assert!(start_response.is_ok());

        let status_request = Request::new(());
        let status_response = remocon.list_packages(status_request).await;
        assert!(status_response.is_ok());

        let packages_status = status_response.unwrap().into_inner().entries;
        let transition_pkg_status = packages_status
            .iter()
            .find(|p| p.package_name == "transition-pkg")
            .unwrap();
        assert!(transition_pkg_status.status.contains("Running"));

        let stop_request = Request::new(StopPackageRequest {
            package_names: vec!["transition-pkg".to_string()],
        });
        let stop_response = remocon.stop_package(stop_request).await;
        assert!(stop_response.is_ok());
    }

    #[tokio::test]
    async fn test_edge_cases_empty_package_name() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(StartPackageRequest {
            package_names: vec![String::new()],
        });

        let response = remocon.start_package(request).await;
        assert!(response.is_ok());
        let ops = response.unwrap().into_inner().results;
        assert_eq!(ops.len(), 1);
        assert!(!ops[0].success);
    }

    #[tokio::test]
    async fn test_edge_cases_long_package_name() {
        let (remocon, mock_manager) = create_test_remocon();

        let long_name = "a".repeat(1000);
        mock_manager.add_package(long_name.clone(), PackageStatusEnum::Ready);
        mock_manager.set_start_result(long_name.clone(), Ok(()));

        let request = Request::new(StartPackageRequest {
            package_names: vec![long_name],
        });

        let response = remocon.start_package(request).await;
        assert!(response.is_ok());
    }

    #[tokio::test]
    async fn test_get_package_info_with_predefined_info() {
        let (remocon, mock_manager) = create_test_remocon();

        let package_config = libssam::config::PackageConfigSpec {
            package: libssam::config::Package {
                name: "test-package".to_string(),
                description: "A test package".to_string(),
                version: "1.0.0".to_string(),
                autostart: Some(false),
            },
            container: libssam::config::Container {
                storage_limit: Some(2048),
                data_dirs: Some("/custom/data".to_string()),
                security: libssam::config::Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(libssam::config::Network {
                    mode: None,
                    bridge: None,
                }),
            },
            service: libssam::config::Service {
                service_type: "simple".to_string(),
                bus_name: Some("com.example.test".to_string()),
                remain_after_exit: Some(true),
            },
        };
        let package_metadata = PackageMetadata::new(
            package_config.clone(),
            FsType::Erofs,
            PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                table_params: "1 4096 4096 100 101 sha256 custom_hash_root deadbeef".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let package_info = PackageStateInfo {
            package_metadata,
            package_status: PackageStatusEnum::Ready.to_string(),
            package_path: "/mock/packages/test-package.ssam".to_string(),
            package_name: "test-package".to_string(),
            quota_info: QuotaInformation {
                enabled: false,
                limit: 2048,
            },
        };

        mock_manager.set_package_info("test-package".to_string(), package_info.clone());

        let request = package_infos_request(&["test-package"]);

        let response = remocon.list_package_info(request).await;
        assert!(response.is_ok());
        let package_info_response = response
            .unwrap()
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");
        assert!(package_info_response.success);
        assert_eq!(package_info_response.name, "test-package");
        assert_eq!(
            package_info_response.package_path,
            "/mock/packages/test-package.ssam"
        );
    }

    #[tokio::test]
    async fn test_get_package_info_with_default_info() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("default-package".to_string(), PackageStatusEnum::Ready);

        let request = package_infos_request(&["default-package"]);

        let response = remocon.list_package_info(request).await;
        assert!(response.is_ok());
        let package_info_response = response
            .unwrap()
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");
        assert!(package_info_response.success);
        assert_eq!(package_info_response.name, "default-package");
        assert_eq!(package_info_response.status, "Ready");
        assert_eq!(
            package_info_response.package_path,
            "/mock/packages/default-package.ssam"
        );
    }

    #[tokio::test]
    async fn test_get_package_info_running_package() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package(
            "running-package".to_string(),
            PackageStatusEnum::Running("running-package".to_string()),
        );

        let request = package_infos_request(&["running-package"]);

        let response = remocon.list_package_info(request).await;
        assert!(response.is_ok());
        let package_info_response = response
            .unwrap()
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");
        assert_eq!(package_info_response.name, "running-package");
        assert_eq!(
            package_info_response.package_path,
            "/mock/packages/running-package.ssam"
        );
        assert!(package_info_response.status.contains("Running"));
        assert!(
            package_info_response
                .quota
                .as_ref()
                .is_some_and(|q| q.enabled)
        );
    }

    #[tokio::test]
    async fn test_get_package_info_broken_package() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package(
            "broken-package".to_string(),
            PackageStatusEnum::Broken(BrokenReason::new("broken-package", "detailed error")),
        );

        let request = package_infos_request(&["broken-package"]);

        let response = remocon.list_package_info(request).await;
        assert!(response.is_ok());
        let package_info_response = response
            .unwrap()
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");
        assert_eq!(package_info_response.name, "broken-package");
        assert!(package_info_response.success);
        assert_eq!(package_info_response.status, "Broken");
        assert_eq!(
            package_info_response.package_path,
            "/mock/packages/broken-package.ssam"
        );
        assert!(package_info_response.error_summary.is_some());
        assert!(package_info_response.error_details.is_some());
    }

    #[tokio::test]
    async fn test_get_package_info_package_not_found() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = package_infos_request(&["non-existent-package"]);

        let response = remocon
            .list_package_info(request)
            .await
            .expect("response should contain the package failure");
        let package_info = response
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");
        assert!(!package_info.success);
        assert_eq!(package_info.status, "NotFound");
        assert_eq!(package_info.name, "non-existent-package");
        assert!(
            package_info
                .error_summary
                .is_some_and(|summary| summary.contains("non-existent-package"))
        );
    }

    #[tokio::test]
    async fn test_get_package_info_empty_package_name() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = package_infos_request(&["", "  "]);

        let response = remocon
            .list_package_info(request)
            .await
            .expect("response should contain no package results");
        assert!(response.into_inner().packages.is_empty());
    }

    #[tokio::test]
    async fn test_get_package_info_multiple_packages() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("package1".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package(
            "package2".to_string(),
            PackageStatusEnum::Running("package2".to_string()),
        );

        let custom_config = libssam::config::PackageConfigSpec {
            package: libssam::config::Package {
                name: "package1".to_string(),
                version: "1.0.0".to_string(),
                description: "A test package".to_string(),
                autostart: Some(false),
            },
            container: libssam::config::Container {
                storage_limit: Some(512),
                data_dirs: Some("/custom/path".to_string()),
                security: libssam::config::Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(libssam::config::Network {
                    mode: None,
                    bridge: None,
                }),
            },
            service: libssam::config::Service {
                service_type: "forking".to_string(),
                bus_name: None,
                remain_after_exit: Some(true),
            },
        };
        let package_metadata = PackageMetadata::new(
            custom_config,
            FsType::Erofs,
            PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                table_params: "1 4096 4096 100 101 sha256 mock_hash_root deadbeef".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let custom_info = PackageStateInfo {
            package_metadata,
            package_status: PackageStatusEnum::Ready.to_string(),
            package_path: "/mock/packages/package1.ssam".to_string(),
            package_name: "package1".to_string(),
            quota_info: QuotaInformation {
                enabled: true,
                limit: 512,
            },
        };
        mock_manager.set_package_info("package1".to_string(), custom_info);

        let request = package_infos_request(&["package1", "missing-package", "package2"]);
        let packages = remocon
            .list_package_info(request)
            .await
            .expect("response should contain every package result")
            .into_inner()
            .packages;
        assert_eq!(packages.len(), 3);

        let package_info1 = &packages[0];
        assert!(package_info1.success);
        assert_eq!(package_info1.name, "package1");
        assert_eq!(package_info1.package_path, "/mock/packages/package1.ssam");
        assert_eq!(package_info1.quota.as_ref().map(|q| q.limit), Some(512));

        let missing_package = &packages[1];
        assert_eq!(missing_package.name, "missing-package");
        assert!(!missing_package.success);
        assert_eq!(missing_package.status, "NotFound");
        assert!(
            missing_package
                .error_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("missing-package"))
        );

        let package_info2 = &packages[2];
        assert!(package_info2.success);
        assert_eq!(package_info2.name, "package2");
        assert_eq!(package_info2.package_path, "/mock/packages/package2.ssam");
        assert!(package_info2.status.contains("Running"));
        assert_eq!(package_info2.quota.as_ref().map(|q| q.limit), Some(1024));
    }

    #[tokio::test]
    async fn test_get_package_info_config_serialization() {
        let (remocon, mock_manager) = create_test_remocon();

        let complex_config = libssam::config::PackageConfigSpec {
            package: libssam::config::Package {
                name: "complex-package".to_string(),
                version: "0.0.1".to_string(),
                description: "A complex package".to_string(),
                autostart: Some(true),
            },
            container: libssam::config::Container {
                storage_limit: Some(4096),
                data_dirs: Some("/var/lib/complex:/opt/data".to_string()),
                security: libssam::config::Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(libssam::config::Network {
                    mode: None,
                    bridge: None,
                }),
            },
            service: libssam::config::Service {
                service_type: "oneshot".to_string(),
                bus_name: Some("org.example.complex.service".to_string()),
                remain_after_exit: Some(false),
            },
        };
        let package_metadata = PackageMetadata::new(
            complex_config,
            FsType::Erofs,
            PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                table_params: "1 4096 4096 100 101 sha256 mock_hash_root deadbeef".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let complex_info = PackageStateInfo {
            package_metadata,
            package_status: PackageStatusEnum::Ready.to_string(),
            package_path: "/mock/packages/complex-package.ssam".to_string(),
            package_name: "complex-package".to_string(),
            quota_info: QuotaInformation {
                enabled: true,
                limit: 4096,
            },
        };

        mock_manager.set_package_info("complex-package".to_string(), complex_info);

        let request = package_infos_request(&["complex-package"]);

        let response = remocon.list_package_info(request).await;
        assert!(response.is_ok());
        let package_info_response = response
            .unwrap()
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");

        assert!(package_info_response.package_data.is_some());
        assert_eq!(
            package_info_response.package_path,
            "/mock/packages/complex-package.ssam"
        );

        assert!(
            package_info_response
                .quota
                .as_ref()
                .is_some_and(|q| q.enabled)
        );
        assert_eq!(
            package_info_response.quota.as_ref().map(|q| q.limit),
            Some(4096)
        );
    }

    #[tokio::test]
    async fn test_get_package_info_concurrent_access() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.add_package("concurrent1".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package("concurrent2".to_string(), PackageStatusEnum::Ready);
        mock_manager.add_package("concurrent3".to_string(), PackageStatusEnum::Ready);

        let request1 = package_infos_request(&["concurrent1"]);
        let request2 = package_infos_request(&["concurrent2"]);
        let request3 = package_infos_request(&["concurrent3"]);

        let (response1, response2, response3) = tokio::join!(
            remocon.list_package_info(request1),
            remocon.list_package_info(request2),
            remocon.list_package_info(request3),
        );

        assert!(response1.is_ok());
        assert!(response2.is_ok());
        assert!(response3.is_ok());

        let package_info1 = response1
            .unwrap()
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");
        let package_info2 = response2
            .unwrap()
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");
        let package_info3 = response3
            .unwrap()
            .into_inner()
            .packages
            .into_iter()
            .next()
            .expect("package info should be present");

        assert_eq!(package_info1.name, "concurrent1");
        assert_eq!(package_info2.name, "concurrent2");
        assert_eq!(package_info3.name, "concurrent3");
    }
}
