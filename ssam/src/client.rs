// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context as _, Result};
use std::net::Ipv4Addr;
use tonic::Request;
use tonic::transport::Channel;

use libssam::remocon::remocon_client::RemoconClient;
use libssam::remocon::{
    InstallPackageRequest, InstallPackageResponse, ListPackageInfoFromFilesRequest,
    ListPackageInfoFromFilesResponse, ListPackageInfoRequest, ListPackageInfoResponse,
    ListPackagesResponse, RemovePackageRequest, StartPackageRequest, StartPackageResponse,
    StopPackageRequest, StopPackageResponse, TimelineData, WipePackageDataRequest,
    WipePackageDataResponse,
};

#[cfg(test)]
use libssam::remocon::InspectLocalPackageResult;

#[tonic::async_trait]
pub trait Client: Send {
    async fn list_packages(&mut self) -> Result<ListPackagesResponse>;
    /// Empty `names` means "all installed packages" — the server resolves the
    /// full list via `GetPackageNames`. Callers must gate this with an explicit
    /// user intent (e.g. `--all` flag) to prevent accidental bulk operations.
    async fn start_package(&mut self, names: Vec<String>) -> Result<StartPackageResponse>;
    /// See [`Self::start_package`] for empty-names semantics.
    async fn stop_package(&mut self, names: Vec<String>) -> Result<StopPackageResponse>;
    async fn install_package(
        &mut self,
        path: &str,
        downgrade: bool,
        remove_data: bool,
    ) -> Result<InstallPackageResponse>;
    async fn remove_package(&mut self, name: &str) -> Result<()>;
    /// See [`Self::start_package`] for empty-names semantics.
    async fn wipe_package_data(&mut self, names: Vec<String>) -> Result<WipePackageDataResponse>;
    async fn get_timeline_info(&mut self) -> Result<TimelineData>;
    async fn list_package_info(&mut self, names: Vec<String>) -> Result<ListPackageInfoResponse>;
    async fn list_package_info_from_files(
        &mut self,
        paths: Vec<String>,
    ) -> Result<ListPackageInfoFromFilesResponse>;
    fn is_local_mode(&self) -> bool;
}

pub struct GrpcClient {
    inner: RemoconClient<Channel>,
    server_ip: Ipv4Addr,
}

impl GrpcClient {
    pub fn new(client: RemoconClient<Channel>, server_ip: Ipv4Addr) -> Self {
        Self {
            inner: client,
            server_ip,
        }
    }
}

#[tonic::async_trait]
impl Client for GrpcClient {
    fn is_local_mode(&self) -> bool {
        self.server_ip.is_loopback()
    }

    async fn list_packages(&mut self) -> Result<ListPackagesResponse> {
        Ok(self
            .inner
            .list_packages(Request::new(()))
            .await
            .context("Daemon side error. list_packages has failed.")?
            .into_inner())
    }

    async fn start_package(&mut self, names: Vec<String>) -> Result<StartPackageResponse> {
        let request = Request::new(StartPackageRequest {
            package_names: names,
        });
        Ok(self
            .inner
            .start_package(request)
            .await
            .context("Cannot start requested package(s)")?
            .into_inner())
    }

    async fn stop_package(&mut self, names: Vec<String>) -> Result<StopPackageResponse> {
        let request = Request::new(StopPackageRequest {
            package_names: names,
        });
        Ok(self
            .inner
            .stop_package(request)
            .await
            .context("Cannot stop requested package(s)")?
            .into_inner())
    }

    async fn install_package(
        &mut self,
        path: &str,
        downgrade: bool,
        remove_data: bool,
    ) -> Result<InstallPackageResponse> {
        let request = Request::new(InstallPackageRequest {
            package_path: path.to_owned(),
            downgrade,
            remove_data,
        });
        Ok(self
            .inner
            .install_package(request)
            .await
            .with_context(|| format!("Cannot install package {path}"))?
            .into_inner())
    }

    async fn remove_package(&mut self, name: &str) -> Result<()> {
        let request = Request::new(RemovePackageRequest {
            package_name: name.to_owned(),
        });
        self.inner
            .remove_package(request)
            .await
            .with_context(|| format!("Cannot remove package {name}"))?
            .into_inner();
        Ok(())
    }

    async fn wipe_package_data(&mut self, names: Vec<String>) -> Result<WipePackageDataResponse> {
        let request = Request::new(WipePackageDataRequest {
            package_names: names,
        });
        Ok(self
            .inner
            .wipe_package_data(request)
            .await
            .context("Cannot wipe data for requested package(s)")?
            .into_inner())
    }

    async fn get_timeline_info(&mut self) -> Result<TimelineData> {
        Ok(self
            .inner
            .get_timeline_info(Request::new(()))
            .await
            .context("Daemon side error. get_timeline_info has failed.")?
            .into_inner())
    }

    async fn list_package_info(&mut self, names: Vec<String>) -> Result<ListPackageInfoResponse> {
        let request = Request::new(ListPackageInfoRequest {
            package_names: names,
        });
        Ok(self
            .inner
            .list_package_info(request)
            .await
            .context("Cannot get package info")?
            .into_inner())
    }

    async fn list_package_info_from_files(
        &mut self,
        paths: Vec<String>,
    ) -> Result<ListPackageInfoFromFilesResponse> {
        let request = Request::new(ListPackageInfoFromFilesRequest {
            package_paths: paths,
        });
        Ok(self
            .inner
            .list_package_info_from_files(request)
            .await
            .context("Cannot get package info from file(s)")?
            .into_inner())
    }
}

#[cfg(test)]
use libssam::PackageInfo;

#[cfg(test)]
pub struct MockClient {
    pub list_packages_result: Option<anyhow::Result<ListPackagesResponse>>,
    pub start_package_result: Option<anyhow::Result<StartPackageResponse>>,
    pub stop_package_result: Option<anyhow::Result<StopPackageResponse>>,
    pub install_package_result: Option<anyhow::Result<InstallPackageResponse>>,
    pub remove_package_result: Option<anyhow::Result<()>>,
    pub wipe_package_data_result: Option<anyhow::Result<WipePackageDataResponse>>,
    pub get_timeline_info_result: Option<anyhow::Result<TimelineData>>,
    pub list_package_info_result: Option<anyhow::Result<ListPackageInfoResponse>>,
    pub list_package_info_from_files_result:
        Option<anyhow::Result<ListPackageInfoFromFilesResponse>>,
    pub is_local: bool,
    pub captured_downgrade: Option<bool>,
    pub captured_start_names: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    pub captured_list_package_info_names: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    pub captured_list_package_info_from_files_paths:
        std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    pub captured_stop_names: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    pub captured_wipe_names: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[cfg(test)]
impl MockClient {
    pub fn new_success() -> Self {
        use libssam::remocon::QuotaInformation;
        use libssam::{PackageActionResult, PackageData, PackageInfo, PackageStatusEntry};

        let package_metadata = Self::make_test_metadata();

        let list_response = ListPackagesResponse {
            entries: vec![PackageStatusEntry {
                package_name: "test-pkg".to_owned(),
                status: "running".to_owned(),
            }],
        };

        let package_info_data = PackageInfo {
            success: true,
            name: "test-pkg".to_owned(),
            status: "Running".to_owned(),
            package_path: "/mock/packages/test-pkg.ssam".to_owned(),
            package_data: Some((&package_metadata).into()),
            quota: Some(QuotaInformation {
                enabled: true,
                limit: 2048,
            }),
            error_summary: None,
            error_details: None,
        };
        let package_infos_response = ListPackageInfoResponse {
            packages: vec![
                package_info_data.clone(),
                PackageInfo {
                    success: true,
                    name: "broken-pkg".to_owned(),
                    status: "Broken".to_owned(),
                    package_path: "/mock/packages/broken-pkg.ssam".to_owned(),
                    package_data: None,
                    quota: None,
                    error_summary: Some("Parse error".to_owned()),
                    error_details: Some("Failed to parse package.toml".to_owned()),
                },
            ],
        };

        let inspect_local_response = InspectLocalPackageResult {
            package_path: "/mock/packages/test-pkg.ssam".to_owned(),
            package_data: Some((&package_metadata).into()),
            success: true,
            error_summary: None,
            error_details: None,
        };
        let start_stop_response = StartPackageResponse {
            results: vec![PackageActionResult {
                package_name: "test-pkg".to_owned(),
                success: true,
                message: None,
            }],
        };
        let stop_response = StopPackageResponse {
            results: start_stop_response.results.clone(),
        };
        let wipe_data_response = WipePackageDataResponse {
            results: start_stop_response.results.clone(),
        };
        let install_response = InstallPackageResponse {
            package_data: Some(PackageData {
                package_name: package_metadata.get_package_name().to_owned(),
                version: package_metadata.get_package_version().to_owned(),
                description: package_metadata.package.description.clone(),
            }),
        };

        Self {
            list_packages_result: Some(Ok(list_response)),
            start_package_result: Some(Ok(start_stop_response)),
            stop_package_result: Some(Ok(stop_response)),
            install_package_result: Some(Ok(install_response)),
            remove_package_result: Some(Ok(())),
            wipe_package_data_result: Some(Ok(wipe_data_response)),
            get_timeline_info_result: Some(Ok(Self::make_test_timeline())),
            list_package_info_result: Some(Ok(package_infos_response)),
            list_package_info_from_files_result: Some(Ok(ListPackageInfoFromFilesResponse {
                packages: vec![inspect_local_response],
            })),
            is_local: true,
            captured_downgrade: None,
            captured_start_names: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            captured_list_package_info_names: std::sync::Arc::new(
                std::sync::Mutex::new(Vec::new()),
            ),
            captured_list_package_info_from_files_paths: std::sync::Arc::new(
                std::sync::Mutex::new(Vec::new()),
            ),
            captured_stop_names: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            captured_wipe_names: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn make_test_metadata() -> libssam::ssam_package::ssam_pkg_metadata::PackageMetadata {
        let package_config = libssam::config::PackageConfigSpec {
            package: libssam::config::Package {
                name: "test-pkg".to_owned(),
                autostart: Some(true),
                version: "1.0.0".to_owned(),
                description: "Test package".to_owned(),
            },
            container: libssam::config::Container {
                storage_limit: Some(2048),
                data_dirs: Some("/app/data:/var/lib/app".to_owned()),
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
                service_type: "notify".to_owned(),
                bus_name: Some("org.example.test".to_owned()),
                remain_after_exit: Some(false),
            },
        };
        libssam::ssam_package::ssam_pkg_metadata::PackageMetadata::new(
            package_config,
            libssam::superblock::FsType::Erofs,
            libssam::ssam_package::PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                table_params: "1 4096 4096 100 101 sha256 dummy_hash_root deadbeef".to_owned(),
                hash_offset: 0,
            },
        )
        .expect("test metadata should be valid")
    }

    fn make_test_timeline() -> libssam::TimelineData {
        use libssam::remocon::{TimelineEvent, TimelineEventKind};

        libssam::TimelineData {
            events: vec![
                TimelineEvent {
                    pkg: "test-pkg".to_owned(),
                    phase: "Parse".to_owned(),
                    duration_ns: 1_000_000_000u64,
                    kind: TimelineEventKind::Completed as i32,
                },
                TimelineEvent {
                    pkg: "unknown-pkg".to_owned(),
                    phase: "Unknown".to_owned(),
                    duration_ns: 0,
                    kind: 0,
                },
            ],
            ssamd_uptime: Some(prost_types::Duration {
                seconds: 42,
                nanos: 0,
            }),
        }
    }

    pub fn with_remote_mode(mut self) -> Self {
        self.is_local = false;
        self
    }

    pub fn with_list_error(mut self) -> Self {
        self.list_packages_result = Some(Err(anyhow::anyhow!("gRPC connection failed")));
        self
    }

    pub fn with_start_error(mut self) -> Self {
        self.start_package_result = Some(Err(anyhow::anyhow!("Start failed")));
        self
    }

    pub fn with_stop_error(mut self) -> Self {
        self.stop_package_result = Some(Err(anyhow::anyhow!("Stop failed")));
        self
    }

    pub fn with_install_error(mut self) -> Self {
        self.install_package_result = Some(Err(anyhow::anyhow!("Install failed")));
        self
    }

    pub fn with_remove_error(mut self) -> Self {
        self.remove_package_result = Some(Err(anyhow::anyhow!("Remove failed")));
        self
    }

    pub fn with_wipe_data_error(mut self) -> Self {
        self.wipe_package_data_result = Some(Err(anyhow::anyhow!("Wipe data failed")));
        self
    }

    pub fn with_timeline_error(mut self) -> Self {
        self.get_timeline_info_result = Some(Err(anyhow::anyhow!("Timeline info failed")));
        self
    }

    pub fn with_pkg_info_error(mut self) -> Self {
        self.list_package_info_result = Some(Err(anyhow::anyhow!("Package info failed")));
        self
    }

    pub fn with_broken_pkg_info(mut self) -> Self {
        use libssam::PackageInfo;

        let package_info_data = PackageInfo {
            success: true,
            name: "broken-pkg".to_owned(),
            status: "Broken".to_owned(),
            package_path: "/mock/packages/broken-pkg.ssam".to_owned(),
            package_data: None,
            quota: None,
            error_summary: Some("Parse error".to_owned()),
            error_details: Some("Failed to parse package.toml".to_owned()),
        };
        self.list_package_info_result = Some(Ok(ListPackageInfoResponse {
            packages: vec![package_info_data],
        }));
        self
    }

    pub fn with_start_results(mut self, results: Vec<libssam::PackageActionResult>) -> Self {
        self.start_package_result = Some(Ok(StartPackageResponse { results }));
        self
    }

    pub fn with_stop_results(mut self, results: Vec<libssam::PackageActionResult>) -> Self {
        self.stop_package_result = Some(Ok(StopPackageResponse { results }));
        self
    }

    pub fn with_wipe_data_results(mut self, results: Vec<libssam::PackageActionResult>) -> Self {
        self.wipe_package_data_result = Some(Ok(WipePackageDataResponse { results }));
        self
    }
}

#[cfg(test)]
#[tonic::async_trait]
impl Client for MockClient {
    fn is_local_mode(&self) -> bool {
        self.is_local
    }

    async fn list_packages(&mut self) -> anyhow::Result<ListPackagesResponse> {
        self.list_packages_result
            .take()
            .expect("list_packages_result not set")
    }

    async fn start_package(&mut self, names: Vec<String>) -> anyhow::Result<StartPackageResponse> {
        self.captured_start_names.lock().unwrap().push(names);
        self.start_package_result
            .take()
            .expect("start_package_result not set")
    }

    async fn stop_package(&mut self, names: Vec<String>) -> anyhow::Result<StopPackageResponse> {
        self.captured_stop_names.lock().unwrap().push(names);
        self.stop_package_result
            .take()
            .expect("stop_package_result not set")
    }

    async fn install_package(
        &mut self,
        _path: &str,
        downgrade: bool,
        _remove_data: bool,
    ) -> anyhow::Result<InstallPackageResponse> {
        self.captured_downgrade = Some(downgrade);
        self.install_package_result
            .take()
            .expect("install_package_result not set")
    }

    async fn remove_package(&mut self, _name: &str) -> anyhow::Result<()> {
        self.remove_package_result
            .take()
            .expect("remove_package_result not set")
    }

    async fn wipe_package_data(
        &mut self,
        names: Vec<String>,
    ) -> anyhow::Result<WipePackageDataResponse> {
        self.captured_wipe_names.lock().unwrap().push(names);
        self.wipe_package_data_result
            .take()
            .expect("wipe_package_data_result not set")
    }

    async fn get_timeline_info(&mut self) -> anyhow::Result<TimelineData> {
        self.get_timeline_info_result
            .take()
            .expect("get_timeline_info_result not set")
    }

    async fn list_package_info(
        &mut self,
        names: Vec<String>,
    ) -> anyhow::Result<ListPackageInfoResponse> {
        let requested_names = names.clone();
        self.captured_list_package_info_names
            .lock()
            .unwrap()
            .push(names);
        let response = self
            .list_package_info_result
            .take()
            .expect("list_package_info_result not set")?;
        if requested_names.is_empty() || response.packages.len() == 1 {
            return Ok(response);
        }

        Ok(ListPackageInfoResponse {
            packages: response
                .packages
                .into_iter()
                .filter(|info| requested_names.iter().any(|name| name == &info.name))
                .collect(),
        })
    }

    async fn list_package_info_from_files(
        &mut self,
        paths: Vec<String>,
    ) -> anyhow::Result<ListPackageInfoFromFilesResponse> {
        self.captured_list_package_info_from_files_paths
            .lock()
            .unwrap()
            .push(paths);
        self.list_package_info_from_files_result
            .take()
            .expect("list_package_info_from_files_result not set")
    }
}

#[cfg(test)]
mod grpc_fake_server {
    use super::*;
    use libssam::remocon::remocon_server::{Remocon, RemoconServer};
    use libssam::remocon::*;
    use std::net::SocketAddr;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tonic::transport::Server;
    use tonic::{Request, Response, Status};

    pub struct MockRemocon;

    #[tonic::async_trait]
    impl Remocon for MockRemocon {
        async fn list_packages(
            &self,
            _request: Request<()>,
        ) -> Result<Response<ListPackagesResponse>, Status> {
            use libssam::PackageStatusEntry;

            Ok(Response::new(ListPackagesResponse {
                entries: vec![PackageStatusEntry {
                    package_name: "test-pkg".to_owned(),
                    status: "running".to_owned(),
                }],
            }))
        }

        async fn start_package(
            &self,
            request: Request<StartPackageRequest>,
        ) -> Result<Response<StartPackageResponse>, Status> {
            use libssam::PackageActionResult;

            let names = &request.get_ref().package_names;
            let ops: Vec<PackageActionResult> = names
                .iter()
                .map(|name| {
                    if name == "error" {
                        PackageActionResult {
                            package_name: name.clone(),
                            success: false,
                            message: Some("Failed to start package".to_owned()),
                        }
                    } else {
                        PackageActionResult {
                            package_name: name.clone(),
                            success: true,
                            message: None,
                        }
                    }
                })
                .collect();
            Ok(Response::new(StartPackageResponse { results: ops }))
        }

        async fn stop_package(
            &self,
            request: Request<StopPackageRequest>,
        ) -> Result<Response<StopPackageResponse>, Status> {
            use libssam::PackageActionResult;

            let names = &request.get_ref().package_names;
            let ops: Vec<PackageActionResult> = names
                .iter()
                .map(|name| PackageActionResult {
                    package_name: name.clone(),
                    success: true,
                    message: None,
                })
                .collect();
            Ok(Response::new(StopPackageResponse { results: ops }))
        }

        async fn install_package(
            &self,
            request: Request<InstallPackageRequest>,
        ) -> Result<Response<InstallPackageResponse>, Status> {
            use std::path::Path;

            let package_path = &request.get_ref().package_path;
            if package_path == "/error.ssam" {
                return Err(Status::internal("Installation failed"));
            }
            let response = {
                let name = Path::new(package_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_owned();
                let package_metadata =
                    libssam::ssam_package::ssam_pkg_metadata::PackageMetadata::new(
                        libssam::config::PackageConfigSpec {
                            package: libssam::config::Package {
                                name,
                                autostart: Some(true),
                                version: "1.0.0".to_owned(),
                                description: "Test package".to_owned(),
                            },
                            container: libssam::config::Container {
                                storage_limit: Some(2048),
                                data_dirs: Some("/app/data:/var/lib/app".to_owned()),
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
                                service_type: "notify".to_owned(),
                                bus_name: Some("org.example.test".to_owned()),
                                remain_after_exit: Some(false),
                            },
                        },
                        libssam::superblock::FsType::Erofs,
                        libssam::ssam_package::PackageFsVerityInfo {
                            data_size: 0,
                            hash_size: 0,
                            table_params: "1 4096 4096 100 101 sha256 dummy_hash_root deadbeef"
                                .to_owned(),
                            hash_offset: 0,
                        },
                    )
                    .expect("Failed to build package metadata");
                InstallPackageResponse {
                    package_data: Some((&package_metadata).into()),
                }
            };

            Ok(Response::new(response))
        }

        async fn remove_package(
            &self,
            _request: Request<RemovePackageRequest>,
        ) -> Result<Response<()>, Status> {
            Ok(Response::new(()))
        }

        async fn wipe_package_data(
            &self,
            request: Request<WipePackageDataRequest>,
        ) -> Result<Response<WipePackageDataResponse>, Status> {
            use libssam::PackageActionResult;

            let names = &request.get_ref().package_names;
            let ops: Vec<PackageActionResult> = names
                .iter()
                .map(|name| PackageActionResult {
                    package_name: name.clone(),
                    success: true,
                    message: None,
                })
                .collect();
            Ok(Response::new(WipePackageDataResponse { results: ops }))
        }

        async fn get_timeline_info(
            &self,
            _request: Request<()>,
        ) -> Result<Response<TimelineData>, Status> {
            use libssam::remocon::{TimelineEvent, TimelineEventKind};

            let timeline_data = TimelineData {
                events: vec![TimelineEvent {
                    pkg: "test-pkg".to_owned(),
                    phase: "Parse".to_owned(),
                    duration_ns: 1_000_000_000u64,
                    kind: TimelineEventKind::Completed as i32,
                }],
                ssamd_uptime: Some(prost_types::Duration {
                    seconds: 42,
                    nanos: 0,
                }),
            };

            Ok(Response::new(timeline_data))
        }

        async fn list_package_info(
            &self,
            request: Request<ListPackageInfoRequest>,
        ) -> Result<Response<ListPackageInfoResponse>, Status> {
            let package_config = libssam::config::PackageConfigSpec {
                package: libssam::config::Package {
                    name: "test-pkg".to_owned(),
                    autostart: Some(true),
                    version: "1.0.0".to_owned(),
                    description: "Test package".to_owned(),
                },
                container: libssam::config::Container {
                    storage_limit: Some(2048),
                    data_dirs: Some("/app/data:/var/lib/app".to_owned()),
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
                    service_type: "notify".to_owned(),
                    bus_name: Some("org.example.test".to_owned()),
                    remain_after_exit: Some(false),
                },
            };
            let package_metadata = libssam::ssam_package::ssam_pkg_metadata::PackageMetadata::new(
                package_config,
                libssam::superblock::FsType::Erofs,
                libssam::ssam_package::PackageFsVerityInfo {
                    data_size: 0,
                    hash_size: 0,
                    table_params: "1 4096 4096 100 101 sha256 dummy_hash_root deadbeef".to_owned(),
                    hash_offset: 0,
                },
            )
            .expect("Failed to build package metadata");

            let package_info_data = PackageInfo {
                success: true,
                name: "test-pkg".to_owned(),
                status: "Running".to_owned(),
                package_path: "/mock/packages/test-pkg.ssam".to_owned(),
                package_data: Some((&package_metadata).into()),
                quota: Some(QuotaInformation {
                    enabled: true,
                    limit: 2048,
                }),
                error_summary: None,
                error_details: None,
            };
            let broken_package_info = PackageInfo {
                success: true,
                name: "broken-pkg".to_owned(),
                status: "Broken".to_owned(),
                package_path: "/mock/packages/broken-pkg.ssam".to_owned(),
                package_data: None,
                quota: None,
                error_summary: Some("Parse error".to_owned()),
                error_details: Some("Failed to parse package.toml".to_owned()),
            };
            let infos = if request.get_ref().package_names.is_empty() {
                vec![package_info_data, broken_package_info]
            } else {
                request
                    .get_ref()
                    .package_names
                    .iter()
                    .map(|name| match name.as_str() {
                        "test-pkg" => package_info_data.clone(),
                        "broken-pkg" => broken_package_info.clone(),
                        _ => {
                            let mut info = package_info_data.clone();
                            info.name.clone_from(name);
                            info.package_path = format!("/mock/packages/{name}.ssam");
                            info
                        }
                    })
                    .collect()
            };
            Ok(Response::new(ListPackageInfoResponse { packages: infos }))
        }

        async fn list_package_info_from_files(
            &self,
            _request: Request<ListPackageInfoFromFilesRequest>,
        ) -> Result<Response<ListPackageInfoFromFilesResponse>, Status> {
            let package_config = libssam::config::PackageConfigSpec {
                package: libssam::config::Package {
                    name: "test-pkg".to_owned(),
                    autostart: Some(true),
                    version: "1.0.0".to_owned(),
                    description: "Test package".to_owned(),
                },
                container: libssam::config::Container {
                    storage_limit: Some(2048),
                    data_dirs: Some("/app/data:/var/lib/app".to_owned()),
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
                    service_type: "notify".to_owned(),
                    bus_name: Some("org.example.test".to_owned()),
                    remain_after_exit: Some(false),
                },
            };
            let package_metadata = libssam::ssam_package::ssam_pkg_metadata::PackageMetadata::new(
                package_config,
                libssam::superblock::FsType::Erofs,
                libssam::ssam_package::PackageFsVerityInfo {
                    data_size: 0,
                    hash_size: 0,
                    table_params: "1 4096 4096 100 101 sha256 dummy_hash_root deadbeef".to_owned(),
                    hash_offset: 0,
                },
            )
            .expect("Failed to build package metadata");

            let inspect_response = InspectLocalPackageResult {
                package_path: "/mock/packages/test-pkg.ssam".to_owned(),
                package_data: Some((&package_metadata).into()),
                success: true,
                error_summary: None,
                error_details: None,
            };

            Ok(Response::new(ListPackageInfoFromFilesResponse {
                packages: vec![inspect_response],
            }))
        }
    }

    pub async fn spawn_fake_server() -> (
        SocketAddr,
        oneshot::Sender<()>,
        JoinHandle<Result<(), tonic::transport::Error>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind fake server");
        let addr = listener.local_addr().expect("Failed to get local addr");

        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let handle = tokio::spawn(async move {
            Server::builder()
                .add_service(RemoconServer::new(MockRemocon))
                .serve_with_incoming_shutdown(incoming, async {
                    shutdown_rx.await.ok();
                })
                .await
        });

        (addr, shutdown_tx, handle)
    }
}

#[cfg(test)]
mod grpc_tests {
    use super::*;
    use grpc_fake_server::spawn_fake_server;
    use libssam::remocon::remocon_client::RemoconClient;
    use tonic::transport::Channel;

    #[tokio::test]
    async fn test_grpc_list_packages() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client.list_packages().await.unwrap();
        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.entries[0].package_name, "test-pkg");

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_start_package() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client
            .start_package(vec!["test-pkg".to_owned()])
            .await
            .unwrap();
        let ops = response.results;
        assert_eq!(ops.len(), 1);
        assert!(ops[0].success);

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_stop_package() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client
            .stop_package(vec!["test-pkg".to_owned()])
            .await
            .unwrap();
        let ops = response.results;
        assert_eq!(ops.len(), 1);
        assert!(ops[0].success);

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_install_package() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client
            .install_package("/tmp/dummy.ssam", false, false)
            .await
            .unwrap();
        let package_data = response.package_data.as_ref().unwrap();
        assert_eq!(package_data.package_name, "dummy");
        assert_eq!(package_data.version, "1.0.0");

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_remove_package() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        client.remove_package("test-pkg").await.unwrap();

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_wipe_package_data() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client
            .wipe_package_data(vec!["test-pkg".to_owned()])
            .await
            .unwrap();
        let ops = response.results;
        assert_eq!(ops.len(), 1);
        assert!(ops[0].success);

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_get_timeline_info() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client.get_timeline_info().await.unwrap();
        assert_eq!(response.events.len(), 1);
        assert!(response.ssamd_uptime.is_some());

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_list_package_info() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client
            .list_package_info(vec!["test-pkg".to_owned()])
            .await
            .unwrap();
        assert_eq!(response.packages.len(), 1);
        assert_eq!(
            response.packages[0].package_path,
            "/mock/packages/test-pkg.ssam"
        );

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_list_package_info_empty_names() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client.list_package_info(Vec::new()).await.unwrap();
        let data = response.packages;
        assert_eq!(data.len(), 2);
        assert!(
            data.iter()
                .any(|p| p.success && p.status == "Running" && p.name == "test-pkg")
        );
        assert!(
            data.iter()
                .any(|p| p.status == "Broken" && p.name == "broken-pkg")
        );

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_start_package_error() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let response = client
            .start_package(vec!["error".to_owned()])
            .await
            .unwrap();
        let ops = response.results;
        assert_eq!(ops.len(), 1);
        assert!(!ops[0].success, "Per-package result should be failure");
        assert!(
            ops[0]
                .message
                .as_deref()
                .unwrap()
                .contains("Failed to start package"),
            "Message should contain failure description"
        );

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_install_package_error() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let error = client
            .install_package("/error.ssam", false, false)
            .await
            .expect_err("installation should return a gRPC status");
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<tonic::Status>()
                .is_some_and(|status| status.code() == tonic::Code::Internal)
        }));

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_connection_failure() {
        use std::time::Duration;

        let result = Channel::from_shared("http://127.0.0.1:1".to_string())
            .unwrap()
            .connect_timeout(Duration::from_millis(100))
            .connect()
            .await;

        assert!(result.is_err(), "Expected connection failure to port 1");
    }

    #[tokio::test]
    async fn test_grpc_is_local_mode() {
        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        // Test case 1: Localhost (127.0.0.1) -> should be local mode
        let client = GrpcClient::new(RemoconClient::new(channel.clone()), Ipv4Addr::LOCALHOST);
        assert!(client.is_local_mode(), "LOCALHOST should be local mode");

        // Test case 2: Non-local IP -> should NOT be local mode
        let non_local_ip = Ipv4Addr::new(192, 168, 0, 1);
        let client = GrpcClient::new(RemoconClient::new(channel), non_local_ip);
        assert!(
            !client.is_local_mode(),
            "External IP should not be local mode"
        );

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }
}
