// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::router::Router;

pub struct SsamLogger {
    router: Router,
}

impl SsamLogger {
    pub fn with_router(router: Router) -> Self {
        Self { router }
    }
}

impl log::Log for SsamLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        self.router.route(record);
    }

    fn flush(&self) {
        self.router.flush_all();
    }
}
