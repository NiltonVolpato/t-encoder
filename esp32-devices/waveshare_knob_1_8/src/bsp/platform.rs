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
    Framebuffer, BigEndianRgb565, RENDER_HEIGHT, RENDER_STRIDE, RENDER_WIDTH,
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
        Some(1.0), // Native 1:1 scaling for crisp, unaliased rendering
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
    // Single framebuffer in fast internal SRAM (259,200 bytes = 253.125 KiB)
    #[repr(align(16))]
    struct FrameBuffer([BigEndianRgb565; RENDER_STRIDE * RENDER_HEIGHT as usize]);

    static FRAME_BUFFER: static_cell::ConstStaticCell<FrameBuffer> =
        static_cell::ConstStaticCell::new(FrameBuffer(
            [BigEndianRgb565(0); RENDER_STRIDE * RENDER_HEIGHT as usize],
        ));

    let fb = Framebuffer(&mut FRAME_BUFFER.take().0);
    FLUSH_RETURN_CHANNEL
        .try_send(fb)
        .expect("FLUSH_RETURN_CHANNEL full while seeding framebuffer");

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
                        let x0 = origin.x.max(0) as u16;
                        let y0 = origin.y.max(0) as u16;
                        if x0 >= RENDER_WIDTH || y0 >= RENDER_HEIGHT {
                            continue;
                        }
                        // SH8601 QSPI requires even start coordinate and even width (2-pixel alignment)
                        let x = (x0 / 2) * 2;
                        let y = (y0 / 2) * 2;
                        let right = ((x0 + size.width as u16 + 1) / 2) * 2;
                        let bottom = ((y0 + size.height as u16 + 1) / 2) * 2;
                        let width = right.min(RENDER_WIDTH).saturating_sub(x);
                        let height = bottom.min(RENDER_HEIGHT).saturating_sub(y);

                        if width > 0 && height > 0 {
                            let _ = rects.push(DirtyRect { x, y, width, height });
                        }
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
