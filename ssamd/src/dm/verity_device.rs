// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use libssam::config::SSAM_SERIALIZATION_CONFIG;
use libssam::ssam_package::PackageFsVerityInfo;
use std::{
    fmt::Write,
    fs::File,
    io::{Seek, SeekFrom},
    path::{Path, PathBuf},
};

use super::dm_control::{DMControl, DMControlBackend, DMDevice, DMTargetInfo};

#[repr(C, packed)]
#[derive(Debug, bincode::Decode)]
struct VeritySuperBlock {
    _signature: [u8; 8],
    version: u32,
    #[allow(dead_code)]
    hash_type: u32,
    #[allow(dead_code)]
    uuid: [u8; 16],
    algorithm: [u8; 32],
    data_block_size: u32,
    hash_block_size: u32,
    data_blocks: u64,
    salt_size: u16,
    _pad1: [u8; 6],
    salt: [u8; 256],
    _pad2: [u8; 168],
}

impl VeritySuperBlock {
    fn get_algorithm(&self) -> anyhow::Result<String> {
        Ok(String::from_utf8(
            self.algorithm.into_iter().take_while(|c| *c != 0).collect(),
        )?)
    }

    fn get_salt(&self) -> anyhow::Result<String> {
        let salt_size = usize::from(self.salt_size);
        anyhow::ensure!(
            salt_size <= self.salt.len(),
            "Invalid dm-verity superblock: salt_size ({salt_size}) exceeds salt buffer length ({})",
            self.salt.len()
        );
        let salt_slice: &[u8] = &self.salt[..salt_size];
        let salt_hex = salt_slice.iter().fold(String::new(), |mut acc, byte| {
            write!(acc, "{byte:02x}").unwrap();
            acc
        });
        Ok(salt_hex)
    }
}

#[derive(Debug)]
pub(crate) struct VerityDevice {
    dm_dev: DMDevice,
    dm_control: DMControl,
}

impl VerityDevice {
    pub(crate) fn new(
        name: &str,
        dev_path: &Path,
        verity_info: &PackageFsVerityInfo,
    ) -> anyhow::Result<Self> {
        let dm_control = DMControl::new()?;
        let target = Self::make_target(dev_path, verity_info)?;
        let dm_dev = Self::setup(&dm_control, name, &[target])?;

        Ok(Self { dm_dev, dm_control })
    }

    fn setup(
        dm_control: &(impl DMControlBackend + ?Sized),
        device_name: &str,
        targets: &[DMTargetInfo],
    ) -> anyhow::Result<DMDevice> {
        // EBUSY: create_device failed, no device to clean up
        let device = match dm_control.create_device(device_name.to_string()) {
            Ok(dev) => dev,
            Err(e) => {
                match e
                    .chain()
                    .find_map(|c| c.downcast_ref::<rustix::io::Errno>())
                {
                    Some(&error_code) if error_code == rustix::io::Errno::BUSY => {
                        let dev_list = dm_control.list_devices()?;
                        for dev in dev_list {
                            if dev == device_name {
                                return Err(anyhow::anyhow!("Device {device_name} already exists"));
                            }
                        }
                        return Err(anyhow::anyhow!(
                            "Device {device_name} is busy, but does not exist in the device list"
                        ));
                    }
                    _ => (),
                }
                return Err(anyhow::anyhow!("Failed to create device: {e}"));
            }
        };

        // Device created — clean up on any subsequent failure
        let result = dm_control
            .load_table(Some(device_name), None, targets)
            .context("Failed to load table")
            .and_then(|()| {
                dm_control
                    .resume_device(Some(device_name), None)
                    .context("Failed to resume device")
            });

        if let Err(e) = result {
            if let Err(cleanup_err) = dm_control.remove_device(Some(device_name), None) {
                log::warn!("Failed to remove device {device_name} during cleanup: {cleanup_err:?}");
            }
            return Err(e);
        }

        Ok(device)
    }

    fn read_super_block(
        hash_file: &mut File,
        hash_offset: u64,
    ) -> anyhow::Result<VeritySuperBlock> {
        hash_file.seek(SeekFrom::Start(hash_offset))?;
        Ok(bincode::decode_from_std_read(
            hash_file,
            SSAM_SERIALIZATION_CONFIG,
        )?)
    }

    fn make_target(
        dev_path: &Path,
        verity_info: &PackageFsVerityInfo,
    ) -> anyhow::Result<DMTargetInfo> {
        let data_size = verity_info.data_size;
        let hash_offset = verity_info.hash_offset;
        let root_hash = &verity_info.root_hash;

        #[cfg(not(test))]
        let sector_count = {
            let data_file = File::open(dev_path)?;
            data_size / u64::from(rustix::fs::ioctl_blksszget(&data_file)?)
        };

        #[cfg(test)]
        let sector_count = {
            // In test mode, assume 512 byte sectors
            data_size / 512
        };

        let mut hash_file = File::open(dev_path)?;
        let sb = Self::read_super_block(&mut hash_file, hash_offset)?;
        // should copy it as VeritySuperBlock is a packed struct.
        let version = sb.version;
        let data_block_size = sb.data_block_size;
        let hash_block_size = sb.hash_block_size;
        let data_blocks = sb.data_blocks;
        let algorithm = sb.get_algorithm()?;
        let salt = sb.get_salt()?;

        if data_block_size == 0 || hash_block_size == 0 {
            anyhow::bail!(
                "Invalid dm-verity superblock: block sizes must be non-zero \
                 (data_block_size={data_block_size}, hash_block_size={hash_block_size})"
            );
        }

        let hash_start_block = data_size / u64::from(data_block_size) + 1u64;

        let dev_path_str = dev_path.to_string_lossy();
        let table_params = format!(
            "{version} {dev_path_str} {dev_path_str} {data_block_size} {hash_block_size} {data_blocks} {hash_start_block} {algorithm} {root_hash} {salt}"
        );

        Ok(DMTargetInfo::new(
            0,
            sector_count,
            "verity".to_string(),
            table_params,
        ))
    }

    pub(crate) fn name(&self) -> String {
        self.dm_dev.device_name()
    }

    pub(crate) fn devnode(&self) -> PathBuf {
        ["/dev", &format!("dm-{}", self.dm_dev.dev_no().minor())]
            .iter()
            .collect()
    }
}

impl Drop for VerityDevice {
    fn drop(&mut self) {
        if let Err(e) = self.dm_control.remove_device(Some(&self.name()), None) {
            log::warn!("Failed to request remove dm-verity device with error: {e:?}");
        } else {
            log::debug!(
                "Succeeded to request remove dm-verity device: {:?}",
                self.name()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as FmtWrite;
    use std::io::{Seek, SeekFrom, Write};
    use tempfile::{NamedTempFile, TempDir};

    /// Helper function to create a test hash file with `VeritySuperBlock`
    fn create_test_hash_file(
        temp_dir: &TempDir,
        algorithm: &str,
        salt_data: &[u8],
    ) -> anyhow::Result<NamedTempFile> {
        let mut hash_file = NamedTempFile::new_in(temp_dir)?;
        let mut verity_sb = VeritySuperBlock {
            _signature: *b"verity\0\0",
            version: 1,
            hash_type: 1,
            uuid: [0; 16],
            algorithm: [0; 32],
            data_block_size: 4096,
            hash_block_size: 4096,
            data_blocks: 100,
            salt_size: 16,
            _pad1: [0; 6],
            salt: [0; 256],
            _pad2: [0; 168],
        };

        // Set algorithm
        let algo_bytes = algorithm.as_bytes();
        verity_sb.algorithm[..algo_bytes.len()].copy_from_slice(algo_bytes);

        // Set salt
        verity_sb.salt_size =
            u16::try_from(salt_data.len()).context("salt_data length exceeds u16 range")?;
        verity_sb.salt[..salt_data.len()].copy_from_slice(salt_data);

        // Write superblock to hash file
        hash_file.seek(SeekFrom::Start(0))?;
        let sb_bytes: [u8; 512] = unsafe { std::mem::transmute(verity_sb) };
        hash_file.write_all(&sb_bytes)?;
        hash_file.flush()?;

        Ok(hash_file)
    }

    /// Tests for `VeritySuperBlock` struct
    // Salt sizes in tests are small (\u2264 256 bytes), so truncation cannot occur; intentional cast.
    #[allow(clippy::cast_possible_truncation)]
    mod verity_super_block_tests {
        use super::*;

        #[test]
        fn test_get_algorithm() -> anyhow::Result<()> {
            let mut verity_sb = VeritySuperBlock {
                _signature: *b"verity\0\0",
                version: 1,
                hash_type: 1,
                uuid: [0; 16],
                algorithm: [0; 32],
                data_block_size: 4096,
                hash_block_size: 4096,
                data_blocks: 100,
                salt_size: 16,
                _pad1: [0; 6],
                salt: [0; 256],
                _pad2: [0; 168],
            };

            // Test SHA256
            let algo = b"sha256";
            verity_sb.algorithm[..algo.len()].copy_from_slice(algo);
            assert_eq!(verity_sb.get_algorithm()?, "sha256");

            // Test SHA512
            verity_sb.algorithm.fill(0); // Clear previous algorithm
            let algo = b"sha512";
            verity_sb.algorithm[..algo.len()].copy_from_slice(algo);
            assert_eq!(verity_sb.get_algorithm()?, "sha512");

            // Test SHA1
            verity_sb.algorithm.fill(0);
            let algo = b"sha1";
            verity_sb.algorithm[..algo.len()].copy_from_slice(algo);
            assert_eq!(verity_sb.get_algorithm()?, "sha1");

            Ok(())
        }

        #[test]
        fn test_get_algorithm_invalid_utf8() {
            let verity_sb = VeritySuperBlock {
                _signature: *b"verity\0\0",
                version: 1,
                hash_type: 1,
                uuid: [0; 16],
                algorithm: [0xFF; 32], // Invalid UTF-8
                data_block_size: 4096,
                hash_block_size: 4096,
                data_blocks: 100,
                salt_size: 16,
                _pad1: [0; 6],
                salt: [0; 256],
                _pad2: [0; 168],
            };
            let result = verity_sb.get_algorithm();
            assert!(result.is_err());
        }

        #[test]
        fn test_get_salt() -> anyhow::Result<()> {
            let mut verity_sb = VeritySuperBlock {
                _signature: *b"verity\0\0",
                version: 1,
                hash_type: 1,
                uuid: [0; 16],
                algorithm: [0; 32],
                data_block_size: 4096,
                hash_block_size: 4096,
                data_blocks: 100,
                salt_size: 8, // 8 bytes salt
                _pad1: [0; 6],
                salt: [0; 256],
                _pad2: [0; 168],
            };

            // Test with simple pattern
            for i in 0..8 {
                verity_sb.salt[i] = i as u8;
            }
            assert_eq!(verity_sb.get_salt()?, "0001020304050607");

            // Test with different salt size
            verity_sb.salt_size = 4;
            assert_eq!(verity_sb.get_salt()?, "00010203");

            // Test with hex pattern
            verity_sb.salt_size = 16;
            let hex_bytes = [
                0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,
                0xde, 0xf0,
            ];
            verity_sb.salt[..16].copy_from_slice(&hex_bytes);
            assert_eq!(verity_sb.get_salt()?, "deadbeefcafebabe123456789abcdef0");

            verity_sb.salt_size = 0;
            assert_eq!(verity_sb.get_salt()?, "");

            verity_sb.salt_size = 256;
            verity_sb.salt = [0xAB; 256];
            assert_eq!(verity_sb.get_salt()?, "ab".repeat(256));

            Ok(())
        }

        #[test]
        fn test_creation_and_parsing() -> anyhow::Result<()> {
            let temp_dir = TempDir::new()?;

            // Test different configurations
            let test_cases = vec![
                ("sha1", &b"test_salt_16byte"[..]),
                ("sha256", &b"another_salt_for_testing_256long"[..]),
                ("sha512", &b"short_s_"[..]),
            ];

            for (algorithm, salt_data) in test_cases {
                let mut hash_file = create_test_hash_file(&temp_dir, algorithm, salt_data)?;

                // Read back and verify
                let read_sb = super::VerityDevice::read_super_block(hash_file.as_file_mut(), 0)?;

                // Copy values to avoid packed struct issues
                let version = read_sb.version;
                let data_blocks = read_sb.data_blocks;
                let salt_size_read = read_sb.salt_size;

                assert_eq!(version, 1);
                assert_eq!(data_blocks, 100); // Updated from default helper function
                assert_eq!(salt_size_read, salt_data.len() as u16);
                assert_eq!(read_sb.get_algorithm()?, algorithm);

                // Verify salt matches
                let expected_salt_hex =
                    salt_data
                        .iter()
                        .take(salt_data.len())
                        .fold(String::new(), |mut acc, byte| {
                            FmtWrite::write_fmt(&mut acc, format_args!("{byte:02x}")).unwrap();
                            acc
                        });
                assert_eq!(read_sb.get_salt()?, expected_salt_hex);
            }

            Ok(())
        }

        #[test]
        fn test_read_super_block() -> anyhow::Result<()> {
            let temp_dir = TempDir::new()?;

            let test_salt = "deadbeefcafebabe123456789abcdef0";
            let expected_hex = "6465616462656566636166656261626531323334353637383961626364656630";
            let salt_bytes = test_salt.as_bytes();

            let mut hash_file = create_test_hash_file(&temp_dir, "sha512", salt_bytes)?;

            let read_sb = super::VerityDevice::read_super_block(hash_file.as_file_mut(), 0)?;
            let version = read_sb.version;
            let data_blocks = read_sb.data_blocks;
            assert_eq!(version, 1);
            assert_eq!(data_blocks, 100); // Updated from default helper function  
            assert_eq!(read_sb.get_algorithm()?, "sha512");
            assert_eq!(read_sb.get_salt()?, expected_hex);

            Ok(())
        }

        #[test]
        fn test_read_super_block_decode_failure() -> anyhow::Result<()> {
            let temp_dir = TempDir::new()?;
            let mut empty_file = NamedTempFile::new_in(&temp_dir)?;
            let result = super::VerityDevice::read_super_block(empty_file.as_file_mut(), 0);
            assert!(result.is_err(), "Should fail to decode from empty file");
            Ok(())
        }

        #[test]
        fn test_get_salt_rejects_oversized_salt_size() {
            let verity_sb = VeritySuperBlock {
                _signature: *b"verity\0\0",
                version: 1,
                hash_type: 1,
                uuid: [0; 16],
                algorithm: [0; 32],
                data_block_size: 4096,
                hash_block_size: 4096,
                data_blocks: 100,
                salt_size: 257,
                _pad1: [0; 6],
                salt: [0; 256],
                _pad2: [0; 168],
            };
            let result = verity_sb.get_salt();
            assert!(result.is_err(), "salt_size > 256 should return an error");
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("257"),
                "Error message should contain the invalid salt_size value: {err_msg}"
            );
            assert!(
                err_msg.contains("256"),
                "Error message should contain the buffer length: {err_msg}"
            );
        }
    }

    mod setup_tests {
        use super::super::super::dm_control::DeviceNum;
        use super::*;
        use std::cell::Cell;

        enum CreateDeviceResult {
            Ok,
            Busy,
        }

        struct MockDMControl {
            create_device_result: CreateDeviceResult,
            load_table_fail: bool,
            resume_device_fail: bool,
            list_devices_result: Result<Vec<String>, &'static str>,
            remove_device_called: Cell<bool>,
        }

        impl MockDMControl {
            fn new(load_table_fail: bool, resume_device_fail: bool) -> Self {
                Self {
                    create_device_result: CreateDeviceResult::Ok,
                    load_table_fail,
                    resume_device_fail,
                    list_devices_result: Ok(vec![]),
                    remove_device_called: Cell::new(false),
                }
            }

            fn with_busy(list_devices_result: Result<Vec<String>, &'static str>) -> Self {
                Self {
                    create_device_result: CreateDeviceResult::Busy,
                    load_table_fail: false,
                    resume_device_fail: false,
                    list_devices_result,
                    remove_device_called: Cell::new(false),
                }
            }
        }

        impl DMControlBackend for MockDMControl {
            fn create_device(&self, device_name: String) -> anyhow::Result<DMDevice> {
                match self.create_device_result {
                    CreateDeviceResult::Ok => {
                        Ok(DMDevice::new(DeviceNum::new(253, 0), device_name))
                    }
                    CreateDeviceResult::Busy => Err(anyhow::Error::from(rustix::io::Errno::BUSY)
                        .context("Failed to perform ioctl (DevCreate)")),
                }
            }

            fn load_table(
                &self,
                _device_name: Option<&str>,
                _device_uuid: Option<&str>,
                _targets: &[DMTargetInfo],
            ) -> anyhow::Result<()> {
                if self.load_table_fail {
                    anyhow::bail!("simulated load_table failure")
                }
                Ok(())
            }

            fn resume_device(
                &self,
                _device_name: Option<&str>,
                _device_uuid: Option<&str>,
            ) -> anyhow::Result<()> {
                if self.resume_device_fail {
                    anyhow::bail!("simulated resume_device failure")
                }
                Ok(())
            }

            fn remove_device(
                &self,
                _device_name: Option<&str>,
                _device_uuid: Option<&str>,
            ) -> anyhow::Result<()> {
                self.remove_device_called.set(true);
                Ok(())
            }

            fn list_devices(&self) -> anyhow::Result<Vec<String>> {
                match &self.list_devices_result {
                    Ok(v) => Ok(v.clone()),
                    Err(msg) => anyhow::bail!("{msg}"),
                }
            }
        }

        #[test]
        fn test_setup_success() {
            let mock = MockDMControl::new(false, false);
            let targets = vec![DMTargetInfo::new(
                0,
                100,
                "verity".to_string(),
                "params".to_string(),
            )];

            let result = VerityDevice::setup(&mock, "test-dev", &targets);
            assert!(result.is_ok());
            assert!(!mock.remove_device_called.get());
        }

        #[test]
        fn test_setup_calls_remove_device_on_load_table_failure() {
            let mock = MockDMControl::new(true, false);
            let targets = vec![DMTargetInfo::new(
                0,
                100,
                "verity".to_string(),
                "params".to_string(),
            )];

            let result = VerityDevice::setup(&mock, "test-dev", &targets);
            assert!(result.is_err());
            assert!(
                mock.remove_device_called.get(),
                "remove_device must be called when load_table fails"
            );
        }

        #[test]
        fn test_setup_calls_remove_device_on_resume_failure() {
            let mock = MockDMControl::new(false, true);
            let targets = vec![DMTargetInfo::new(
                0,
                100,
                "verity".to_string(),
                "params".to_string(),
            )];

            let result = VerityDevice::setup(&mock, "test-dev", &targets);
            assert!(result.is_err());
            assert!(
                mock.remove_device_called.get(),
                "remove_device must be called when resume_device fails"
            );
        }

        #[test]
        fn test_setup_ebusy_device_already_exists() {
            let mock =
                MockDMControl::with_busy(Ok(vec!["other-dev".to_string(), "test-dev".to_string()]));
            let targets = vec![DMTargetInfo::new(
                0,
                100,
                "verity".to_string(),
                "params".to_string(),
            )];

            let result = VerityDevice::setup(&mock, "test-dev", &targets);
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("already exists"),
                "Expected 'already exists' error, got: {err_msg}"
            );
        }

        #[test]
        fn test_setup_ebusy_device_not_in_list() {
            let mock = MockDMControl::with_busy(Ok(vec!["other-dev".to_string()]));
            let targets = vec![DMTargetInfo::new(
                0,
                100,
                "verity".to_string(),
                "params".to_string(),
            )];

            let result = VerityDevice::setup(&mock, "test-dev", &targets);
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("busy, but does not exist"),
                "Expected 'busy, but does not exist' error, got: {err_msg}"
            );
        }

        #[test]
        fn test_setup_ebusy_list_devices_fails() {
            let mock = MockDMControl::with_busy(Err("simulated list_devices failure"));
            let targets = vec![DMTargetInfo::new(
                0,
                100,
                "verity".to_string(),
                "params".to_string(),
            )];

            let result = VerityDevice::setup(&mock, "test-dev", &targets);
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("list_devices failure"),
                "Expected list_devices error to propagate, got: {err_msg}"
            );
        }
    }
}
