// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

use app_clock::{ClockApp, Time, setup_clock};
use slint::ComponentHandle;

fn main() -> Result<(), slint::PlatformError> {
    let app = ClockApp::new()?;
    let mut time = Time::new(10, 42, 30);
    setup_clock(&app, time);

    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        core::time::Duration::from_secs(1),
        move || {
            if let Some(app) = weak.upgrade() {
                time.tick();
                app.set_hours(time.hours as i32);
                app.set_minutes(time.minutes as i32);
                app.set_seconds(time.seconds as i32);
            }
        },
    );

    app.run()
}
