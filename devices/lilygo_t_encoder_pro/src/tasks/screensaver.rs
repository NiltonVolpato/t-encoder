// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

use embassy_futures::select::{Either, select};
use embassy_sync::watch::DynReceiver;
use embassy_time::{Duration, Timer};

use super::subscribe_user_activity;
use crate::bsp::event::{Event, ScreenEvent, send_event};

#[derive(Clone, Copy, Debug)]
pub struct InactivityStage {
    pub timeout: Duration,
    pub action: ScreenEvent,
}

const STAGES: [InactivityStage; 3] = [
    InactivityStage {
        timeout: Duration::from_secs(60), // Dim screen to 50% in 1 minute
        action: ScreenEvent::DimRelative(0.50),
    },
    InactivityStage {
        timeout: Duration::from_secs(15), // Dim screen to 25% in 1:15 minute
        action: ScreenEvent::DimRelative(0.25),
    },
    InactivityStage {
        timeout: Duration::from_secs(15), // Turn off screen in 1:30 minute
        action: ScreenEvent::TurnOff,
    },
];

enum TimeoutState {
    Reset,
    TimedOut,
}

async fn wait_for_activity(
    activity: &mut DynReceiver<'static, ()>,
    optional_timeout: Option<&Duration>,
) -> TimeoutState {
    let Some(timeout) = optional_timeout else {
        activity.get().await;
        return TimeoutState::Reset;
    };
    match select(activity.get(), Timer::after(*timeout)).await {
        Either::First(_) => TimeoutState::Reset,
        Either::Second(_) => TimeoutState::TimedOut,
    }
}

#[embassy_executor::task]
pub async fn screensaver_task() {
    let mut activity = subscribe_user_activity().expect("Failed to subscribe to user activity");
    let mut stages = STAGES.iter();
    loop {
        let current_stage = stages.next();
        match wait_for_activity(&mut activity, current_stage.map(|s| &s.timeout)).await {
            // User interacted; reset all inactivity timers.
            TimeoutState::Reset => {
                stages = STAGES.iter();
            }
            // Timer expired without interaction; report and wait for next timeout.
            TimeoutState::TimedOut => {
                if let Some(stage) = current_stage {
                    send_event(Event::Screen(stage.action));
                }
            }
        }
    }
}
