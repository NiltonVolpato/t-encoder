// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Unified system event model and central event queue for LilyGO T-Encoder Pro.

pub use common::channels::{EVENTS, send_event, send_input_event, send_screen_event};
pub use common::event::{Event, InputEvent, ScreenEvent, TouchEvent, TouchPoint};
