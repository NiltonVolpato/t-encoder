// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::any::Any;

slint::include_modules!();

pub struct Magic8AppFactory {
    info: theme::AppInfo,
}

impl Magic8AppFactory {
    pub fn new() -> Self {
        let app = Magic8App::new().expect("Failed to create Magic8App");
        let info = app.global::<Magic8Info>().get_info();
        Self { info }
    }
}

impl Default for Magic8AppFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl theme::AppFactory for Magic8AppFactory {
    fn info(&self) -> theme::AppInfo {
        self.info.clone()
    }

    fn launch(&self, on_exit: Box<dyn Fn() + 'static>) -> Box<dyn Any> {
        let app = Magic8App::new().expect("Failed to create Magic8App");
        app.on_exit(move || on_exit());
        let _ = app.show();
        Box::new(app)
    }
}

