// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

use app_simon::SimonApp;
use slint::ComponentHandle;

fn main() -> Result<(), slint::PlatformError> {
    let app = SimonApp::new()?;
    app.on_exit({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                let _ = app.hide();
            }
        }
    });
    app.run()
}
