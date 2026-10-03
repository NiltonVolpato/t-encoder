// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

use app_sonos::{SonosApp, setup_sonos};
use slint::ComponentHandle;

fn main() -> Result<(), slint::PlatformError> {
    let app = SonosApp::new()?;
    let _handles = setup_sonos(&app);
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
