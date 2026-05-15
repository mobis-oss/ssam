// Copyright (c) 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use simplelog::{Config, SimpleLogger};

pub struct SimpleLogBackend {
    inner: Box<SimpleLogger>,
}

impl SimpleLogBackend {
    /// Create a backend that forwards to `simplelog::SimpleLogger`.
    ///
    /// # Examples
    ///
    /// ```
    /// use log::Log;
    /// use ssam_log::backend::simplelog::SimpleLogBackend;
    ///
    /// let backend = SimpleLogBackend::new(log::LevelFilter::Info);
    /// assert!(backend.enabled(&log::Metadata::builder().level(log::Level::Info).build()));
    /// ```
    #[must_use]
    pub fn new(level: log::LevelFilter) -> Self {
        Self {
            inner: SimpleLogger::new(level, Config::default()),
        }
    }
}

impl Default for SimpleLogBackend {
    fn default() -> Self {
        Self::new(log::LevelFilter::max())
    }
}

impl log::Log for SimpleLogBackend {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        self.inner.log(record);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}
