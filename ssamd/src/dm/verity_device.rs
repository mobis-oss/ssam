// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use libssam::ssam_package::PackageFsVerityInfo;
use std::path::{Path, PathBuf};

use super::dm_control::{DMControl, DMControlBackend, DMDevice, DMTargetInfo};

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

    fn make_target(
        dev_path: &Path,
        verity_info: &PackageFsVerityInfo,
    ) -> anyhow::Result<DMTargetInfo> {
        let data_size = verity_info.data_size;

        #[cfg(not(test))]
        let sector_count = {
            let data_file = std::fs::File::open(dev_path)?;
            data_size / u64::from(rustix::fs::ioctl_blksszget(&data_file)?)
        };

        #[cfg(test)]
        let sector_count = {
            // In test mode, assume 512 byte sectors
            data_size / 512
        };

        let dev_path_str = dev_path.to_string_lossy();
        let table_params = verity_info.resolve_table(&dev_path_str)?;

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
