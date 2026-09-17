// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::any::Any;

slint::include_modules!();

pub struct SonosAppFactory {
    info: theme::AppInfo,
}

impl SonosAppFactory {
    pub fn new() -> Self {
        let app = SonosApp::new().expect("Failed to create SonosApp");
        let info = app.global::<SonosInfo>().get_info();
        Self { info }
    }
}

impl Default for SonosAppFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl theme::AppFactory for SonosAppFactory {
    fn info(&self) -> theme::AppInfo {
        self.info.clone()
    }

    fn launch(&self, on_exit: Box<dyn Fn() + 'static>) -> Box<dyn Any> {
        let app = SonosApp::new().expect("Failed to create SonosApp");
        app.on_exit(move || on_exit());
        let _ = app.show();
        Box::new(app)
    }
}

