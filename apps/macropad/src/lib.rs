// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::any::Any;

slint::include_modules!();

pub struct MacropadAppFactory {
    info: theme::AppInfo,
}

impl MacropadAppFactory {
    pub fn new() -> Self {
        let app = MacropadApp::new().expect("Failed to create MacropadApp");
        let info = app.global::<MacropadInfo>().get_info();
        Self { info }
    }
}

impl Default for MacropadAppFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl theme::AppFactory for MacropadAppFactory {
    fn info(&self) -> theme::AppInfo {
        self.info.clone()
    }

    fn launch(&self, on_exit: Box<dyn Fn() + 'static>) -> Box<dyn Any> {
        let app = MacropadApp::new().expect("Failed to create MacropadApp");
        app.on_exit(move || on_exit());
        let _ = app.show();
        Box::new(app)
    }
}

