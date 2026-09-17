// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::any::Any;

slint::include_modules!();

pub struct SimonAppFactory {
    info: theme::AppInfo,
}

impl SimonAppFactory {
    pub fn new() -> Self {
        let app = SimonApp::new().expect("Failed to create SimonApp");
        let info = app.global::<SimonInfo>().get_info();
        Self { info }
    }
}

impl Default for SimonAppFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl theme::AppFactory for SimonAppFactory {
    fn info(&self) -> theme::AppInfo {
        self.info.clone()
    }

    fn launch(&self, on_exit: Box<dyn Fn() + 'static>) -> Box<dyn Any> {
        let app = SimonApp::new().expect("Failed to create SimonApp");
        app.on_exit(move || on_exit());
        let _ = app.show();
        Box::new(app)
    }
}

