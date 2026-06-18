// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub mod error;
pub mod ssam_pkg_info;
pub mod ssam_pkg_metadata;
pub(crate) mod ssam_pkg_payload;
pub mod ssam_pkg_runtime_config;
pub mod ssam_pkg_seccomp;

pub use error::PackageParseError;

#[derive(
    Debug, Clone, PartialEq, bincode::Encode, bincode::Decode, serde::Serialize, serde::Deserialize,
)]
pub struct PackageFsVerityInfo {
    pub data_size: u64,
    pub hash_size: u64,
    pub root_hash: String,
    pub hash_offset: u64,
}

use crate::config::{PackageConfigSpec, SSAM_SERIALIZATION_CONFIG};
use crate::superblock::FsType;
use anyhow::{Context, Result, anyhow, bail};
use ssam_pkg_metadata::PackageMetadata;
use ssam_pkg_payload::Payloads;
use ssam_pkg_payload::{Payload, PayloadType, SSAM_PAYLOAD_CONFIGURATION};
use ssam_pkg_runtime_config::PackageRuntimeConfig;
pub use ssam_pkg_seccomp::PackageSeccompPolicy;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::{fs::File, path::Path};
use strum::IntoEnumIterator;
use zip::{ZipArchive, ZipWriter};

// Instead of usize from len(), clarifying it as u64
type PayloadSizeType = u64;

// Footer constants
const SSAM_PKG_MAGIC_LEN: usize = 24;
const SSAM_PKG_FORMAT_VERSION_LEN: usize = 8;
const SSAM_PKG_MAGIC: &[u8; SSAM_PKG_MAGIC_LEN] = b"MIRAEPLATFORMGAEBALGROUP";
const SSAM_PKG_FORMAT_VERSION: &[u8; SSAM_PKG_FORMAT_VERSION_LEN] = b"0.4.0\0\0\0";
const SSAM_PKG_FOOTER_LEN: usize = SSAM_PKG_MAGIC_LEN + SSAM_PKG_FORMAT_VERSION_LEN;

/// Read N bytes from the end of file at given offset.
fn read_footer_field<const N: usize>(
    pkg_fp: &mut File,
    offset_from_end: usize,
) -> Result<[u8; N], PackageParseError> {
    let offset = i64::try_from(offset_from_end)
        .with_context(|| format!("Offset {offset_from_end} bytes from end of file overflows i64"))
        .map_err(|source| PackageParseError::Io {
            message: "Package file offset too large to seek.".to_string(),
            source,
        })?;
    // SeekFrom::End(n) seeks relative to the end of the file.
    // Negative n moves backwards from the end.
    pkg_fp
        .seek(SeekFrom::End(-offset))
        .with_context(|| format!("Seeking to -{offset} bytes from end of file"))
        .map_err(|source| PackageParseError::Io {
            message: "Failed to seek package file.".to_string(),
            source,
        })?;

    let mut buffer = [0u8; N];
    pkg_fp
        .read_exact(&mut buffer)
        .with_context(|| format!("Reading {N} bytes from footer field"))
        .map_err(|source| PackageParseError::Io {
            message: "Failed to read package file.".to_string(),
            source,
        })?;

    Ok(buffer)
}

/// Verify the package file has a valid footer (MAGIC and `FORMAT_VERSION`).
fn verify_footer(pkg_fp: &mut File) -> Result<(), PackageParseError> {
    let magic = read_footer_field::<SSAM_PKG_MAGIC_LEN>(pkg_fp, SSAM_PKG_MAGIC_LEN)?;
    if &magic != SSAM_PKG_MAGIC {
        return Err(PackageParseError::InvalidMagic);
    }

    let version_offset_from_end = SSAM_PKG_FOOTER_LEN;
    let version =
        read_footer_field::<SSAM_PKG_FORMAT_VERSION_LEN>(pkg_fp, version_offset_from_end)?;
    if &version != SSAM_PKG_FORMAT_VERSION {
        let expected = String::from_utf8_lossy(SSAM_PKG_FORMAT_VERSION)
            .trim_end_matches('\0')
            .to_string();
        let actual = String::from_utf8_lossy(&version)
            .trim_end_matches('\0')
            .to_string();
        return Err(PackageParseError::InvalidFormatVersion { expected, actual });
    }

    Ok(())
}

/// Append footer to the end of the package file.
fn write_footer(pkg_fp: &mut File) -> Result<()> {
    pkg_fp
        .write_all(SSAM_PKG_FORMAT_VERSION)
        .context("Failed to write FORMAT_VERSION to package file")?;
    pkg_fp
        .write_all(SSAM_PKG_MAGIC)
        .context("Failed to write MAGIC to package file")?;
    Ok(())
}

#[derive(Debug)]
pub struct PackageFile {
    metadata: PackageMetadata,
    runtime_config: PackageRuntimeConfig,
    seccomp_policy: PackageSeccompPolicy,
    payloads: Payloads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PkgfsExtent {
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone)]
pub struct PackageFilesystem {
    pub(crate) payload: Payload,
    pub(crate) pkgfs_type: FsType,
    pub(crate) verity_info: PackageFsVerityInfo,
}

impl PackageFilesystem {
    /// Constructs a [`PackageFilesystem`] for an image already embedded in a
    /// package file at the given offset and length. Primarily intended for
    /// cross-crate test mocks that need to fabricate a filesystem handle
    /// without depending on the internal payload representation.
    #[must_use]
    pub fn new(
        offset: u64,
        length: u64,
        pkgfs_type: FsType,
        verity_info: PackageFsVerityInfo,
    ) -> Self {
        Self {
            payload: Payload::Internal((offset, length)),
            pkgfs_type,
            verity_info,
        }
    }

    /// Returns the location of the filesystem image inside the package file,
    /// or [`None`] if the payload is not an embedded image.
    #[must_use]
    pub fn pkgfs_extent(&self) -> Option<PkgfsExtent> {
        match &self.payload {
            Payload::Internal((offset, length)) => Some(PkgfsExtent {
                offset: *offset,
                length: *length,
            }),
            Payload::External(_) | Payload::Data(_) => None,
        }
    }

    /// Returns the filesystem type of the package image.
    #[must_use]
    pub fn pkgfs_type(&self) -> FsType {
        self.pkgfs_type
    }

    /// Returns the dm-verity metadata for the package image.
    #[must_use]
    pub fn verity_info(&self) -> &PackageFsVerityInfo {
        &self.verity_info
    }
}

impl PackageFile {
    fn new(
        metadata: PackageMetadata,
        runtime_config: PackageRuntimeConfig,
        seccomp_policy: PackageSeccompPolicy,
        payloads: Payloads,
    ) -> Self {
        PackageFile {
            metadata,
            runtime_config,
            seccomp_policy,
            payloads,
        }
    }

    /// Returns the metadata for this package.
    #[must_use]
    pub fn metadata(&self) -> &PackageMetadata {
        &self.metadata
    }

    /// Returns the runtime config for this package.
    #[must_use]
    pub fn runtime_config(&self) -> &PackageRuntimeConfig {
        &self.runtime_config
    }

    /// Returns the seccomp policy for this package.
    #[must_use]
    pub fn seccomp_policy(&self) -> &PackageSeccompPolicy {
        &self.seccomp_policy
    }

    /// Returns the filesystem info for this package.
    ///
    /// # Errors
    ///
    /// Returns an error if the package filesystem payload is missing.
    pub fn pkgfs(&self) -> Result<PackageFilesystem> {
        Ok(PackageFilesystem {
            payload: self
                .payloads
                .get(PayloadType::PackageFilesystem)
                .ok_or(anyhow!("No pkgfs found in package file"))?
                .clone(),
            pkgfs_type: *self.metadata().pkgfs_type(),
            verity_info: self.metadata().pkgfs_verity_info().clone(),
        })
    }

    /// Constructs a [`PackageFile`] from source files: a package config TOML,
    /// a runtime config, a seccomp policy, and a pre-built filesystem image.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or parsing the package config file, runtime config,
    /// seccomp policy, or creating the package metadata fails.
    pub fn from_source(
        package_config_file: impl AsRef<Path>,
        runtime_config: impl AsRef<Path>,
        seccomp_policy: impl AsRef<Path>,
        pkgfs: &PackageFilesystem,
    ) -> Result<Self> {
        let package_config_str = fs::read_to_string(&package_config_file).with_context(|| {
            format!(
                "Unable to read package config file: {}",
                package_config_file.as_ref().display()
            )
        })?;
        let package_config: PackageConfigSpec =
            toml::from_str(&package_config_str).with_context(|| {
                format!(
                    "Failed to parse package config spec from file {}",
                    package_config_file.as_ref().display()
                )
            })?;

        let runtime_config =
            PackageRuntimeConfig::from_file(&runtime_config).with_context(|| {
                format!(
                    "Unable to initialize package runtime config from {}",
                    runtime_config.as_ref().display(),
                )
            })?;

        let seccomp_policy =
            PackageSeccompPolicy::from_file(&seccomp_policy).with_context(|| {
                format!(
                    "Unable to load seccomp policy from {}",
                    seccomp_policy.as_ref().display()
                )
            })?;

        let pkgfs_payload = pkgfs.payload.clone();

        let metadata =
            PackageMetadata::new(package_config, pkgfs.pkgfs_type, pkgfs.verity_info.clone())
                .with_context(|| {
                    format!(
                        "Failed to create package metadata for {}",
                        package_config_file.as_ref().display()
                    )
                })?;

        let ssam_payloads =
            Payloads::init().set_mut(PayloadType::PackageFilesystem, Some(pkgfs_payload));

        Ok(PackageFile {
            metadata,
            runtime_config,
            seccomp_policy,
            payloads: ssam_payloads,
        })
    }

    /// Opens and verifies an SSAM package file, then parses its embedded payloads
    /// (metadata, runtime config, and seccomp policy) after signature verification.
    ///
    /// # Errors
    ///
    /// Returns an error if opening the package file, verifying the signature, or parsing
    /// any of the embedded payloads (metadata, runtime config, seccomp policy) fails.
    pub fn from_file_verified(
        package_file: impl AsRef<std::path::Path>,
        public_key: impl AsRef<std::path::Path>,
    ) -> Result<PackageFile, PackageParseError> {
        let package_file = package_file.as_ref();
        let mut pkg_fp = File::open(package_file).map_err(|e| PackageParseError::FileOpen {
            path: package_file.to_path_buf(),
            source: e,
        })?;

        let ssam_payloads = Self::load_payloads_info(&mut pkg_fp, &public_key)?;

        let mut builder = PackageFileBuilder::new();
        for (payload_type, payload) in ssam_payloads.iter() {
            let payload = payload
                .as_ref()
                .ok_or_else(|| PackageParseError::ParseFailed {
                    source: anyhow!("Missing payload for {payload_type:?}"),
                })?;

            // Parameters would be handled per Payload
            if let Payload::Internal((offset, size)) = payload {
                // TODO: Make more generic to handle different payload types with PayloadConfig
                match payload_type {
                    PayloadType::Metadata => {
                        pkg_fp
                            .seek(std::io::SeekFrom::Start(*offset))
                            .with_context(|| {
                                format!("Seeking to metadata payload at offset {offset}")
                            })
                            .map_err(|source| PackageParseError::Io {
                                message: "Failed to seek package file.".to_string(),
                                source,
                            })?;
                        let metadata = PackageMetadata::deserialize(&mut pkg_fp)?;
                        builder.set_metadata(metadata);
                    }
                    PayloadType::RuntimeConfig => {
                        pkg_fp
                            .seek(std::io::SeekFrom::Start(*offset))
                            .with_context(|| {
                                format!("Seeking to runtime config payload at offset {offset}")
                            })
                            .map_err(|source| PackageParseError::Io {
                                message: "Failed to seek package file.".to_string(),
                                source,
                            })?;
                        let mut fp = pkg_fp
                            .try_clone()
                            .context("Cloning file handle for runtime config payload read")
                            .map_err(|source| PackageParseError::Io {
                                message: "Failed to clone file handle.".to_string(),
                                source,
                            })?
                            .take(*size);
                        let runtime_config = PackageRuntimeConfig::deserialize(&mut fp)?;
                        builder.set_runtime_config(runtime_config);
                    }
                    PayloadType::SeccompPolicy => {
                        pkg_fp
                            .seek(std::io::SeekFrom::Start(*offset))
                            .with_context(|| {
                                format!("Seeking to seccomp policy payload at offset {offset}")
                            })
                            .map_err(|source| PackageParseError::Io {
                                message: "Failed to seek package file.".to_string(),
                                source,
                            })?;
                        let mut fp = pkg_fp
                            .try_clone()
                            .context("Cloning file handle for seccomp policy payload read")
                            .map_err(|source| PackageParseError::Io {
                                message: "Failed to clone file handle.".to_string(),
                                source,
                            })?
                            .take(*size);
                        let seccomp_policy = PackageSeccompPolicy::deserialize(&mut fp)?;
                        builder.set_seccomp_policy(seccomp_policy);
                    }
                    _ => (), // Other payloads are kept as they are
                }
            } else {
                return Err(PackageParseError::ParseFailed {
                    source: anyhow!("Expected internal payload for {payload_type:?}"),
                })?;
            }
        }

        builder.set_payloads(ssam_payloads);

        builder.build()
    }

    /// Signs and serializes this package to an SSAM package file at `output_filepath`.
    ///
    /// The package payloads (metadata, runtime config, seccomp policy, and filesystem
    /// images) are written sequentially, then a signed footer is appended.
    ///
    /// # Errors
    ///
    /// Returns an error if creating the output file, signing, serializing payloads,
    /// or writing the package footer fails.
    pub fn wrap(
        &self,
        output_filepath: impl AsRef<Path>,
        private_key: impl AsRef<Path>,
    ) -> Result<()> {
        let mut ssam_payloads = Payloads::init()
            .set_mut(
                PayloadType::Metadata,
                Some(Payload::Data(
                    self.metadata
                        .serialize()
                        .context("Failed to serialize PackageMetadata")?,
                )),
            )
            .set_mut(
                PayloadType::RuntimeConfig,
                Some(Payload::Data(
                    self.runtime_config
                        .serialize()
                        .context("Failed to serialize PackageRuntimeConfig")?,
                )),
            )
            .set_mut(
                PayloadType::SeccompPolicy,
                Some(Payload::Data(self.seccomp_policy.serialize())),
            )
            .set_mut(
                PayloadType::PackageFilesystem,
                self.payloads.get(PayloadType::PackageFilesystem).cloned(),
            );

        ssam_payloads
            .sign(private_key)
            .context("Failed to sign package")?;

        let package_filepath = output_filepath.as_ref();
        let package_file = File::create(package_filepath).with_context(|| {
            format!(
                "Failed to create a package file {}",
                package_filepath.display()
            )
        })?;
        let mut package_zip = zip::ZipWriter::new(&package_file);

        Self::wrap_payloads(&ssam_payloads, &mut package_zip)
            .with_context(|| format!("Failed to wrap payloads {ssam_payloads:?}"))?;

        let mut archive = package_zip
            .finish_into_readable()
            .context("Failed to finish zip package")?;

        let mut package_file = File::options()
            .append(true)
            .open(package_filepath)
            .context("Failed to open package file for appending payloads info")?;
        Self::wrap_payloads_info(&mut package_file, &mut archive).with_context(|| {
            format!(
                "Failed to wrap payloads info to file {}",
                package_filepath.display()
            )
        })?;

        write_footer(&mut package_file).with_context(|| {
            format!(
                "Failed to write footer to package file {}",
                package_filepath.display()
            )
        })?;

        Ok(())
    }

    fn wrap_payloads<R>(payloads: &Payloads, package_zip: &mut ZipWriter<R>) -> Result<()>
    where
        R: Seek + Write,
    {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        for (payload_type, payload) in payloads {
            let conf = SSAM_PAYLOAD_CONFIGURATION
                .get(payload_type)
                .with_context(|| format!("Payload configuration for {payload_type} is missing!"))?;
            package_zip
                .start_file(conf.filename(), options)
                .context("Failed to start file in zip")?;
            let payload = payload
                .as_ref()
                .with_context(|| format!("Payload for {payload_type} does NOT exist!"))?;
            match payload {
                Payload::Data(payload) => {
                    package_zip
                        .write_all(payload)
                        .with_context(|| format!("Failed to write payload {payload_type}"))?;
                }
                Payload::External(path) => {
                    let mut f = File::open(path).with_context(|| {
                        format!("Failed to open external payload file {}", path.display())
                    })?;
                    std::io::copy(&mut f, package_zip).with_context(|| {
                        format!(
                            "Failed to copy content of the external payload file {} to zip",
                            path.display()
                        )
                    })?;
                }
                Payload::Internal(_) => {
                    bail!("Unsupported payload type for {payload_type}: {payload:?}");
                }
            }
        }
        Ok(())
    }

    fn wrap_payloads_info<W, R>(package_file: &mut W, archive: &mut ZipArchive<R>) -> Result<()>
    where
        W: Write + Seek,
        R: Read + Seek,
    {
        let mut package_payloads = Payloads::init();

        for payload_type in PayloadType::iter() {
            let filename = SSAM_PAYLOAD_CONFIGURATION
                .get(&payload_type)
                .with_context(|| format!("Payload configuration for {payload_type} is missing!"))?
                .filename();
            let payload = archive
                .by_name(filename)
                .with_context(|| format!("Failed to find payload {payload_type} from archive"))?;
            let data_start = payload
                .data_start()
                .with_context(|| format!("Failed to get data start for {payload_type}"))?;
            let p = Payload::Internal((data_start, payload.size()));
            package_payloads.set(payload_type, Some(p));
        }

        let serialized_payloads = package_payloads
            .serialize()
            .context("Failed to serialize entire payloads info")?;
        let payloads_size = serialized_payloads.len() as PayloadSizeType;
        // Since default is .with_variable_int_encoding(), which causes
        // unable to determine the size of the size field when parsing.
        let serialized_payload_size =
            bincode::encode_to_vec(payloads_size, SSAM_SERIALIZATION_CONFIG).with_context(
                || format!("Failed to serialize size of payloads info: {payloads_size}"),
            )?;

        package_file
            .write_all(&serialized_payloads)
            .context("Failed to write serialized payloads info to package file")?;

        package_file
            .write_all(&serialized_payload_size)
            .context("Failed to write serialized size of payloads info to package file")?;
        Ok(())
    }

    fn load_payloads_info(
        pkg_fp: &mut File,
        public_key: impl AsRef<std::path::Path>,
    ) -> Result<Payloads, PackageParseError> {
        verify_footer(pkg_fp)?;

        let pkg_file_size = pkg_fp
            .metadata()
            .context("Querying file metadata before payload parsing")
            .map_err(|source| PackageParseError::Io {
                message: "Failed to get package file metadata.".to_string(),
                source,
            })?
            .len();
        let footer_len = SSAM_PKG_FOOTER_LEN as u64;
        let payloads_size_len = size_of::<PayloadSizeType>() as u64;
        let payloads_size_offset = pkg_file_size.saturating_sub(footer_len + payloads_size_len);

        pkg_fp
            .seek(SeekFrom::Start(payloads_size_offset))
            .with_context(|| {
                format!("Seeking to payloads size field at offset {payloads_size_offset}")
            })
            .map_err(|source| PackageParseError::Io {
                message: "Failed to seek package file.".to_string(),
                source,
            })?;
        let payloads_size: PayloadSizeType =
            bincode::decode_from_std_read(pkg_fp, SSAM_SERIALIZATION_CONFIG).map_err(|e| {
                PackageParseError::ParseFailed {
                    source: anyhow!(e).context("Failed to decode payloads size"),
                }
            })?;

        // File layout: [zip data | payloads_info | payloads_size_field | footer]
        let max_payloads_size = pkg_file_size.saturating_sub(footer_len + payloads_size_len);
        if payloads_size > max_payloads_size {
            return Err(PackageParseError::ParseFailed {
                source: anyhow!(
                    "Declared payloads size ({payloads_size}) exceeds \
                     available file space ({max_payloads_size})"
                ),
            });
        }

        let payloads_offset =
            pkg_file_size.saturating_sub(footer_len + payloads_size_len + payloads_size);

        pkg_fp
            .seek(SeekFrom::Start(payloads_offset))
            .with_context(|| format!("Seeking to payloads data at offset {payloads_offset}"))
            .map_err(|source| PackageParseError::Io {
                message: "Failed to seek package file.".to_string(),
                source,
            })?;

        Payloads::deserialize_from_pkg_verified(pkg_fp, &public_key)
    }
}

impl PackageFilesystem {
    pub fn from_source(
        pkgfs_image: impl AsRef<Path>,
        pkgfs_type: FsType,
        verity_info: PackageFsVerityInfo,
    ) -> Self {
        PackageFilesystem {
            payload: Payload::External(pkgfs_image.as_ref().to_path_buf()),
            pkgfs_type,
            verity_info,
        }
    }
}

struct PackageFileBuilder {
    metadata: Option<PackageMetadata>,
    runtime_config: Option<PackageRuntimeConfig>,
    seccomp_policy: Option<PackageSeccompPolicy>,
    payloads: Option<Payloads>,
}

/// Builder for constructing `PackageFile`. Fields can be overwritten if set multiple times.
impl PackageFileBuilder {
    fn new() -> Self {
        PackageFileBuilder {
            metadata: None,
            runtime_config: None,
            seccomp_policy: None,
            payloads: None,
        }
    }

    fn set_metadata(&mut self, metadata: PackageMetadata) {
        self.metadata = Some(metadata);
    }

    fn set_runtime_config(&mut self, runtime_config: PackageRuntimeConfig) {
        self.runtime_config = Some(runtime_config);
    }

    fn set_seccomp_policy(&mut self, seccomp_policy: PackageSeccompPolicy) {
        self.seccomp_policy = Some(seccomp_policy);
    }

    fn set_payloads(&mut self, payloads: Payloads) {
        self.payloads = Some(payloads);
    }

    fn build(self) -> Result<PackageFile, PackageParseError> {
        let (Some(metadata), Some(runtime_config), Some(seccomp_policy), Some(payloads)) = (
            self.metadata,
            self.runtime_config,
            self.seccomp_policy,
            self.payloads,
        ) else {
            return Err(PackageParseError::ParseFailed {
                source: anyhow!("Package is missing required components"),
            });
        };

        Ok(PackageFile::new(
            metadata,
            runtime_config,
            seccomp_policy,
            payloads,
        ))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::{Container, Network, Package, PackageConfigSpec, Security, Service};
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use rsa::{RsaPrivateKey, RsaPublicKey, rand_core};
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::tempdir;

    pub(crate) fn make_verity(root_hash: &str, hash_offset: u64) -> PackageFsVerityInfo {
        PackageFsVerityInfo {
            data_size: 0,
            hash_size: 0,
            root_hash: root_hash.to_string(),
            hash_offset,
        }
    }

    fn create_test_package_config() -> PackageConfigSpec {
        PackageConfigSpec {
            package: Package {
                name: "test_package".to_string(),
                autostart: Some(true),
                version: "0.0.1".to_string(),
                description: "A test package".to_string(),
            },
            container: Container {
                storage_limit: Some(1000),
                data_dirs: Some("/test/data1:/test/data2".to_string()),
                security: Security {
                    seccomp: true,
                    mac: true,
                },
                network: Network {
                    mode: None,
                    interface_name: None,
                },
            },
            service: Service {
                service_type: "notify".to_string(),
                bus_name: Some("com.test.package".to_string()),
                remain_after_exit: Some(false),
            },
        }
    }

    fn check_test_package_config(config: &PackageConfigSpec) {
        assert_eq!(config.package.name, "test_package");
        assert_eq!(config.container.storage_limit, Some(1000));
        assert_eq!(config.service.service_type, "notify");
    }

    fn create_test_runtime_config(output_file: impl AsRef<Path>) {
        let content = r#"{
            "ociVersion": "1.0.0",
            "process": {
                "terminal": false,
                "user": {"uid": 0, "gid": 0},
                "args": ["sh"],
                "env": ["PATH=/usr/bin", "TERM=xterm"],
                "cwd": "/"
            },
            "hostname": "test"
        }"#;
        fs::write(output_file, content).unwrap();
    }

    fn create_test_ssam_pkg_filesystem() -> PackageFilesystem {
        let temp_dir = tempdir().unwrap();
        let pkgfs_path = temp_dir.path().join("test.img");

        File::create(&pkgfs_path)
            .unwrap()
            .write_all(b"dummy pkgfs data")
            .unwrap();

        PackageFilesystem::from_source(
            pkgfs_path,
            FsType::Erofs,
            make_verity("test_hash_root_value", 0u64),
        )
    }

    fn create_test_keys(private_key_path: impl AsRef<Path>, public_key_path: impl AsRef<Path>) {
        let mut rng = rand_core::OsRng;
        let bits = 2048;
        let private_key = RsaPrivateKey::new(&mut rng, bits).expect("failed to generate a key");
        let public_key = RsaPublicKey::from(&private_key);

        let private_key_pem = private_key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let public_key_pem = public_key
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();

        std::fs::write(&private_key_path, private_key_pem).unwrap();
        std::fs::write(&public_key_path, public_key_pem).unwrap();
    }

    fn create_test_package_config_file(output_file: impl AsRef<Path>) {
        // Create package config file
        let package_config_content = r#"
                [package]
                name = "test_package"
                autostart = true
                version = "0.0.1"
                description = "A test package"

                [container]
                storage_limit = 2000
                data_dirs = "/app/data:/app/logs"

                [container.security]
                seccomp = true
                mac = true

                [container.network]

                [service]
                service_type = "notify"
                bus_name = "com.test.service"
                remain_after_exit = false
        "#;
        fs::write(output_file, package_config_content).unwrap();
    }

    fn create_test_seccomp_policy_file(output_file: impl AsRef<Path>) {
        let content = r#"{
            "defaultAction": "SCMP_ACT_ERRNO",
            "architectures": ["SCMP_ARCH_X86_64"],
            "syscalls": [
                {
                    "names": ["read", "write"],
                    "action": "SCMP_ACT_ALLOW"
                }
            ]
        }"#;
        fs::write(output_file, content).unwrap();
    }

    fn create_test_seccomp_policy() -> PackageSeccompPolicy {
        PackageSeccompPolicy::default_policy()
    }

    #[test]
    fn test_ssam_package_file_new() {
        let metadata = PackageMetadata::new(
            create_test_package_config(),
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap();

        let temp_dir = tempdir().unwrap();
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let runtime_config = PackageRuntimeConfig::from_file(&runtime_config_path).unwrap();
        let seccomp_policy = create_test_seccomp_policy();
        let payloads = Payloads::init();

        let package_file = PackageFile::new(metadata, runtime_config, seccomp_policy, payloads);

        assert_eq!(package_file.metadata().package.name, "test_package");
        assert_eq!(package_file.runtime_config().version(), "1.0.0");
    }

    #[test]
    fn test_ssam_package_file_metadata_access() {
        let metadata = PackageMetadata::new(
            create_test_package_config(),
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap();

        let temp_dir = tempdir().unwrap();
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let runtime_config = PackageRuntimeConfig::from_file(&runtime_config_path).unwrap();
        let seccomp_policy = create_test_seccomp_policy();
        let payloads = Payloads::init();

        let package_file = PackageFile::new(metadata, runtime_config, seccomp_policy, payloads);

        let retrieved_metadata = package_file.metadata();
        assert_eq!(retrieved_metadata.package.name, "test_package");
        assert_eq!(retrieved_metadata.service.service_type, "notify");
        assert_eq!(
            retrieved_metadata.pkgfs_verity_info().root_hash,
            "test_hash_root"
        );
    }

    #[test]
    fn test_ssam_package_file_runtime_config_access() {
        let metadata = PackageMetadata::new(
            create_test_package_config(),
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap();

        let temp_dir = tempdir().unwrap();
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let runtime_config = PackageRuntimeConfig::from_file(&runtime_config_path).unwrap();
        let seccomp_policy = create_test_seccomp_policy();
        let payloads = Payloads::init();

        let package_file = PackageFile::new(metadata, runtime_config, seccomp_policy, payloads);

        let retrieved_runtime_config = package_file.runtime_config();
        assert_eq!(retrieved_runtime_config.version(), "1.0.0");
        assert_eq!(retrieved_runtime_config.hostname().as_deref(), Some("test"));
    }

    #[test]
    fn test_ssam_package_file_pkgfs_missing() {
        let metadata = PackageMetadata::new(
            create_test_package_config(),
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap();

        let temp_dir = tempdir().unwrap();
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let runtime_config = PackageRuntimeConfig::from_file(&runtime_config_path).unwrap();
        let seccomp_policy = create_test_seccomp_policy();
        let payloads = Payloads::init(); // Empty payloads

        let package_file = PackageFile::new(metadata, runtime_config, seccomp_policy, payloads);

        let result = package_file.pkgfs();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No pkgfs found"));
    }

    #[test]
    fn test_ssam_package_file_pkgfs_success() {
        let metadata = PackageMetadata::new(
            create_test_package_config(),
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap();

        let temp_dir = tempdir().unwrap();
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let runtime_config = PackageRuntimeConfig::from_file(&runtime_config_path).unwrap();
        let seccomp_policy = create_test_seccomp_policy();

        let payloads = Payloads::init().set_mut(
            PayloadType::PackageFilesystem,
            Some(Payload::Data(b"pkgfs_data".to_vec())),
        );

        let package_file = PackageFile::new(metadata, runtime_config, seccomp_policy, payloads);

        let result = package_file.pkgfs();
        assert!(result.is_ok());

        let pkgfs = result.unwrap();
        assert_eq!(pkgfs.pkgfs_type, FsType::Erofs);
        assert_eq!(pkgfs.verity_info.root_hash, "test_hash_root");
    }

    #[test]
    fn test_ssam_package_file_from_source() {
        let temp_dir = tempdir().unwrap();

        // Create package config file
        let package_config_path = temp_dir.path().join("package.toml");
        create_test_package_config_file(&package_config_path);

        // Create runtime config file
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        // Create seccomp policy file
        let seccomp_policy_path = temp_dir.path().join("seccomp.json");
        create_test_seccomp_policy_file(&seccomp_policy_path);

        let pkgfs_path = temp_dir.path().join("test.img");
        File::create(&pkgfs_path)
            .unwrap()
            .write_all(b"dummy pkgfs data")
            .unwrap();
        let expected_hash_offset: u64 = 8192;
        let pkgfs = PackageFilesystem::from_source(
            pkgfs_path,
            FsType::Erofs,
            make_verity("test_hash_root_value", expected_hash_offset),
        );

        let result = PackageFile::from_source(
            &package_config_path,
            &runtime_config_path,
            &seccomp_policy_path,
            &pkgfs,
        );

        assert!(result.is_ok());
        let package_file = result.unwrap();

        assert_eq!(package_file.metadata().package.name, "test_package");
        assert_eq!(package_file.metadata().container.storage_limit, Some(2000));
        assert_eq!(package_file.runtime_config().version(), "1.0.0");
        assert_eq!(
            package_file.metadata().pkgfs_verity_info().hash_offset,
            expected_hash_offset
        );
    }

    #[test]
    fn test_ssam_package_file_from_source_invalid_config() {
        let temp_dir = tempdir().unwrap();

        // Create invalid package config file
        let package_config_path = temp_dir.path().join("package.toml");
        fs::write(&package_config_path, "invalid toml content").unwrap();

        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let seccomp_policy_path = temp_dir.path().join("seccomp.json");
        create_test_seccomp_policy_file(&seccomp_policy_path);

        let pkgfs = create_test_ssam_pkg_filesystem();

        let result = PackageFile::from_source(
            &package_config_path,
            &runtime_config_path,
            &seccomp_policy_path,
            &pkgfs,
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Failed to parse package config")
        );
    }

    #[test]
    fn test_ssam_package_file_from_source_missing_file() {
        let temp_dir = tempdir().unwrap();
        let nonexistent_path = temp_dir.path().join("nonexistent.toml");
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let seccomp_policy_path = temp_dir.path().join("seccomp.json");
        create_test_seccomp_policy_file(&seccomp_policy_path);

        let pkgfs = create_test_ssam_pkg_filesystem();

        let result = PackageFile::from_source(
            &nonexistent_path,
            &runtime_config_path,
            &seccomp_policy_path,
            &pkgfs,
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unable to read package config file")
        );
    }

    #[test]
    fn test_ssam_pkg_filesystem_from_source() {
        let temp_dir = tempdir().unwrap();
        let pkgfs_path = temp_dir.path().join("filesystem.img");

        File::create(&pkgfs_path)
            .unwrap()
            .write_all(b"filesystem data")
            .unwrap();

        let filesystem = PackageFilesystem::from_source(
            &pkgfs_path,
            FsType::Ext4,
            make_verity("hash_root_test", 0u64),
        );

        assert_eq!(filesystem.pkgfs_type, FsType::Ext4);
        assert_eq!(filesystem.verity_info.root_hash, "hash_root_test");
        assert_eq!(filesystem.verity_info.hash_offset, 0u64);
        assert!(matches!(filesystem.payload, Payload::External(ref p) if p == &pkgfs_path));
    }

    #[test]
    fn test_ssam_package_file_builder_new() {
        let builder = PackageFileBuilder::new();
        assert!(builder.metadata.is_none());
        assert!(builder.runtime_config.is_none());
        assert!(builder.payloads.is_none());
    }

    #[test]
    fn test_ssam_package_file_builder_set_metadata() {
        let mut builder = PackageFileBuilder::new();
        let metadata = PackageMetadata::new(
            create_test_package_config(),
            FsType::Erofs,
            make_verity("test_hash", 0),
        )
        .unwrap();

        builder.set_metadata(metadata);
        assert!(builder.metadata.is_some());
    }

    #[test]
    fn test_ssam_package_file_builder_set_runtime_config() {
        let temp_dir = tempdir().unwrap();
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let mut builder = PackageFileBuilder::new();
        let runtime_config = PackageRuntimeConfig::from_file(&runtime_config_path).unwrap();

        builder.set_runtime_config(runtime_config);
        assert!(builder.runtime_config.is_some());
    }

    #[test]
    fn test_ssam_package_file_builder_set_payloads() {
        let mut builder = PackageFileBuilder::new();
        let payloads = Payloads::init();

        builder.set_payloads(payloads);
        assert!(builder.payloads.is_some());
    }

    #[test]
    fn test_ssam_package_file_builder_build_incomplete() {
        let builder = PackageFileBuilder::new();
        let result = builder.build();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageParseError::ParseFailed { .. }),
            "Expected ParseFailed error, got: {err:?}"
        );
    }

    #[test]
    fn test_ssam_package_file_builder_build_success() {
        let temp_dir = tempdir().unwrap();
        let runtime_config_path = temp_dir.path().join("runtime.json");
        create_test_runtime_config(&runtime_config_path);

        let mut builder = PackageFileBuilder::new();

        let metadata = PackageMetadata::new(
            create_test_package_config(),
            FsType::Erofs,
            make_verity("test_hash", 0),
        )
        .unwrap();
        let runtime_config = PackageRuntimeConfig::from_file(&runtime_config_path).unwrap();
        let seccomp_policy = create_test_seccomp_policy();
        let payloads = Payloads::init();

        builder.set_metadata(metadata);
        builder.set_runtime_config(runtime_config);
        builder.set_seccomp_policy(seccomp_policy);
        builder.set_payloads(payloads);

        let result = builder.build();
        assert!(result.is_ok());

        let package_file = result.unwrap();
        assert_eq!(package_file.metadata().package.name, "test_package");
    }

    #[test]
    fn test_payload_size_type() {
        // Test that PayloadSizeType is properly defined as u64
        let size: PayloadSizeType = 1024;
        assert_eq!(size, 1024u64);
        assert_eq!(std::mem::size_of::<PayloadSizeType>(), 8);
    }

    #[test]
    fn test_sign_verify() {
        let temp_dir = tempdir().unwrap();
        let output_package_file = temp_dir.path().join("test_package.ssam");
        let runtime_config_path = temp_dir.path().join("runtime.json");
        let private1_key_path = temp_dir.path().join("private_key1.pem");
        let public1_key_path = temp_dir.path().join("public_key1.pem");
        let private2_key_path = temp_dir.path().join("private_key2.pem");
        let public2_key_path = temp_dir.path().join("public_key2.pem");
        create_test_keys(&private1_key_path, &public1_key_path);
        create_test_keys(&private2_key_path, &public2_key_path);

        let metadata = PackageMetadata::new(
            create_test_package_config(),
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap();

        create_test_runtime_config(&runtime_config_path);

        let pkgfs = temp_dir.path().join("pkgfs.img");
        std::fs::write(&pkgfs, b"pkgfs_data").unwrap();
        let runtime_config = PackageRuntimeConfig::from_file(&runtime_config_path).unwrap();
        let seccomp_policy = create_test_seccomp_policy();
        let payloads = Payloads::init().set_mut(
            PayloadType::PackageFilesystem,
            Some(Payload::External(pkgfs)),
        );

        let mut package_file = PackageFileBuilder::new();
        package_file.set_metadata(metadata.clone());
        package_file.set_runtime_config(runtime_config.clone());
        package_file.set_seccomp_policy(seccomp_policy.clone());
        package_file.set_payloads(payloads.clone());

        let package_file = package_file.build().unwrap();
        let result = package_file.wrap(
            temp_dir.path().join("test_package.ssam"),
            &private1_key_path,
        );
        assert!(result.is_ok());

        let package_file =
            PackageFile::from_file_verified(&output_package_file, public1_key_path).unwrap();

        assert_eq!(package_file.metadata().package.name, "test_package");
        assert_eq!(package_file.runtime_config().version(), "1.0.0");
        check_test_package_config(package_file.metadata());

        let package_file = PackageFile::from_file_verified(&output_package_file, public2_key_path);
        assert!(package_file.is_err());
    }

    #[test]
    fn test_read_footer_field() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("test_field.bin");

        // Write test data: "prefix" + FORMAT_VERSION + MAGIC
        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(b"prefix_data").unwrap();
            write_footer(&mut file).unwrap();
        }

        // Read MAGIC (last 24 bytes)
        {
            let mut file = File::open(&test_file_path).unwrap();
            let magic =
                read_footer_field::<SSAM_PKG_MAGIC_LEN>(&mut file, SSAM_PKG_MAGIC_LEN).unwrap();
            assert_eq!(&magic, SSAM_PKG_MAGIC);
        }

        // Read FORMAT_VERSION (32 bytes from end, 8 bytes)
        {
            let mut file = File::open(&test_file_path).unwrap();
            let version =
                read_footer_field::<SSAM_PKG_FORMAT_VERSION_LEN>(&mut file, SSAM_PKG_FOOTER_LEN)
                    .unwrap();
            assert_eq!(&version, SSAM_PKG_FORMAT_VERSION);
        }
    }

    #[test]
    fn test_verify_footer_valid() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("valid_footer.bin");

        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(b"some_package_data").unwrap();
            write_footer(&mut file).unwrap();
        }

        let mut file = File::open(&test_file_path).unwrap();
        assert!(verify_footer(&mut file).is_ok());
    }

    #[test]
    fn test_verify_footer_invalid_magic() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("invalid_magic.bin");

        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(b"some_package_data").unwrap();
            file.write_all(SSAM_PKG_FORMAT_VERSION).unwrap();
            file.write_all(b"INVALIDMAGICSTRING123456").unwrap();
        }

        let mut file = File::open(&test_file_path).unwrap();
        let result = verify_footer(&mut file);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageParseError::InvalidMagic),
            "Expected InvalidMagic error, got: {err:?}"
        );
    }

    #[test]
    fn test_verify_footer_invalid_version() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("invalid_version.bin");

        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(b"some_package_data").unwrap();
            file.write_all(b"9.9.9\0\0\0").unwrap();
            file.write_all(SSAM_PKG_MAGIC).unwrap();
        }

        let mut file = File::open(&test_file_path).unwrap();
        let result = verify_footer(&mut file);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageParseError::InvalidFormatVersion { .. }),
            "Expected InvalidFormatVersion error, got: {err:?}"
        );
    }

    #[test]
    fn test_verify_footer_file_too_small() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("small_file.bin");

        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(b"small").unwrap();
        }

        let mut file = File::open(&test_file_path).unwrap();
        let result = verify_footer(&mut file);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageParseError::Io { .. }),
            "Expected Io error, got: {err:?}"
        );
    }

    #[test]
    fn test_footer_write_read_roundtrip() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("test_footer.bin");

        // Write footer to file
        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(b"some_package_data").unwrap();
            write_footer(&mut file).unwrap();
        }

        // Verify footer
        {
            let mut file = File::open(&test_file_path).unwrap();
            assert!(verify_footer(&mut file).is_ok());
        }
    }

    #[test]
    fn test_footer_size_constant() {
        let expected = SSAM_PKG_MAGIC.len() + SSAM_PKG_FORMAT_VERSION.len();
        assert_eq!(SSAM_PKG_FOOTER_LEN, expected);
        assert_eq!(SSAM_PKG_FOOTER_LEN, 32);
        assert_eq!(SSAM_PKG_MAGIC.len(), 24);
        assert_eq!(SSAM_PKG_FORMAT_VERSION.len(), 8);
    }

    #[test]
    fn rejects_unknown_package_format_version() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("unknown_version.bin");

        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(b"some_package_data").unwrap();
            file.write_all(b"0.2.0\0\0\0").unwrap();
            file.write_all(SSAM_PKG_MAGIC).unwrap();
        }

        let mut file = File::open(&test_file_path).unwrap();
        let result = verify_footer(&mut file);
        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                PackageParseError::InvalidFormatVersion { .. }
            ),
            "Expected InvalidFormatVersion error for unknown version 0.2.0"
        );
    }

    #[test]
    fn load_payloads_info_rejects_oversized_payloads_size() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("oversized.ssam");
        let public_key_path = temp_dir.path().join("pub.pem");

        let private_key_path = temp_dir.path().join("priv.pem");
        create_test_keys(&private_key_path, &public_key_path);

        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(b"some_padding_data").unwrap();

            let oversized: u64 = 0xFFFF_FFFF_FFFF_0000;
            let encoded_size =
                bincode::encode_to_vec(oversized, SSAM_SERIALIZATION_CONFIG).unwrap();
            file.write_all(&encoded_size).unwrap();

            write_footer(&mut file).unwrap();
        }

        let mut file = File::open(&test_file_path).unwrap();
        let result = PackageFile::load_payloads_info(&mut file, &public_key_path);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageParseError::ParseFailed { .. }),
            "Expected ParseFailed for oversized payloads_size, got: {err:?}"
        );
    }

    #[test]
    fn load_payloads_info_rejects_payloads_size_slightly_over_file() {
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("slightly_over.ssam");
        let public_key_path = temp_dir.path().join("pub.pem");

        let private_key_path = temp_dir.path().join("priv.pem");
        create_test_keys(&private_key_path, &public_key_path);

        let padding = b"padding_data_for_test";
        {
            let mut file = File::create(&test_file_path).unwrap();
            file.write_all(padding).unwrap();

            let footer_len = SSAM_PKG_FOOTER_LEN as u64;
            let size_field_len = size_of::<PayloadSizeType>() as u64;
            let total_file_size = padding.len() as u64 + size_field_len + footer_len;
            let max_valid = total_file_size.saturating_sub(footer_len + size_field_len);
            let just_over = max_valid + 1;

            let encoded_size =
                bincode::encode_to_vec(just_over, SSAM_SERIALIZATION_CONFIG).unwrap();
            file.write_all(&encoded_size).unwrap();

            write_footer(&mut file).unwrap();
        }

        let mut file = File::open(&test_file_path).unwrap();
        let result = PackageFile::load_payloads_info(&mut file, &public_key_path);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageParseError::ParseFailed { .. }),
            "Expected ParseFailed for payloads_size slightly over file capacity, got: {err:?}"
        );
    }

    #[test]
    fn package_filesystem_new_exposes_supplied_values_via_accessors() {
        let verity = make_verity("root_hash_xyz", 4096);
        let fs = PackageFilesystem::new(1024, 8192, FsType::Ext4, verity);

        let extent = fs
            .pkgfs_extent()
            .expect("Internal payload must yield extent");
        assert_eq!(extent.offset, 1024);
        assert_eq!(extent.length, 8192);
        assert_eq!(fs.pkgfs_type(), FsType::Ext4);
        assert_eq!(fs.verity_info().root_hash, "root_hash_xyz");
        assert_eq!(fs.verity_info().hash_offset, 4096);
    }

    #[test]
    fn package_filesystem_external_payload_has_no_extent() {
        let fs = PackageFilesystem::from_source(
            Path::new("/nonexistent/pkgfs.img"),
            FsType::Erofs,
            make_verity("h", 0),
        );

        assert_eq!(fs.pkgfs_extent(), None);
        assert_eq!(fs.pkgfs_type(), FsType::Erofs);
        assert_eq!(fs.verity_info().root_hash, "h");
    }
}
