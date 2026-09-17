// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::any::Any;

slint::include_modules!();

pub struct PomodoroAppFactory {
    info: theme::AppInfo,
}

impl PomodoroAppFactory {
    pub fn new() -> Self {
        let app = PomodoroApp::new().expect("Failed to create PomodoroApp");
        let info = app.global::<PomodoroInfo>().get_info();
        Self { info }
    }
}

impl Default for PomodoroAppFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl theme::AppFactory for PomodoroAppFactory {
    fn info(&self) -> theme::AppInfo {
        self.info.clone()
    }

    fn launch(&self, on_exit: Box<dyn Fn() + 'static>) -> Box<dyn Any> {
        let app = PomodoroApp::new().expect("Failed to create PomodoroApp");
        app.on_exit(move || on_exit());
        let _ = app.show();
        Box::new(app)
    }
}

