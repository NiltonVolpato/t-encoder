// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Slint Platform implementation and event loop for Waveshare ESP32-S3-Knob-Touch-LCD-1.8.

extern crate alloc;
use alloc::rc::Rc;

pub use common::{EspPlatform, WindowHolder};
use common::{FeedbackSink, run_event_loop as run_common_event_loop};
use slint::PhysicalSize;
use slint::platform::software_renderer::MinimalSoftwareWindow;

use super::display::{
    DISPLAY_COMMAND_CHANNEL, DirtyRect, DisplayCommand, FLUSH_RETURN_CHANNEL, FlushJob,
    Framebuffer, NativeRgb565, RENDER_HEIGHT, RENDER_STRIDE, RENDER_WIDTH,
};
use common::event::ScreenEvent;
use super::haptics;

pub struct WaveshareFeedback;
impl FeedbackSink for WaveshareFeedback {
    fn on_dial_step_forward(&self) {
        haptics::signal_feedback(haptics::Feedback::DialStepForward);
    }
    fn on_dial_step_backward(&self) {
        haptics::signal_feedback(haptics::Feedback::DialStepBackward);
    }
    fn on_click(&self) {
        haptics::signal_feedback(haptics::Feedback::Click);
    }
    fn on_long_press(&self) {
        haptics::signal_feedback(haptics::Feedback::Haptic);
    }
}

pub fn create_platform() -> (EspPlatform, WindowHolder) {
    EspPlatform::with_config(
        Some(PhysicalSize::new(RENDER_WIDTH as u32, RENDER_HEIGHT as u32)),
        Some(1.0), // 1:1 Native scaling
    )
}

#[inline(always)]
fn cycle_count() -> u32 {
    #[cfg(target_arch = "xtensa")]
    {
        xtensa_lx::timer::get_cycle_count()
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        0
    }
}

/// Runs the main Slint event loop: awaits interrupt-driven inputs, advances
/// animations, and renders updates.
pub async fn run_event_loop(window_holder: WindowHolder) -> ! {
    // Double framebuffers in PSRAM: 2 * (360 * 360 * 2) = 2 * 259,200 bytes = ~506.25 KiB
    let fb_a_mem = alloc::vec![NativeRgb565::new(0); RENDER_STRIDE * RENDER_HEIGHT as usize].leak();
    let fb_b_mem = alloc::vec![NativeRgb565::new(0); RENDER_STRIDE * RENDER_HEIGHT as usize].leak();

    let fb_a = Framebuffer(fb_a_mem);
    let fb_b = Framebuffer(fb_b_mem);
    FLUSH_RETURN_CHANNEL
        .try_send(fb_a)
        .expect("FLUSH_RETURN_CHANNEL full while seeding framebuffer A");
    FLUSH_RETURN_CHANNEL
        .try_send(fb_b)
        .expect("FLUSH_RETURN_CHANNEL full while seeding framebuffer B");

    let base_brightness: u8 = 255;

    run_common_event_loop(
        window_holder,
        &WaveshareFeedback,
        base_brightness,
        async |screen_event| match screen_event {
            ScreenEvent::DimRelative(ratio) => {
                let level = ((base_brightness as f32 * ratio + 0.5) as u32).clamp(1, 255) as u8;
                defmt::info!("Screen dimming to relative brightness {}", level);
                DISPLAY_COMMAND_CHANNEL.send(DisplayCommand::SetBrightness(level)).await;
            }
            ScreenEvent::DimAbsolute(level) => {
                defmt::info!("Screen dimming to absolute brightness {}", level);
                DISPLAY_COMMAND_CHANNEL.send(DisplayCommand::SetBrightness(level)).await;
            }
            ScreenEvent::TurnOff => {
                defmt::info!("Turning off display panel");
                DISPLAY_COMMAND_CHANNEL.send(DisplayCommand::DisplayOff).await;
            }
            ScreenEvent::TurnOn => {
                defmt::info!("Turning on display panel");
                DISPLAY_COMMAND_CHANNEL.send(DisplayCommand::DisplayOn).await;
                DISPLAY_COMMAND_CHANNEL
                    .send(DisplayCommand::SetBrightness(base_brightness))
                    .await;
            }
        },
        async |window: Rc<MinimalSoftwareWindow>| {
            window
                .draw_async_if_needed(async |renderer| {
                    let fb = FLUSH_RETURN_CHANNEL.receive().await;

                    let mut rects: heapless::Vec<DirtyRect, 4> = heapless::Vec::new();
                    let r_start = cycle_count();
                    let region = renderer.render(&mut fb.0[..], RENDER_STRIDE);
                    let render_cycles = cycle_count().wrapping_sub(r_start);

                    for (origin, size) in region.iter_box() {
                        let x = origin.x.max(0) as u16;
                        let y = origin.y.max(0) as u16;
                        let width = size.width as u16;
                        let height = size.height as u16;

                        let _ = rects.push(DirtyRect { x, y, width, height });
                    }

                    DISPLAY_COMMAND_CHANNEL
                        .send(DisplayCommand::Flush(FlushJob { fb, rects, render_cycles }))
                        .await;
                })
                .await
        },
    )
    .await
}
