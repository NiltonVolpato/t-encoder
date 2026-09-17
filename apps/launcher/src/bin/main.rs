// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

use app_launcher::LauncherManager;

fn main() -> Result<(), slint::PlatformError> {
    let _manager = LauncherManager::new();
    slint::run_event_loop_until_quit()
}
