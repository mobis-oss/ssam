// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context, Result, anyhow, bail};
use rsa::{
    Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey,
    pkcs8::DecodePrivateKey,
    pkcs8::DecodePublicKey,
    sha2::{Digest, Sha256},
    traits::PublicKeyParts,
};
use std::fmt::Write as _;

use strum::IntoEnumIterator;

use super::PackageParseError;
use crate::config::SSAM_SERIALIZATION_CONFIG;

const STREAMING_CHUNK_SIZE: usize = 64 * 1024;

fn seek_to_payload(
    reader: &mut (impl Read + Seek),
    offset: PayloadOffset,
    size: usize,
    file_size: u64,
) -> Result<(), PackageParseError> {
    if offset.saturating_add(size as u64) > file_size {
        return Err(PackageParseError::ParseFailed {
            source: anyhow!(
                "Payload region (offset={offset}, size={size}) exceeds file size {file_size}"
            ),
        });
    }
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|e| PackageParseError::Io {
            message: "Failed to seek package file.".to_string(),
            source: e.into(),
        })?;
    Ok(())
}

fn read_payload(
    reader: &mut (impl Read + Seek),
    offset: PayloadOffset,
    size: usize,
    file_size: u64,
    max_allowed_size: usize,
) -> Result<Vec<u8>, PackageParseError> {
    if size > max_allowed_size {
        return Err(PackageParseError::ParseFailed {
            source: anyhow!("Payload size {size} exceeds maximum allowed size {max_allowed_size}"),
        });
    }
    seek_to_payload(reader, offset, size, file_size)?;
    let mut buf = vec![0; size];
    reader
        .read_exact(&mut buf)
        .map_err(|e| PackageParseError::Io {
            message: "Failed to read package file.".to_string(),
            source: e.into(),
        })?;
    Ok(buf)
}

fn update_hasher(
    reader: &mut (impl Read + Seek),
    offset: PayloadOffset,
    size: usize,
    file_size: u64,
    hasher: &mut Sha256,
    buf: &mut [u8],
) -> Result<(), PackageParseError> {
    let chunk_size = buf.len();
    if chunk_size == 0 {
        return Err(PackageParseError::ParseFailed {
            source: anyhow!("Streaming buffer must not be empty"),
        });
    }
    seek_to_payload(reader, offset, size, file_size)?;
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(chunk_size);
        reader
            .read_exact(&mut buf[..n])
            .map_err(|e| PackageParseError::Io {
                message: "Failed to read package file.".to_string(),
                source: e.into(),
            })?;
        hasher.update(&buf[..n]);
        remaining -= n;
    }
    Ok(())
}

#[derive(
    Debug,
    Clone,
    Copy,
    Ord,
    Eq,
    Hash,
    PartialOrd,
    PartialEq,
    strum::EnumIter,
    strum::Display,
    bincode::Encode,
    bincode::Decode,
)]
pub(crate) enum PayloadType {
    Metadata,
    RuntimeConfig,
    SeccompPolicy,
    Signature,
    PackageFilesystem,
}

type PayloadOffset = u64;
type PayloadSize = u64;

#[derive(Debug, Clone, PartialEq, bincode::Encode, bincode::Decode)]
pub enum Payload {
    Internal((PayloadOffset, PayloadSize)),
    External(PathBuf),
    Data(Vec<u8>),
}

#[derive(Debug, Clone, bincode::Encode, bincode::Decode)]
pub(crate) struct Payloads(BTreeMap<PayloadType, Option<Payload>>);

pub(crate) struct PayloadConfig {
    need_verify: bool,
    filename: String,
}

pub(crate) static SSAM_PAYLOAD_CONFIGURATION: LazyLock<HashMap<PayloadType, PayloadConfig>> =
    LazyLock::new(|| {
        HashMap::from_iter([
            (
                PayloadType::Metadata,
                PayloadConfig::new(true, "metadata.json"),
            ),
            (
                PayloadType::RuntimeConfig,
                PayloadConfig::new(true, "runtime_config.json"),
            ),
            (
                PayloadType::SeccompPolicy,
                PayloadConfig::new(true, "seccomp.json"),
            ),
            (
                PayloadType::Signature,
                PayloadConfig::new(false, "signature.sig"),
            ),
            (
                PayloadType::PackageFilesystem,
                PayloadConfig::new(false, "filesystem.img"),
            ),
        ])
    });

impl Payloads {
    pub(crate) fn init() -> Self {
        let mut map = BTreeMap::new();
        PayloadType::iter().for_each(|t| {
            map.insert(t, None);
        });
        Payloads(map)
    }

    pub(crate) fn set(&mut self, payload_type: PayloadType, payload: Option<Payload>) {
        self.0.insert(payload_type, payload);
    }

    pub(crate) fn set_mut(mut self, payload_type: PayloadType, payload: Option<Payload>) -> Self {
        self.set(payload_type, payload);
        self
    }

    pub(crate) fn get(&self, payload_type: PayloadType) -> Option<&Payload> {
        self.0.get(&payload_type).and_then(|v| v.as_ref())
    }

    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        bincode::encode_to_vec(self, SSAM_SERIALIZATION_CONFIG)
            .context("Failed to serialize Payloads")
    }

    pub(crate) fn deserialize_from_pkg_verified(
        pkg_fp: &mut File,
        public_key: impl AsRef<std::path::Path>,
    ) -> Result<Self, PackageParseError> {
        let deserialized: Self = bincode::decode_from_std_read(pkg_fp, SSAM_SERIALIZATION_CONFIG)
            .map_err(|e| PackageParseError::ParseFailed {
            source: anyhow!(e).context("Failed to decode payload info"),
        })?;
        Self::verify(pkg_fp, &deserialized, &public_key)?;
        Ok(deserialized)
    }

    pub(crate) fn sign(&mut self, private_key: impl AsRef<Path>) -> Result<()> {
        let mut hasher = Sha256::new();
        for (payload_type, payload) in self.iter() {
            if SSAM_PAYLOAD_CONFIGURATION
                .get(payload_type)
                .ok_or_else(|| {
                    anyhow!(
                        "Failed to sign: Unable to find payload configuration for {payload_type}"
                    )
                })?
                .need_verify()
            {
                if let Some(Payload::Data(data)) = payload {
                    hasher.update(data);
                } else {
                    bail!(
                        "Failed to sign: Unsupported payload type: {payload:?} for {payload_type}"
                    );
                }
            }
        }
        let digest = hasher.finalize();
        let signature = RsaPrivateKey::read_pkcs8_pem_file(&private_key)
            .with_context(|| {
                format!(
                    "Failed to sign: Error reading private key file {}",
                    private_key.as_ref().display()
                )
            })?
            .sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
            .context("Failed to sign package")?;
        self.set(PayloadType::Signature, Some(Payload::Data(signature)));
        Ok(())
    }

    fn verify(
        pkg_fp: &mut File,
        ssam_payloads: &Self,
        public_key: impl AsRef<Path>,
    ) -> Result<(), PackageParseError> {
        let file_size = pkg_fp
            .metadata()
            .context("Querying file metadata for payload size validation")
            .map_err(|source| PackageParseError::Io {
                message: "Failed to get package file metadata.".to_string(),
                source,
            })?
            .len();

        let public_key = RsaPublicKey::read_public_key_pem_file(&public_key).map_err(|e| {
            PackageParseError::ParseFailed {
                source: anyhow!(e).context("Failed to read public key"),
            }
        })?;

        let max_signature_size = public_key.size();
        let signature;
        if let Some(Payload::Internal((offset, size))) = ssam_payloads.get(PayloadType::Signature) {
            let size = usize::try_from(*size)
                .with_context(|| format!("Signature payload size {size} exceeds addressable range"))
                .map_err(|source| PackageParseError::Io {
                    message: "Signature payload size too large".to_string(),
                    source,
                })?;
            signature = read_payload(pkg_fp, *offset, size, file_size, max_signature_size)?;
        } else {
            return Err(PackageParseError::ParseFailed {
                source: anyhow!("Missing signature payload"),
            });
        }

        let mut hasher = Sha256::new();
        let mut hash_buf = vec![0u8; STREAMING_CHUNK_SIZE];
        for (payload_type, payload) in ssam_payloads {
            let payload_conf = SSAM_PAYLOAD_CONFIGURATION
                .get(payload_type)
                .ok_or_else(|| PackageParseError::ParseFailed {
                    source: anyhow!("Unknown payload type: {payload_type}"),
                })?;
            if !payload_conf.need_verify() {
                continue;
            }
            let payload = payload
                .as_ref()
                .ok_or_else(|| PackageParseError::ParseFailed {
                    source: anyhow!("Missing payload for {payload_type}"),
                })?;
            if let Payload::Internal((offset, size)) = payload {
                let size = usize::try_from(*size)
                    .with_context(|| {
                        format!("Payload size {size} for {payload_type} exceeds addressable range")
                    })
                    .map_err(|source| PackageParseError::Io {
                        message: format!("Payload size too large for {payload_type}"),
                        source,
                    })?;
                update_hasher(pkg_fp, *offset, size, file_size, &mut hasher, &mut hash_buf)?;
            } else {
                return Err(PackageParseError::ParseFailed {
                    source: anyhow!("Expected internal payload for {payload_type}"),
                });
            }
        }

        let digest = hasher.finalize().to_vec();
        log::debug!("Verifying package (digest: {})", {
            digest.iter().fold(String::new(), |mut acc, b| {
                let _ = write!(acc, "{b:02x}");
                acc
            })
        });

        public_key
            .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &signature)
            .map_err(|_| PackageParseError::SignatureVerificationFailed)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&PayloadType, &Option<Payload>)> {
        self.into_iter()
    }
}

impl IntoIterator for Payloads {
    type Item = (PayloadType, Option<Payload>);
    type IntoIter = std::collections::btree_map::IntoIter<PayloadType, Option<Payload>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Payloads {
    type Item = (&'a PayloadType, &'a Option<Payload>);
    type IntoIter = std::collections::btree_map::Iter<'a, PayloadType, Option<Payload>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl PayloadConfig {
    fn new(need_verification: bool, filename: &str) -> Self {
        PayloadConfig {
            need_verify: need_verification,
            filename: filename.to_string(),
        }
    }

    pub(crate) fn need_verify(&self) -> bool {
        self.need_verify
    }

    pub(crate) fn filename(&self) -> &str {
        &self.filename
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn test_ssam_payloads_set_all_types() {
        let mut ssam_payloads = Payloads::init();
        let data_sets = [
            (
                PayloadType::Metadata,
                Payload::Data(b"metadata content".to_vec()),
            ),
            (
                PayloadType::RuntimeConfig,
                Payload::External(PathBuf::from("/path/to/runtime.json")),
            ),
            (
                PayloadType::SeccompPolicy,
                Payload::Data(b"seccomp policy content".to_vec()),
            ),
            (PayloadType::Signature, Payload::Internal((1024, 256))),
            (
                PayloadType::PackageFilesystem,
                Payload::External(PathBuf::from("/path/to/filesystem.img")),
            ),
        ];

        for (payload_type, payload) in &data_sets {
            ssam_payloads = ssam_payloads.set_mut(*payload_type, Some(payload.clone()));
        }

        for (payload_type, payload) in &data_sets {
            let v = ssam_payloads.get(*payload_type);
            assert!(v.is_some(), "Payload type {payload_type:?} should be set");
            assert_eq!(
                v.unwrap(),
                payload,
                "Payload for {payload_type:?} should match"
            );
        }

        PayloadType::iter().for_each(|payload_type| {
            assert!(
                ssam_payloads.get(payload_type).is_some(),
                "Payload type {payload_type} should be set"
            );
        });
    }

    #[test]
    fn test_ssam_payloads_set_partial() {
        let b = BTreeMap::from_iter([
            (
                PayloadType::Metadata,
                Payload::Data(b"partial metadata".to_vec()),
            ),
            (
                PayloadType::PackageFilesystem,
                Payload::External(PathBuf::from("/path/to/fs.img")),
            ),
        ]);
        let mut ssam_payloads = Payloads::init();

        for (set_t, set_v) in &b {
            ssam_payloads = ssam_payloads.set_mut(*set_t, Some(set_v.clone()));
        }

        ssam_payloads.iter().for_each(|(payload_type, payload)| {
            if b.contains_key(payload_type) {
                assert!(
                    payload.is_some(),
                    "Payload type {payload_type:?} should be set"
                );
                assert_eq!(
                    payload.as_ref().unwrap(),
                    b.get(payload_type).unwrap(),
                    "Payload for {payload_type:?} should match"
                );
            } else {
                assert!(
                    payload.is_none(),
                    "Payload type {payload_type:?} should not be set"
                );
            }
        });
    }

    #[test]
    fn test_ssam_payloads_iterator() {
        let payloads = Payloads::init()
            .set_mut(
                PayloadType::Metadata,
                Some(Payload::Data(b"test metadata".to_vec())),
            )
            .set_mut(
                PayloadType::RuntimeConfig,
                Some(Payload::External(PathBuf::from("/config.json"))),
            )
            .set_mut(PayloadType::Signature, Some(Payload::Internal((0, 128))));

        for payload_type in PayloadType::iter() {
            match payload_type {
                PayloadType::Metadata => {
                    let payload = payloads.get(payload_type).unwrap();
                    assert_eq!(payload, &Payload::Data(b"test metadata".to_vec()));
                }
                PayloadType::RuntimeConfig => {
                    let payload = payloads.get(payload_type).unwrap();
                    assert_eq!(payload, &Payload::External(PathBuf::from("/config.json")));
                }
                PayloadType::Signature => {
                    let payload = payloads.get(payload_type).unwrap();
                    assert_eq!(payload, &Payload::Internal((0, 128)));
                }
                _ => assert!(
                    payloads.get(payload_type).is_none(),
                    "Payload type {payload_type} should not be set"
                ),
            }
        }
    }

    #[test]
    fn test_ssam_payloads_ordered_iteration() {
        let payloads = Payloads::init()
            .set_mut(
                PayloadType::PackageFilesystem,
                Some(Payload::Data(b"filesystem".to_vec())),
            )
            .set_mut(
                PayloadType::Metadata,
                Some(Payload::Data(b"metadata".to_vec())),
            )
            .set_mut(
                PayloadType::Signature,
                Some(Payload::Data(b"signature".to_vec())),
            );

        let expected_order: Vec<PayloadType> = PayloadType::iter().collect();
        let actual_order: Vec<_> = payloads
            .iter()
            .map(|(payload_type, _)| *payload_type)
            .collect();

        assert_eq!(actual_order, expected_order);
    }

    #[test]
    fn test_payload_types() {
        let internal = Payload::Internal((0, 100));
        if let Payload::Internal((offset, size)) = internal {
            assert_eq!(offset, 0);
            assert_eq!(size, 100);
        } else {
            panic!("Expected INTERNAL variant");
        }

        let external = Payload::External(PathBuf::from("/test/path"));
        if let Payload::External(path) = external {
            assert_eq!(path, PathBuf::from("/test/path"));
        } else {
            panic!("Expected EXTERNAL variant");
        }

        let data = Payload::Data(b"test data".to_vec());
        if let Payload::Data(content) = data {
            assert_eq!(content, b"test data".to_vec());
        } else {
            panic!("Expected DATA variant");
        }
    }

    #[test]
    fn test_ssam_payload_type_ordering() {
        let mut types = vec![
            PayloadType::Metadata,
            PayloadType::RuntimeConfig,
            PayloadType::SeccompPolicy,
            PayloadType::PackageFilesystem,
            PayloadType::Signature,
        ];

        types.sort();

        let expected = vec![
            PayloadType::Metadata,
            PayloadType::RuntimeConfig,
            PayloadType::SeccompPolicy,
            PayloadType::Signature,
            PayloadType::PackageFilesystem,
        ];

        assert_eq!(types, expected);
    }

    #[test]
    fn seek_to_payload_rejects_region_past_eof() {
        let mut cursor = Cursor::new([0u8; 10]);
        let result = seek_to_payload(&mut cursor, 5, 10, 10);
        assert!(result.is_err());
    }

    #[test]
    fn seek_to_payload_rejects_overflow() {
        let mut cursor = Cursor::new([0u8; 100]);
        let result = seek_to_payload(&mut cursor, u64::MAX, 1, 100);
        assert!(result.is_err());
    }

    #[test]
    fn seek_to_payload_accepts_exact_boundary() {
        let mut cursor = Cursor::new([0u8; 10]);
        let result = seek_to_payload(&mut cursor, 0, 10, 10);
        assert!(result.is_ok());
    }

    #[test]
    fn seek_to_payload_accepts_nonzero_offset() {
        let mut cursor = Cursor::new([0u8; 20]);
        let result = seek_to_payload(&mut cursor, 10, 10, 20);
        assert!(result.is_ok());
    }

    #[test]
    fn read_payload_rejects_size_exceeding_max() {
        let mut cursor = Cursor::new(b"small file content".to_vec());
        let file_size = 18;

        let result = read_payload(&mut cursor, 0, 8192, file_size, 1024);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageParseError::ParseFailed { .. }),
            "Expected ParseFailed, got: {err:?}"
        );
    }

    #[test]
    fn read_payload_succeeds_at_exact_boundary() {
        let data = b"exactly16bytes!!";
        let mut cursor = Cursor::new(data.to_vec());
        let file_size = data.len() as u64;

        let result = read_payload(&mut cursor, 0, data.len(), file_size, data.len());

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), data.to_vec());
    }

    #[test]
    fn read_payload_rejects_region_past_eof() {
        let mut cursor = Cursor::new(b"short".to_vec());

        let result = read_payload(&mut cursor, 0, 100, 5, 200);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageParseError::ParseFailed { .. }),
            "Expected ParseFailed, got: {err:?}"
        );
    }

    #[test]
    fn read_payload_reads_from_nonzero_offset() {
        let data = b"HEADERpayload_data_here";
        let mut cursor = Cursor::new(data.to_vec());
        let file_size = data.len() as u64;

        let result = read_payload(&mut cursor, 6, 17, file_size, 64);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"payload_data_here".to_vec());
    }

    #[test]
    fn update_hasher_produces_correct_hash() {
        let data = b"hello world streaming hash test data";
        let mut cursor = Cursor::new(data.to_vec());
        let file_size = data.len() as u64;

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAMING_CHUNK_SIZE];
        let result = update_hasher(&mut cursor, 0, data.len(), file_size, &mut hasher, &mut buf);
        assert!(result.is_ok());

        let mut expected_hasher = Sha256::new();
        expected_hasher.update(data);
        assert_eq!(hasher.finalize(), expected_hasher.finalize());
    }

    #[test]
    fn update_hasher_rejects_region_past_eof() {
        let mut cursor = Cursor::new(b"short".to_vec());

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAMING_CHUNK_SIZE];
        let result = update_hasher(&mut cursor, 0, 9999, 5, &mut hasher, &mut buf);

        assert!(result.is_err());
    }

    #[test]
    fn update_hasher_handles_multi_chunk_data() {
        let data = vec![0xABu8; STREAMING_CHUNK_SIZE * 3 + 1000];
        let mut cursor = Cursor::new(data.clone());
        let file_size = data.len() as u64;

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAMING_CHUNK_SIZE];
        let result = update_hasher(&mut cursor, 0, data.len(), file_size, &mut hasher, &mut buf);
        assert!(result.is_ok());

        let mut expected_hasher = Sha256::new();
        expected_hasher.update(&data);
        assert_eq!(hasher.finalize(), expected_hasher.finalize());
    }

    #[test]
    fn update_hasher_handles_exact_chunk_boundary() {
        let data = vec![0xCDu8; STREAMING_CHUNK_SIZE * 2];
        let mut cursor = Cursor::new(data.clone());
        let file_size = data.len() as u64;

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAMING_CHUNK_SIZE];
        let result = update_hasher(&mut cursor, 0, data.len(), file_size, &mut hasher, &mut buf);
        assert!(result.is_ok());

        let mut expected_hasher = Sha256::new();
        expected_hasher.update(&data);
        assert_eq!(hasher.finalize(), expected_hasher.finalize());
    }

    #[test]
    fn update_hasher_from_nonzero_offset() {
        let data = b"SKIPthis_is_hashed";
        let mut cursor = Cursor::new(data.to_vec());
        let file_size = data.len() as u64;

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAMING_CHUNK_SIZE];
        let result = update_hasher(&mut cursor, 4, 14, file_size, &mut hasher, &mut buf);
        assert!(result.is_ok());

        let mut expected_hasher = Sha256::new();
        expected_hasher.update(b"this_is_hashed");
        assert_eq!(hasher.finalize(), expected_hasher.finalize());
    }

    #[test]
    fn update_hasher_zero_size() {
        let mut cursor = Cursor::new(b"data".to_vec());

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAMING_CHUNK_SIZE];
        let result = update_hasher(&mut cursor, 0, 0, 4, &mut hasher, &mut buf);
        assert!(result.is_ok());

        let mut expected_hasher = Sha256::new();
        expected_hasher.update(b"");
        assert_eq!(hasher.finalize(), expected_hasher.finalize());
    }

    #[test]
    fn update_hasher_rejects_empty_buffer() {
        let mut cursor = Cursor::new(b"data".to_vec());

        let mut hasher = Sha256::new();
        let mut buf = vec![];
        let result = update_hasher(&mut cursor, 0, 4, 4, &mut hasher, &mut buf);

        assert!(result.is_err());
    }
}
