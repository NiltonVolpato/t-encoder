// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

extern crate alloc;

use alloc::boxed::Box;

use app_launcher::LauncherAppFactory;
use app_shell::{AppShell, DefaultAppShellPlatform};

fn main() -> Result<(), slint::PlatformError> {
    let launcher = LauncherAppFactory::default_apps();
    let shell = AppShell::new(Box::new(DefaultAppShellPlatform), Box::new(launcher));
    AppShell::start(&shell);
    slint::run_event_loop_until_quit()
}
