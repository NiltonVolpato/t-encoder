// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::any::Any;

slint::include_modules!();

#[derive(Clone)]
pub struct SonosAppFactory {
    info: app_shell::AppInfo,
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

impl app_shell::AppFactory for SonosAppFactory {
    fn info(&self) -> app_shell::AppInfo {
        self.info.clone()
    }

    fn launch(&self, context: app_shell::ShellContext) -> Box<dyn Any> {
        let app = SonosApp::new().expect("Failed to create SonosApp");
        app.on_exit(move || context.exit());
        let _ = app.show();
        Box::new(app)
    }

    fn clone_box(&self) -> Box<dyn app_shell::AppFactory> {
        Box::new(self.clone())
    }
}

