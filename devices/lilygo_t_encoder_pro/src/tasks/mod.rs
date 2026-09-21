// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

pub mod profiler;
pub mod screensaver;

pub use profiler::{
    PROFILER_ENABLED, ProfileScope, ProfilerConfig, ProfilerTrigger, TARGET_SCOPE, profiler_task,
    scope,
};
pub use screensaver::screensaver_task;
pub use user_activity::{report_user_activity, subscribe_user_activity};

mod user_activity {
    //! Shared user activity notification channel for background tasks.
    //! Keep this hidden here since it's not a "task".

    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::watch::{DynReceiver, Watch};

    const MAX_ACTIVITY_SUBSCRIBERS: usize = 4;

    type UserActivityWatch = Watch<CriticalSectionRawMutex, (), MAX_ACTIVITY_SUBSCRIBERS>;

    /// Broadcast watch channel notifying subscribers of user interactions.
    static USER_ACTIVITY_WATCH: UserActivityWatch = Watch::new();

    /// Notifies all background task subscribers of user input interaction.
    pub fn report_user_activity() {
        USER_ACTIVITY_WATCH.sender().send(());
    }

    /// Helper to subscribe to user input activity notifications.
    pub fn subscribe_user_activity() -> Option<DynReceiver<'static, ()>> {
        USER_ACTIVITY_WATCH.dyn_receiver()
    }
}
