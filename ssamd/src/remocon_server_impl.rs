// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt as _;

use crate::package::PackageStatus as InternalPackageStatus;
use crate::utils::ellipsis;
use anyhow::Context as _;
use libssam::{
    InspectLocalPackageResponse, InstallResponse, JsonResult, PackageInfoResponse,
    PackageStatusEntry, StartStopResult, TimelineResponse,
    json_result::serialize_metadata,
    remocon::{
        AllPackageInformationResponse, InspectLocalPackageRequest,
        InspectLocalPackageResponse as InspectLocalPackageProtoResponse, InstallPackageRequest,
        InstallPackageResponse, ListPackagesStatusResponse, PackageInformationRequest,
        PackageInformationResponse, RemovePackageRequest, RemovePackageResponse,
        StartPackageRequest, StartPackageResponse, StopPackageRequest, StopPackageResponse,
        TimeLineInformationResponse, remocon_server::Remocon,
    },
};

use tonic::{Request, Response, Status};

use crate::package_manager::PackageManagerService;

pub(crate) struct RemoconImpl {
    package_manager: Arc<dyn PackageManagerService>,
}

/// Convert a handler-local `Result<T, E>` into a JSON-encoded `String`.
///
/// This helper centralizes the common pattern used by Remocon RPC handlers:
///
/// - `Ok(T)`  -> build a success JSON string (e.g., `JsonResult::success(...)`)
/// - `Err(E)` -> log and build a failure JSON string (e.g., `JsonResult::failure(...)`)
///
/// Callers provide two closures:
///
/// - `on_ok`: produces the JSON string for the successful case.
/// - `on_err`: produces the JSON string for the error case (and may log).
fn into_json_result<T, E>(
    inner: Result<T, E>,
    on_ok: impl FnOnce(T) -> String,
    on_err: impl FnOnce(E) -> String,
) -> String {
    match inner {
        Ok(v) => on_ok(v),
        Err(e) => on_err(e),
    }
}

fn build_install_response(
    metadata: &libssam::ssam_package::ssam_pkg_metadata::PackageMetadata,
) -> InstallResponse {
    InstallResponse::from(metadata)
}

impl RemoconImpl {
    pub(crate) fn new(package_manager: Arc<dyn PackageManagerService>) -> Self {
        Self { package_manager }
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

fn into_start_stop_result(name: String, outcome: anyhow::Result<()>) -> StartStopResult {
    match outcome {
        Ok(()) => StartStopResult {
            package_name: name,
            success: true,
            message: None,
        },
        Err(e) => StartStopResult {
            package_name: name,
            success: false,
            message: Some(format!("{e}")),
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
        let results = futures_util::stream::iter(names)
            .fold(Vec::new(), |mut results, name| async {
                let outcome = self
                    .package_manager
                    .start_package(&name)
                    .await
                    .inspect_err(|e| log::error!("Failed to start package {name}: {e:#}"));
                results.push(into_start_stop_result(name, outcome));
                results
            })
            .await;

        Ok(Response::new(StartPackageResponse {
            result: JsonResult::success(results),
        }))
    }

    async fn stop_package(
        &self,
        request: Request<StopPackageRequest>,
    ) -> Result<Response<StopPackageResponse>, Status> {
        let package_names = request.into_inner().package_names;
        log::debug!("Received request to stop packages: {package_names:?}");

        let names = self.resolve_package_names(package_names).await;
        let results = futures_util::stream::iter(names)
            .fold(Vec::new(), |mut results, name| async {
                let outcome = self
                    .package_manager
                    .stop_package(&name)
                    .await
                    .inspect_err(|e| log::error!("Failed to stop package {name}: {e:#}"));
                results.push(into_start_stop_result(name, outcome));
                results
            })
            .await;

        Ok(Response::new(StopPackageResponse {
            result: JsonResult::success(results),
        }))
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
        let force = request.get_ref().force;
        let remove_data = request.get_ref().remove_data;

        let path = PathBuf::from(package_path);
        if !path.is_absolute() {
            let result = JsonResult::<InstallResponse>::failure(format!(
                "Package path must be absolute: {path:?}"
            ));
            return Ok(Response::new(InstallPackageResponse { result }));
        }

        let result_inner = self
            .package_manager
            .install_package(path, force, remove_data)
            .await;

        let result = into_json_result(
            result_inner,
            |metadata| JsonResult::success(build_install_response(&metadata)),
            |e| {
                log::error!("Failed to install package: {e:?}");
                JsonResult::<InstallResponse>::failure("Failed to install package")
            },
        );

        Ok(Response::new(InstallPackageResponse { result }))
    }

    async fn remove_package(
        &self,
        request: Request<RemovePackageRequest>,
    ) -> Result<Response<RemovePackageResponse>, Status> {
        log::debug!("Received request to remove package");
        let package_name = &request.get_ref().package_name;

        let result_inner = self
            .package_manager
            .remove_package(package_name, true)
            .await;

        let result = into_json_result(
            result_inner,
            |()| JsonResult::<()>::success_empty(),
            |e| {
                log::error!("Failed to remove package {package_name}: {e:?}");
                JsonResult::<()>::failure(format!("Failed to remove package: {package_name}"))
            },
        );

        Ok(Response::new(RemovePackageResponse { result }))
    }

    async fn list_packages_status(
        &self,
        _request: Request<()>,
    ) -> Result<Response<ListPackagesStatusResponse>, Status> {
        log::debug!("Received request to list packages");
        let status = self.package_manager.get_packages_status().await;

        let result = into_json_result(
            status,
            |packages| {
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
                JsonResult::success(entries)
            },
            |e| {
                log::error!("Failed to list packages: {e:?}");
                JsonResult::<Vec<PackageStatusEntry>>::failure("Failed to list packages")
            },
        );

        Ok(Response::new(ListPackagesStatusResponse { result }))
    }

    async fn get_timeline_info(
        &self,
        _request: Request<()>,
    ) -> Result<Response<TimeLineInformationResponse>, Status> {
        log::debug!("Received request to get timeline info");
        let raw = ssam_log::get_timelines();
        let mut dropped = 0usize;
        let events: Vec<libssam::remocon_schema::TimelineEvent> = raw
            .into_iter()
            .filter_map(|v| {
                let result = serde_json::from_value(v).ok();
                if result.is_none() {
                    dropped += 1;
                }
                result
            })
            .collect();
        if dropped > 0 {
            log::warn!("get_timeline_info: dropped {dropped} invalid timeline event(s)");
        }
        let response = TimelineResponse {
            events,
            ssamd_uptime: *crate::SSAMD_UPTIME,
        };
        let result = JsonResult::success(response);

        Ok(Response::new(TimeLineInformationResponse { result }))
    }

    async fn get_package_info(
        &self,
        request: Request<PackageInformationRequest>,
    ) -> Result<Response<PackageInformationResponse>, Status> {
        log::debug!("Received request to get package info");
        let package_name = request.get_ref().package_name.clone();
        let package_info = self.package_manager.get_package_info(&package_name).await;

        let result = into_json_result(
            package_info,
            |info| {
                let response: PackageInfoResponse = info.into();
                JsonResult::success(response)
            },
            |e| {
                log::error!("Failed to get package info for {package_name}: {e:?}");
                JsonResult::<PackageInfoResponse>::failure(format!(
                    "Failed to get package info: {package_name}"
                ))
            },
        );

        Ok(Response::new(PackageInformationResponse { result }))
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    async fn inspect_local_package(
        &self,
        request: Request<InspectLocalPackageRequest>,
    ) -> Result<Response<InspectLocalPackageProtoResponse>, Status> {
        log::debug!("Received request to inspect local package");
        let package_path = PathBuf::from(request.get_ref().package_path.clone());

        if !package_path.is_absolute() {
            let result = JsonResult::<InspectLocalPackageResponse>::failure(format!(
                "Package path must be absolute: {package_path:?}"
            ));
            return Ok(Response::new(InspectLocalPackageProtoResponse { result }));
        }

        let pkg_path = package_path.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::package_manager::parser::parse_package_file(pkg_path)
                .and_then(|r| r.package_file)
        })
        .await
        .context("inspect_local_package task panicked")
        .and_then(|r| r);

        let result_json = into_json_result(
            result,
            |package_file| {
                let metadata = package_file.metadata();
                let package_path = package_path.to_string_lossy().into_owned();
                let response = InspectLocalPackageResponse {
                    metadata: serialize_metadata(metadata),
                    package_path,
                };
                JsonResult::success(response)
            },
            |e| {
                log::error!("Failed to inspect local package {package_path:?}: {e:?}");
                JsonResult::<InspectLocalPackageResponse>::failure(format!(
                    "Failed to inspect local package: {package_path:?}"
                ))
            },
        );

        Ok(Response::new(InspectLocalPackageProtoResponse {
            result: result_json,
        }))
    }

    async fn get_all_package_info(
        &self,
        _request: Request<()>,
    ) -> Result<Response<AllPackageInformationResponse>, Status> {
        log::debug!("Received request to get all package info");
        let all_info = self.package_manager.get_all_package_info().await;

        let result = into_json_result(
            all_info,
            |infos| {
                let entries: Vec<PackageInfoResponse> = infos.into_iter().map(Into::into).collect();
                JsonResult::success(entries)
            },
            |e| {
                log::error!("Failed to get all package info: {e:?}");
                JsonResult::<Vec<PackageInfoResponse>>::failure("Failed to get all package info")
            },
        );

        Ok(Response::new(AllPackageInformationResponse { result }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::package::PackageStatus as PackageStatusEnum;
    use crate::package_manager::PackageManagerService;

    use async_trait::async_trait;
    use libssam::ssam_package::PackageFsVerityInfo;
    use libssam::ssam_package::ssam_pkg_info::{
        BrokenPackageInfo, BrokenReason, PackageInfo, PackageInfoResult, QuotaInformation,
    };
    use libssam::ssam_package::ssam_pkg_metadata::PackageMetadata;
    use libssam::superblock::FsType;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tonic::Request;

    type InstallResultMap = Arc<Mutex<HashMap<String, Result<PackageMetadata, anyhow::Error>>>>;

    #[derive(Debug, Clone)]
    pub(crate) struct MockPackageManagerService {
        pub packages: Arc<Mutex<HashMap<String, PackageStatusEnum>>>,
        pub package_infos: Arc<Mutex<HashMap<String, PackageInfo>>>,
        pub install_results: InstallResultMap,
        pub remove_results: Arc<Mutex<HashMap<String, Result<(), anyhow::Error>>>>,
        pub start_results: Arc<Mutex<HashMap<String, Result<(), anyhow::Error>>>>,
        pub stop_results: Arc<Mutex<HashMap<String, Result<(), anyhow::Error>>>>,
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
                start_results: Arc::new(Mutex::new(HashMap::new())),
                stop_results: Arc::new(Mutex::new(HashMap::new())),
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

        pub fn set_all_package_info_error(&self, error: String) {
            *self.all_package_info_error.lock().unwrap() = Some(error);
        }

        pub fn set_package_info(&self, package_name: String, package_info: PackageInfo) {
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
                        network: libssam::config::Network {
                            mode: None,
                            interface_name: None,
                        },
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
                    root_hash: "mock_hash_root".to_string(),
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
            force: bool,
            remove_data: bool,
        ) -> anyhow::Result<PackageMetadata> {
            *self.last_install_args.lock().unwrap() = Some((path.clone(), force, remove_data));
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
                    self.packages.lock().unwrap().remove(package_name);
                    Ok(())
                }
                Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
            }
        }

        async fn start_package(&self, package_name: &str) -> anyhow::Result<()> {
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

                Ok(PackageInfoResult::Normal(PackageInfo {
                    package_metadata,
                    package_status: status.to_string(),
                    package_path: format!("/mock/packages/{package_name}.ssam"),
                    package_name: package_name.to_owned(),
                    quota_info,
                }))
            } else {
                Err(anyhow::anyhow!("Package not found: {package_name}"))
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

                results.push(PackageInfoResult::Normal(PackageInfo {
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
        let remocon = RemoconImpl::new(Arc::new(mock));
        (remocon, mock_clone)
    }

    fn parse_json_result<T: serde::de::DeserializeOwned>(json_result: &str) -> Result<T, String> {
        let value: serde_json::Value =
            serde_json::from_str(json_result).map_err(|e| e.to_string())?;
        if let Some(data) = value.get("data") {
            serde_json::from_value(data.clone()).map_err(|e| e.to_string())
        } else {
            Err("No data field in JSON result".to_string())
        }
    }

    fn check_json_success(json_result: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(json_result)
            .ok()
            .and_then(|v| v.get("success").and_then(serde_json::Value::as_bool))
            .unwrap_or(false)
    }

    fn get_json_reason(json_result: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(json_result)
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
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
        let result = &response.unwrap().into_inner().result;
        assert!(check_json_success(result));
        let ops: Vec<StartStopResult> = parse_json_result(result).unwrap();
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
        let result = &response.unwrap().into_inner().result;
        assert!(check_json_success(result));
        let ops: Vec<StartStopResult> = parse_json_result(result).unwrap();
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
        let result = &response.unwrap().into_inner().result;
        assert!(check_json_success(result));
        let ops: Vec<StartStopResult> = parse_json_result(result).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|op| op.success));
        let names: Vec<&str> = ops.iter().map(|op| op.package_name.as_str()).collect();
        assert!(names.contains(&"pkg-a"));
        assert!(names.contains(&"pkg-b"));
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
        let result = &response.unwrap().into_inner().result;
        assert!(check_json_success(result));
        let ops: Vec<StartStopResult> = parse_json_result(result).unwrap();
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
        let result = &response.unwrap().into_inner().result;
        assert!(check_json_success(result));
        let ops: Vec<StartStopResult> = parse_json_result(result).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].package_name, "pkg-a");
        assert!(ops[0].success);
    }

    #[tokio::test]
    async fn test_remove_package_not_found() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.set_remove_result(
            "non-existent".to_string(),
            Err(anyhow::anyhow!("Package not found")),
        );

        let request = Request::new(RemovePackageRequest {
            package_name: "non-existent".to_string(),
        });

        let response = remocon.remove_package(request).await;
        assert!(response.is_ok());
        let json_result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(json_result));
        let reason = get_json_reason(json_result);
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("Failed to remove package"));
    }

    #[tokio::test]
    async fn test_install_package_force_mode() {
        let (remocon, mock_manager) = create_test_remocon();

        // Setup: configure install to succeed in force mode
        mock_manager.set_install_result(
            "force-package".to_string(),
            Ok(PackageMetadata::new(
                libssam::config::PackageConfigSpec {
                    package: libssam::config::Package {
                        name: "force-package".to_string(),
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
                        network: libssam::config::Network {
                            mode: None,
                            interface_name: None,
                        },
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
                    root_hash: "mock_hash_root".to_string(),
                    hash_offset: 0,
                },
            )
            .expect("install metadata should be valid")),
        );

        let request = Request::new(InstallPackageRequest {
            package_path: "/tmp/force-package.ssam".to_string(),
            force: true,
            remove_data: false,
        });

        let response = remocon.install_package(request).await;
        assert!(response.is_ok());
        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let install_response: InstallResponse = parse_json_result(json_result).unwrap();
        assert_eq!(install_response.metadata.package_name, "force-package");
        assert_eq!(install_response.metadata.version, "1.2.3");

        let args = mock_manager
            .last_install_args
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(args.1, "force should be true");
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
                        network: libssam::config::Network {
                            mode: None,
                            interface_name: None,
                        },
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
                    root_hash: "mock_hash_root".to_string(),
                    hash_offset: 0,
                },
            )
            .expect("install metadata should be valid")),
        );

        let request = Request::new(InstallPackageRequest {
            package_path: "/tmp/remove-data-package.ssam".to_string(),
            force: false,
            remove_data: true,
        });

        let response = remocon.install_package(request).await;
        assert!(response.is_ok());
        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let install_response: InstallResponse = parse_json_result(json_result).unwrap();
        assert_eq!(
            install_response.metadata.package_name,
            "remove-data-package"
        );
        assert_eq!(install_response.metadata.version, "2.0.0");

        let args = mock_manager
            .last_install_args
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(!args.1, "force should be false");
        assert!(args.2, "remove_data should be true");
    }

    #[tokio::test]
    async fn test_install_package_force_and_remove_data() {
        let (remocon, mock_manager) = create_test_remocon();

        // Setup: configure install to succeed with both force and remove_data options
        mock_manager.set_install_result(
            "force-remove-package".to_string(),
            Ok(PackageMetadata::new(
                libssam::config::PackageConfigSpec {
                    package: libssam::config::Package {
                        name: "force-remove-package".to_string(),
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
                        network: libssam::config::Network {
                            mode: None,
                            interface_name: None,
                        },
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
                    root_hash: "mock_hash_root".to_string(),
                    hash_offset: 0,
                },
            )
            .expect("install metadata should be valid")),
        );

        let request = Request::new(InstallPackageRequest {
            package_path: "/tmp/force-remove-package.ssam".to_string(),
            force: true,
            remove_data: true,
        });

        let response = remocon.install_package(request).await;
        assert!(response.is_ok());
        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let install_response: InstallResponse = parse_json_result(json_result).unwrap();
        assert_eq!(
            install_response.metadata.package_name,
            "force-remove-package"
        );
        assert_eq!(install_response.metadata.version, "3.1.4");

        let args = mock_manager
            .last_install_args
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(args.1, "force should be true");
        assert!(args.2, "remove_data should be true");
    }

    #[tokio::test]
    async fn test_inspect_local_package_nonexistent_file() {
        crate::configuration::ensure_test_init();
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(InspectLocalPackageRequest {
            package_path: "/nonexistent/path/package.ssam".to_string(),
        });

        let response = remocon.inspect_local_package(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(json_result));
        let reason = get_json_reason(json_result);
        assert!(reason.is_some());
        assert!(
            reason.as_ref().unwrap().contains("Failed to inspect"),
            "expected 'Failed to inspect' in reason, got: {}",
            reason.unwrap()
        );
    }

    #[tokio::test]
    async fn test_inspect_local_package_invalid_file() {
        crate::configuration::ensure_test_init();
        let (remocon, _mock_manager) = create_test_remocon();

        let tmp = tempfile::NamedTempFile::new().expect("temp file creation should succeed");
        std::fs::write(tmp.path(), b"not a valid ssam package")
            .expect("temp file write should succeed");

        let request = Request::new(InspectLocalPackageRequest {
            package_path: tmp.path().to_str().unwrap().to_string(),
        });

        let response = remocon.inspect_local_package(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(json_result));
        let reason = get_json_reason(json_result);
        assert!(reason.is_some());
    }

    #[tokio::test]
    async fn test_inspect_local_package_rejects_relative_path() {
        crate::configuration::ensure_test_init();
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(InspectLocalPackageRequest {
            package_path: "relative/path/package.ssam".to_string(),
        });

        let response = remocon.inspect_local_package(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(json_result));
        let reason = get_json_reason(json_result);
        assert!(
            reason
                .as_ref()
                .unwrap()
                .contains("Package path must be absolute"),
            "expected 'Package path must be absolute' in reason, got: {}",
            reason.unwrap()
        );
    }

    #[tokio::test]
    async fn test_get_all_package_info_empty() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(());
        let response = remocon.get_all_package_info(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let packages: Vec<PackageInfoResponse> = parse_json_result(json_result).unwrap();
        assert!(packages.is_empty());
    }

    #[tokio::test]
    async fn test_get_all_package_info_mixed_states() {
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

        let request = Request::new(());
        let response = remocon.get_all_package_info(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let packages: Vec<PackageInfoResponse> = parse_json_result(json_result).unwrap();
        assert_eq!(packages.len(), 3);

        let by_name: std::collections::HashMap<String, &PackageInfoResponse> =
            packages.iter().map(|p| (p.name.clone(), p)).collect();

        let ready = by_name.get("ready-pkg").expect("ready-pkg should exist");
        assert!(!ready.broken);
        assert_eq!(ready.status, "Ready");

        let running = by_name
            .get("running-pkg")
            .expect("running-pkg should exist");
        assert!(!running.broken);
        assert!(running.status.contains("Running"));

        let broken = by_name.get("broken-pkg").expect("broken-pkg should exist");
        assert!(broken.broken);
        assert!(broken.error_summary.is_some());
        assert!(broken.error_details.is_some());
    }

    #[tokio::test]
    async fn test_get_all_package_info_with_predefined_info() {
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
                network: libssam::config::Network {
                    mode: None,
                    interface_name: None,
                },
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
                root_hash: "custom_hash".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let custom_info = PackageInfo {
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

        let request = Request::new(());
        let response = remocon.get_all_package_info(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let packages: Vec<PackageInfoResponse> = parse_json_result(json_result).unwrap();
        assert_eq!(packages.len(), 1);

        let pkg = &packages[0];
        assert_eq!(pkg.name, "custom-pkg");
        assert_eq!(pkg.package_path, "/mock/packages/custom-pkg.ssam");
        assert_eq!(pkg.quota.as_ref().map(|q| q.limit), Some(512));
    }

    #[tokio::test]
    async fn test_get_all_package_info_failure() {
        let (remocon, mock_manager) = create_test_remocon();

        mock_manager.set_all_package_info_error("Package info retrieval failed".to_string());

        let request = Request::new(());
        let response = remocon.get_all_package_info(request).await;
        assert!(response.is_ok());

        let result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(result));
        let reason = get_json_reason(result);
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("Failed to get all package info"));
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
            force: false,
            remove_data: false,
        });

        let response = remocon.install_package(request).await;
        assert!(response.is_ok());
        let json_result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(json_result));
        let reason = get_json_reason(json_result);
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("Failed to install package"));
    }

    #[tokio::test]
    async fn test_install_package_rejects_relative_path() {
        crate::configuration::ensure_test_init();
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(InstallPackageRequest {
            package_path: "relative/path/package.ssam".to_string(),
            force: false,
            remove_data: false,
        });

        let response = remocon.install_package(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(json_result));
        let reason = get_json_reason(json_result);
        assert!(
            reason
                .as_ref()
                .unwrap()
                .contains("Package path must be absolute"),
            "expected 'Package path must be absolute' in reason, got: {}",
            reason.unwrap()
        );
    }

    #[tokio::test]
    async fn test_list_packages_status_mixed_states() {
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
        let response = remocon.list_packages_status(request).await;

        assert!(response.is_ok());
        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let packages_status: Vec<PackageStatusEntry> = parse_json_result(json_result).unwrap();
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
        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let timeline_response: TimelineResponse = parse_json_result(json_result).unwrap();
        assert!(
            timeline_response.ssamd_uptime.as_secs() > 0
                || timeline_response.ssamd_uptime.subsec_nanos() > 0
        );
        assert!(timeline_response.events.is_empty());
    }

    #[test]
    fn test_timeline_event_filter_discards_invalid_json_values() {
        use libssam::remocon_schema::TimelineEvent;

        let valid = serde_json::json!({
            "pkg": "pkg-a",
            "phase": "mount",
            "duration_ns": 100u64,
            "kind": "started",
        });
        let invalid = serde_json::json!({"unexpected": "shape"});

        let raw = vec![valid, invalid];
        let mut dropped = 0usize;
        let events: Vec<TimelineEvent> = raw
            .into_iter()
            .filter_map(|v| {
                let result = serde_json::from_value(v).ok();
                if result.is_none() {
                    dropped += 1;
                }
                result
            })
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(dropped, 1);
        assert_eq!(events[0].pkg, "pkg-a");
        assert_eq!(events[0].phase, "mount");
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
        let status_response = remocon.list_packages_status(status_request).await;
        assert!(status_response.is_ok());

        let json_result = &status_response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let packages_status: Vec<PackageStatusEntry> = parse_json_result(json_result).unwrap();
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
        let result = &response.unwrap().into_inner().result;
        assert!(check_json_success(result));
        let ops: Vec<StartStopResult> = parse_json_result(result).unwrap();
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
                network: libssam::config::Network {
                    mode: None,
                    interface_name: None,
                },
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
                root_hash: "custom_hash_root".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let package_info = PackageInfo {
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

        let request = Request::new(PackageInformationRequest {
            package_name: "test-package".to_string(),
        });

        let response = remocon.get_package_info(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let package_info_response: PackageInfoResponse = parse_json_result(json_result).unwrap();
        assert!(!package_info_response.broken);
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

        let request = Request::new(PackageInformationRequest {
            package_name: "default-package".to_string(),
        });

        let response = remocon.get_package_info(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let package_info_response: PackageInfoResponse = parse_json_result(json_result).unwrap();
        assert!(!package_info_response.broken);
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

        let request = Request::new(PackageInformationRequest {
            package_name: "running-package".to_string(),
        });

        let response = remocon.get_package_info(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let package_info_response: PackageInfoResponse = parse_json_result(json_result).unwrap();
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

        let request = Request::new(PackageInformationRequest {
            package_name: "broken-package".to_string(),
        });

        let response = remocon.get_package_info(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let package_info_response: PackageInfoResponse = parse_json_result(json_result).unwrap();
        assert_eq!(package_info_response.name, "broken-package");
        assert!(package_info_response.broken);
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

        let request = Request::new(PackageInformationRequest {
            package_name: "non-existent-package".to_string(),
        });

        let response = remocon.get_package_info(request).await;
        assert!(response.is_ok());
        let json_result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(json_result));
        assert!(
            get_json_reason(json_result)
                .unwrap()
                .contains("Failed to get package info")
        );
    }

    #[tokio::test]
    async fn test_get_package_info_empty_package_name() {
        let (remocon, _mock_manager) = create_test_remocon();

        let request = Request::new(PackageInformationRequest {
            package_name: String::new(),
        });

        let response = remocon.get_package_info(request).await;
        assert!(response.is_ok());
        let json_result = &response.unwrap().into_inner().result;
        assert!(!check_json_success(json_result));
        assert!(
            get_json_reason(json_result)
                .unwrap()
                .contains("Failed to get package info")
        );
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
                network: libssam::config::Network {
                    mode: None,
                    interface_name: None,
                },
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
                root_hash: "mock_hash_root".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let custom_info = PackageInfo {
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

        let request1 = Request::new(PackageInformationRequest {
            package_name: "package1".to_string(),
        });
        let response1 = remocon.get_package_info(request1).await;
        assert!(response1.is_ok());
        let json_result1 = &response1.unwrap().into_inner().result;
        assert!(check_json_success(json_result1));
        let package_info1: PackageInfoResponse = parse_json_result(json_result1).unwrap();
        assert_eq!(package_info1.name, "package1");
        assert_eq!(package_info1.package_path, "/mock/packages/package1.ssam");
        assert_eq!(package_info1.quota.as_ref().map(|q| q.limit), Some(512));

        let request2 = Request::new(PackageInformationRequest {
            package_name: "package2".to_string(),
        });
        let response2 = remocon.get_package_info(request2).await;
        assert!(response2.is_ok());
        let json_result2 = &response2.unwrap().into_inner().result;
        assert!(check_json_success(json_result2));
        let package_info2: PackageInfoResponse = parse_json_result(json_result2).unwrap();
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
                network: libssam::config::Network {
                    mode: None,
                    interface_name: None,
                },
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
                root_hash: "mock_hash_root".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let complex_info = PackageInfo {
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

        let request = Request::new(PackageInformationRequest {
            package_name: "complex-package".to_string(),
        });

        let response = remocon.get_package_info(request).await;
        assert!(response.is_ok());

        let json_result = &response.unwrap().into_inner().result;
        assert!(check_json_success(json_result));
        let package_info_response: PackageInfoResponse = parse_json_result(json_result).unwrap();

        assert!(package_info_response.metadata.is_some());
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

        let request1 = Request::new(PackageInformationRequest {
            package_name: "concurrent1".to_string(),
        });
        let request2 = Request::new(PackageInformationRequest {
            package_name: "concurrent2".to_string(),
        });
        let request3 = Request::new(PackageInformationRequest {
            package_name: "concurrent3".to_string(),
        });

        let (response1, response2, response3) = tokio::join!(
            remocon.get_package_info(request1),
            remocon.get_package_info(request2),
            remocon.get_package_info(request3),
        );

        assert!(response1.is_ok());
        assert!(response2.is_ok());
        assert!(response3.is_ok());

        let json_result1 = &response1.unwrap().into_inner().result;
        let json_result2 = &response2.unwrap().into_inner().result;
        let json_result3 = &response3.unwrap().into_inner().result;

        assert!(check_json_success(json_result1));
        assert!(check_json_success(json_result2));
        assert!(check_json_success(json_result3));

        let package_info1: PackageInfoResponse = parse_json_result(json_result1).unwrap();
        let package_info2: PackageInfoResponse = parse_json_result(json_result2).unwrap();
        let package_info3: PackageInfoResponse = parse_json_result(json_result3).unwrap();

        assert_eq!(package_info1.name, "concurrent1");
        assert_eq!(package_info2.name, "concurrent2");
        assert_eq!(package_info3.name, "concurrent3");
    }
}
