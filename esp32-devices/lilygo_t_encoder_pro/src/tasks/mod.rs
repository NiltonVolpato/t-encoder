// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

pub use common::channels::{report_user_activity, subscribe_user_activity};
pub use common::tasks::profiler::{
    PROFILER_ENABLED, ProfileScope, ProfilerConfig, ProfilerTrigger, TARGET_SCOPE, profiler_task,
    scope,
};
pub use common::tasks::screensaver::screensaver_task;
