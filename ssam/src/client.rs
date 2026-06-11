// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context as _, Result};
use std::net::Ipv4Addr;
use tonic::Request;
use tonic::transport::Channel;

use libssam::remocon::remocon_client::RemoconClient;
use libssam::remocon::{
    InspectLocalPackageRequest, InstallPackageRequest, PackageInformationRequest,
    RemovePackageRequest, StartPackageRequest, StopPackageRequest,
};

#[tonic::async_trait]
pub trait Client: Send {
    async fn list_packages_status(&mut self) -> Result<String>;
    /// Empty `names` means "all installed packages" — the server resolves the
    /// full list via `GetPackageNames`. Callers must gate this with an explicit
    /// user intent (e.g. `--all` flag) to prevent accidental bulk operations.
    async fn start_package(&mut self, names: Vec<String>) -> Result<String>;
    /// See [`Self::start_package`] for empty-names semantics.
    async fn stop_package(&mut self, names: Vec<String>) -> Result<String>;
    async fn install_package(
        &mut self,
        path: &str,
        force: bool,
        remove_data: bool,
    ) -> Result<String>;
    async fn remove_package(&mut self, name: &str) -> Result<String>;
    async fn get_timeline_info(&mut self) -> Result<String>;
    async fn get_package_info(&mut self, name: &str) -> Result<String>;
    async fn inspect_local_package(&mut self, path: &str) -> Result<String>;
    async fn get_all_package_info(&mut self) -> Result<String>;
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

    async fn list_packages_status(&mut self) -> Result<String> {
        let response = self
            .inner
            .list_packages_status(Request::new(()))
            .await
            .context("Daemon side error. list_package_status has failed.")?
            .into_inner();
        Ok(response.result)
    }

    async fn start_package(&mut self, names: Vec<String>) -> Result<String> {
        let request = Request::new(StartPackageRequest {
            package_names: names,
        });
        let response = self
            .inner
            .start_package(request)
            .await
            .context("Cannot start requested package(s)")?
            .into_inner();
        Ok(response.result)
    }

    async fn stop_package(&mut self, names: Vec<String>) -> Result<String> {
        let request = Request::new(StopPackageRequest {
            package_names: names,
        });
        let response = self
            .inner
            .stop_package(request)
            .await
            .context("Cannot stop requested package(s)")?
            .into_inner();
        Ok(response.result)
    }

    async fn install_package(
        &mut self,
        path: &str,
        force: bool,
        remove_data: bool,
    ) -> Result<String> {
        let request = Request::new(InstallPackageRequest {
            package_path: path.to_owned(),
            force,
            remove_data,
        });
        let response = self
            .inner
            .install_package(request)
            .await
            .with_context(|| format!("Cannot install package {path}"))?
            .into_inner();
        Ok(response.result)
    }

    async fn remove_package(&mut self, name: &str) -> Result<String> {
        let request = Request::new(RemovePackageRequest {
            package_name: name.to_owned(),
        });
        let response = self
            .inner
            .remove_package(request)
            .await
            .with_context(|| format!("Cannot remove package {name}"))?
            .into_inner();
        Ok(response.result)
    }

    async fn get_timeline_info(&mut self) -> Result<String> {
        let response = self
            .inner
            .get_timeline_info(Request::new(()))
            .await
            .context("Daemon side error. get_timeline_info has failed.")?
            .into_inner();
        Ok(response.result)
    }

    async fn get_package_info(&mut self, name: &str) -> Result<String> {
        let request = Request::new(PackageInformationRequest {
            package_name: name.to_owned(),
        });
        let response = self
            .inner
            .get_package_info(request)
            .await
            .with_context(|| format!("Cannot get package info for {name}"))?
            .into_inner();
        Ok(response.result)
    }

    async fn inspect_local_package(&mut self, path: &str) -> Result<String> {
        let request = Request::new(InspectLocalPackageRequest {
            package_path: path.to_owned(),
        });
        let response = self
            .inner
            .inspect_local_package(request)
            .await
            .with_context(|| format!("Cannot inspect local package: {path}"))?
            .into_inner();
        Ok(response.result)
    }

    async fn get_all_package_info(&mut self) -> Result<String> {
        let response = self
            .inner
            .get_all_package_info(Request::new(()))
            .await
            .context("Daemon side error. get_all_package_info has failed.")?
            .into_inner();
        Ok(response.result)
    }
}

#[cfg(test)]
pub struct MockClient {
    pub list_packages_result: Option<anyhow::Result<String>>,
    pub start_package_result: Option<anyhow::Result<String>>,
    pub stop_package_result: Option<anyhow::Result<String>>,
    pub install_package_result: Option<anyhow::Result<String>>,
    pub remove_package_result: Option<anyhow::Result<String>>,
    pub get_timeline_info_result: Option<anyhow::Result<String>>,
    pub get_package_info_result: Option<anyhow::Result<String>>,
    pub inspect_local_package_result: Option<anyhow::Result<String>>,
    pub get_all_package_info_result: Option<anyhow::Result<String>>,
    pub is_local: bool,
    pub captured_start_names: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    pub captured_stop_names: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[cfg(test)]
impl MockClient {
    pub fn new_success() -> Self {
        use libssam::json_result::serialize_metadata;
        use libssam::{
            InspectLocalPackageResponse, InstallResponse, JsonResult, PackageInfoResponse,
            PackageStatusEntry, StartStopResult,
        };

        let package_metadata = Self::make_test_metadata();

        let list_json = JsonResult::success(vec![PackageStatusEntry {
            package_name: "test-pkg".to_owned(),
            status: "running".to_owned(),
        }]);
        let timeline_json = JsonResult::success(Self::make_test_timeline());

        let package_info_data = PackageInfoResponse {
            broken: false,
            name: "test-pkg".to_owned(),
            status: "Running".to_owned(),
            package_path: "/mock/packages/test-pkg.ssam".to_owned(),
            metadata: Some(serialize_metadata(&package_metadata)),
            quota: Some(libssam::remocon_schema::QuotaInformation {
                enabled: true,
                limit: 2048,
            }),
            error_summary: None,
            error_details: None,
        };
        let package_info_json = JsonResult::success(package_info_data.clone());
        let all_package_info_json = JsonResult::success(vec![
            package_info_data,
            PackageInfoResponse {
                broken: true,
                name: "broken-pkg".to_owned(),
                status: "Broken".to_owned(),
                package_path: "/mock/packages/broken-pkg.ssam".to_owned(),
                metadata: None,
                quota: None,
                error_summary: Some("Parse error".to_owned()),
                error_details: Some("Failed to parse package.toml".to_owned()),
            },
        ]);

        let inspect_local_json = JsonResult::success(InspectLocalPackageResponse {
            package_path: "/mock/packages/test-pkg.ssam".to_owned(),
            metadata: serialize_metadata(&package_metadata),
        });
        let start_stop_json = JsonResult::success(vec![StartStopResult {
            package_name: "test-pkg".to_owned(),
            success: true,
            message: None,
        }]);
        let remove_json = JsonResult::<()>::success_empty();
        let install_json = JsonResult::success(InstallResponse {
            metadata: serialize_metadata(&package_metadata),
        });

        Self {
            list_packages_result: Some(Ok(list_json)),
            start_package_result: Some(Ok(start_stop_json.clone())),
            stop_package_result: Some(Ok(start_stop_json)),
            install_package_result: Some(Ok(install_json)),
            remove_package_result: Some(Ok(remove_json)),
            get_timeline_info_result: Some(Ok(timeline_json)),
            get_package_info_result: Some(Ok(package_info_json)),
            inspect_local_package_result: Some(Ok(inspect_local_json)),
            get_all_package_info_result: Some(Ok(all_package_info_json)),
            is_local: true,
            captured_start_names: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            captured_stop_names: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
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
                network: libssam::config::Network { mode: None },
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
                root_hash: "dummy_hash_root".to_owned(),
                hash_offset: 0,
            },
        )
        .expect("test metadata should be valid")
    }

    fn make_test_timeline() -> libssam::TimelineResponse {
        use libssam::remocon_schema::{TimelineEvent, TimelineEventKind};
        use std::time::Duration;

        libssam::TimelineResponse {
            events: vec![TimelineEvent {
                pkg: "test-pkg".to_owned(),
                phase: "Parse".to_owned(),
                duration_ns: 1_000_000_000u64,
                kind: TimelineEventKind::Completed,
            }],
            ssamd_uptime: Duration::from_secs(42),
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

    pub fn with_timeline_error(mut self) -> Self {
        self.get_timeline_info_result = Some(Err(anyhow::anyhow!("Timeline info failed")));
        self
    }

    pub fn with_pkg_info_error(mut self) -> Self {
        self.get_package_info_result = Some(Err(anyhow::anyhow!("Package info failed")));
        self
    }

    pub fn with_all_pkg_info_error(mut self) -> Self {
        self.get_all_package_info_result = Some(Err(anyhow::anyhow!("All package info failed")));
        self
    }

    pub fn with_broken_pkg_info(mut self) -> Self {
        use libssam::{JsonResult, PackageInfoResponse};

        let package_info_data = PackageInfoResponse {
            broken: true,
            name: "broken-pkg".to_owned(),
            status: "Broken".to_owned(),
            package_path: "/mock/packages/broken-pkg.ssam".to_owned(),
            metadata: None,
            quota: None,
            error_summary: Some("Parse error".to_owned()),
            error_details: Some("Failed to parse package.toml".to_owned()),
        };
        self.get_package_info_result = Some(Ok(JsonResult::success(package_info_data)));
        self
    }

    pub fn with_start_results(mut self, results: Vec<libssam::StartStopResult>) -> Self {
        use libssam::JsonResult;
        self.start_package_result = Some(Ok(JsonResult::success(results)));
        self
    }

    pub fn with_stop_results(mut self, results: Vec<libssam::StartStopResult>) -> Self {
        use libssam::JsonResult;
        self.stop_package_result = Some(Ok(JsonResult::success(results)));
        self
    }
}

#[cfg(test)]
#[tonic::async_trait]
impl Client for MockClient {
    fn is_local_mode(&self) -> bool {
        self.is_local
    }

    async fn list_packages_status(&mut self) -> anyhow::Result<String> {
        self.list_packages_result
            .take()
            .expect("list_packages_result not set")
    }

    async fn start_package(&mut self, names: Vec<String>) -> anyhow::Result<String> {
        self.captured_start_names.lock().unwrap().push(names);
        self.start_package_result
            .take()
            .expect("start_package_result not set")
    }

    async fn stop_package(&mut self, names: Vec<String>) -> anyhow::Result<String> {
        self.captured_stop_names.lock().unwrap().push(names);
        self.stop_package_result
            .take()
            .expect("stop_package_result not set")
    }

    async fn install_package(
        &mut self,
        _path: &str,
        _force: bool,
        _remove_data: bool,
    ) -> anyhow::Result<String> {
        self.install_package_result
            .take()
            .expect("install_package_result not set")
    }

    async fn remove_package(&mut self, _name: &str) -> anyhow::Result<String> {
        self.remove_package_result
            .take()
            .expect("remove_package_result not set")
    }

    async fn get_timeline_info(&mut self) -> anyhow::Result<String> {
        self.get_timeline_info_result
            .take()
            .expect("get_timeline_info_result not set")
    }

    async fn get_package_info(&mut self, _name: &str) -> anyhow::Result<String> {
        self.get_package_info_result
            .take()
            .expect("get_package_info_result not set")
    }

    async fn inspect_local_package(&mut self, _path: &str) -> anyhow::Result<String> {
        self.inspect_local_package_result
            .take()
            .expect("inspect_local_package_result not set")
    }

    async fn get_all_package_info(&mut self) -> anyhow::Result<String> {
        self.get_all_package_info_result
            .take()
            .expect("get_all_package_info_result not set")
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
        async fn list_packages_status(
            &self,
            _request: Request<()>,
        ) -> Result<Response<ListPackagesStatusResponse>, Status> {
            use libssam::{JsonResult, PackageStatusEntry};

            let data = vec![PackageStatusEntry {
                package_name: "test-pkg".to_owned(),
                status: "running".to_owned(),
            }];
            let result = JsonResult::success(data);

            Ok(Response::new(ListPackagesStatusResponse { result }))
        }

        async fn start_package(
            &self,
            request: Request<StartPackageRequest>,
        ) -> Result<Response<StartPackageResponse>, Status> {
            use libssam::{JsonResult, StartStopResult};

            let names = &request.get_ref().package_names;
            let ops: Vec<StartStopResult> = names
                .iter()
                .map(|name| {
                    if name == "error" {
                        StartStopResult {
                            package_name: name.clone(),
                            success: false,
                            message: Some("Failed to start package".to_owned()),
                        }
                    } else {
                        StartStopResult {
                            package_name: name.clone(),
                            success: true,
                            message: None,
                        }
                    }
                })
                .collect();
            let result = JsonResult::success(ops);

            Ok(Response::new(StartPackageResponse { result }))
        }

        async fn stop_package(
            &self,
            request: Request<StopPackageRequest>,
        ) -> Result<Response<StopPackageResponse>, Status> {
            use libssam::{JsonResult, StartStopResult};

            let names = &request.get_ref().package_names;
            let ops: Vec<StartStopResult> = names
                .iter()
                .map(|name| StartStopResult {
                    package_name: name.clone(),
                    success: true,
                    message: None,
                })
                .collect();
            let result = JsonResult::success(ops);
            Ok(Response::new(StopPackageResponse { result }))
        }

        async fn install_package(
            &self,
            request: Request<InstallPackageRequest>,
        ) -> Result<Response<InstallPackageResponse>, Status> {
            use std::path::Path;

            use libssam::json_result::serialize_metadata;
            use libssam::{InstallResponse, JsonResult};

            let package_path = &request.get_ref().package_path;
            let result = if package_path == "/error.ssam" {
                JsonResult::<InstallResponse>::failure("Installation failed")
            } else {
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
                                network: libssam::config::Network { mode: None },
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
                            root_hash: "dummy_hash_root".to_owned(),
                            hash_offset: 0,
                        },
                    )
                    .expect("Failed to build package metadata");
                JsonResult::success(InstallResponse {
                    metadata: serialize_metadata(&package_metadata),
                })
            };

            Ok(Response::new(InstallPackageResponse { result }))
        }

        async fn remove_package(
            &self,
            _request: Request<RemovePackageRequest>,
        ) -> Result<Response<RemovePackageResponse>, Status> {
            use libssam::JsonResult;

            let result = JsonResult::<()>::success_empty();
            Ok(Response::new(RemovePackageResponse { result }))
        }

        async fn get_timeline_info(
            &self,
            _request: Request<()>,
        ) -> Result<Response<TimeLineInformationResponse>, Status> {
            use libssam::remocon_schema::{TimelineEvent, TimelineEventKind};
            use libssam::{JsonResult, TimelineResponse};
            use std::time::Duration;

            let timeline_data = TimelineResponse {
                events: vec![TimelineEvent {
                    pkg: "test-pkg".to_owned(),
                    phase: "Parse".to_owned(),
                    duration_ns: 1_000_000_000u64,
                    kind: TimelineEventKind::Completed,
                }],
                ssamd_uptime: Duration::from_secs(42),
            };
            let result = JsonResult::success(timeline_data);

            Ok(Response::new(TimeLineInformationResponse { result }))
        }

        async fn get_package_info(
            &self,
            _request: Request<PackageInformationRequest>,
        ) -> Result<Response<PackageInformationResponse>, Status> {
            use libssam::json_result::serialize_metadata;
            use libssam::{JsonResult, PackageInfoResponse};

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
                    network: libssam::config::Network { mode: None },
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
                    root_hash: "dummy_hash_root".to_owned(),
                    hash_offset: 0,
                },
            )
            .expect("Failed to build package metadata");

            let package_info_data = PackageInfoResponse {
                broken: false,
                name: "test-pkg".to_owned(),
                status: "Running".to_owned(),
                package_path: "/mock/packages/test-pkg.ssam".to_owned(),
                metadata: Some(serialize_metadata(&package_metadata)),
                quota: Some(libssam::remocon_schema::QuotaInformation {
                    enabled: true,
                    limit: 2048,
                }),
                error_summary: None,
                error_details: None,
            };
            let result = JsonResult::success(package_info_data);

            Ok(Response::new(PackageInformationResponse { result }))
        }

        async fn inspect_local_package(
            &self,
            _request: Request<InspectLocalPackageRequest>,
        ) -> Result<Response<InspectLocalPackageResponse>, Status> {
            use libssam::JsonResult;
            use libssam::json_result::{
                InspectLocalPackageResponse as SchemaResponse, serialize_metadata,
            };

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
                    network: libssam::config::Network { mode: None },
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
                    root_hash: "dummy_hash_root".to_owned(),
                    hash_offset: 0,
                },
            )
            .expect("Failed to build package metadata");

            let inspect_response = SchemaResponse {
                package_path: "/mock/packages/test-pkg.ssam".to_owned(),
                metadata: serialize_metadata(&package_metadata),
            };
            let result = JsonResult::success(inspect_response);

            Ok(Response::new(InspectLocalPackageResponse { result }))
        }

        async fn get_all_package_info(
            &self,
            _request: Request<()>,
        ) -> Result<Response<AllPackageInformationResponse>, Status> {
            use libssam::{JsonResult, PackageInfoResponse};

            let all_package_info_data = vec![
                PackageInfoResponse {
                    broken: false,
                    name: "test-pkg".to_owned(),
                    status: "Running".to_owned(),
                    package_path: "/mock/packages/test-pkg.ssam".to_owned(),
                    metadata: None,
                    quota: Some(libssam::remocon_schema::QuotaInformation {
                        enabled: true,
                        limit: 2048,
                    }),
                    error_summary: None,
                    error_details: None,
                },
                PackageInfoResponse {
                    broken: true,
                    name: "broken-pkg".to_owned(),
                    status: "Broken".to_owned(),
                    package_path: "/mock/packages/broken-pkg.ssam".to_owned(),
                    metadata: None,
                    quota: None,
                    error_summary: Some("Parse error".to_owned()),
                    error_details: Some("Failed to parse package.toml".to_owned()),
                },
            ];
            let result = JsonResult::success(all_package_info_data);

            Ok(Response::new(AllPackageInformationResponse { result }))
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
    async fn test_grpc_list_packages_status() {
        use libssam::{JsonResult, PackageStatusEntry};

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client.list_packages_status().await.unwrap();
        let result: JsonResult<Vec<PackageStatusEntry>> =
            serde_json::from_str(&json_result).unwrap();

        assert!(result.success);
        assert!(result.data.is_some());

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_start_package() {
        use libssam::{JsonResult, StartStopResult};

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client
            .start_package(vec!["test-pkg".to_owned()])
            .await
            .unwrap();
        let result: JsonResult<Vec<StartStopResult>> = serde_json::from_str(&json_result).unwrap();

        assert!(result.success);
        let ops = result.data.unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].success);

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_stop_package() {
        use libssam::{JsonResult, StartStopResult};

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client
            .stop_package(vec!["test-pkg".to_owned()])
            .await
            .unwrap();
        let result: JsonResult<Vec<StartStopResult>> = serde_json::from_str(&json_result).unwrap();

        assert!(result.success);
        let ops = result.data.unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].success);

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_install_package() {
        use libssam::{InstallResponse, JsonResult};

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client
            .install_package("/tmp/dummy.ssam", false, false)
            .await
            .unwrap();
        let result: JsonResult<InstallResponse> = serde_json::from_str(&json_result).unwrap();

        assert!(result.success);
        let metadata = &result.data.as_ref().unwrap().metadata;
        assert_eq!(metadata.package_name, "dummy");
        assert_eq!(metadata.version, "1.0.0");

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_remove_package() {
        use libssam::JsonResult;

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client.remove_package("test-pkg").await.unwrap();
        let result: JsonResult<()> = serde_json::from_str(&json_result).unwrap();

        assert!(result.success);

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_get_timeline_info() {
        use libssam::{JsonResult, TimelineResponse};

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client.get_timeline_info().await.unwrap();
        let result: JsonResult<TimelineResponse> = serde_json::from_str(&json_result).unwrap();

        assert!(result.success);
        assert!(result.data.is_some());

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_get_package_info() {
        use libssam::{JsonResult, PackageInfoResponse};

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client.get_package_info("test-pkg").await.unwrap();
        let result: JsonResult<PackageInfoResponse> = serde_json::from_str(&json_result).unwrap();

        assert!(result.success);
        assert!(result.data.is_some());
        assert_eq!(
            result.data.as_ref().map(|info| info.package_path.as_str()),
            Some("/mock/packages/test-pkg.ssam")
        );

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_get_all_package_info() {
        use libssam::{JsonResult, PackageInfoResponse};

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client.get_all_package_info().await.unwrap();
        let result: JsonResult<Vec<PackageInfoResponse>> =
            serde_json::from_str(&json_result).unwrap();

        assert!(result.success);
        let data = result.data.expect("data should be present");
        assert_eq!(data.len(), 2);
        assert!(data.iter().any(|p| !p.broken && p.name == "test-pkg"));
        assert!(data.iter().any(|p| p.broken && p.name == "broken-pkg"));

        shutdown_tx.send(()).ok();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_grpc_start_package_error() {
        use libssam::{JsonResult, StartStopResult};

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client
            .start_package(vec!["error".to_owned()])
            .await
            .unwrap();
        let result: JsonResult<Vec<StartStopResult>> = serde_json::from_str(&json_result).unwrap();

        assert!(result.success, "Outer result should always be success=true");
        let ops = result.data.unwrap();
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
        use libssam::JsonResult;

        let (addr, shutdown_tx, handle) = spawn_fake_server().await;

        let channel = Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = GrpcClient::new(RemoconClient::new(channel), Ipv4Addr::LOCALHOST);

        let json_result = client
            .install_package("/error.ssam", false, false)
            .await
            .unwrap();
        let result: JsonResult<()> = serde_json::from_str(&json_result).unwrap();

        assert!(
            !result.success,
            "Expected success=false for path '/error.ssam'"
        );
        assert!(
            result.reason.unwrap().contains("Installation failed"),
            "Reason should contain failure message"
        );

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
