use std::env;
use std::time::Duration;

use espielberg::{DeviceEvent, Director};

#[test]
fn launcher_rotation_and_feedback_events() {
    let port = env::var("FLASH_PORT").unwrap_or_else(|_| "/dev/cu.usbmodem101".to_string());
    let mut director = Director::action(&port).expect("connect to device");
    director.reset().expect("reset device to initial state");

    // Cue a clockwise rotation by 1 card
    director.rotate(1).expect("send rotate cue");

    // Wait for the ViewChanged event emitted by the launcher
    let mut saw_view_changed = false;
    let mut saw_buzzer = false;
    let start = std::time::Instant::now();

    while start.elapsed() < Duration::from_secs(3) && (!saw_view_changed || !saw_buzzer) {
        if let Ok(event) = director.wait_for_event(Duration::from_millis(500)) {
            match event {
                DeviceEvent::ViewChanged { screen, card, .. } => {
                    assert_eq!(screen, "launcher");
                    assert_eq!(card, Some(1));
                    saw_view_changed = true;
                }
                DeviceEvent::Buzzer { freq_hz, .. } => {
                    assert!(freq_hz > 0);
                    saw_buzzer = true;
                }
                _ => {}
            }
        }
    }

    assert!(
        saw_view_changed,
        "did not receive ViewChanged event after rotation"
    );
    assert!(saw_buzzer, "did not receive Buzzer event after rotation");

    director.cut().expect("disconnect cleanly");
}
