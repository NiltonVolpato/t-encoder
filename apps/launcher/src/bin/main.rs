// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

use app_launcher::ClockApp;
use slint::ComponentHandle;

fn main() -> Result<(), slint::PlatformError> {
    let app = ClockApp::new()?;
    app.run()
}
