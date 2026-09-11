//! Sonos Controller App for `LilyGo` T-Encoder-Pro.
//!
//! Two-level navigation hierarchy:
//! - Level 1: Speaker Group Selection (`Mode::GroupSelector`)
//! - Level 2: Now Playing & Playback Controls (`Mode::NowPlaying`)

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::Write;
use launcher::{
    Action, App, AppFactory, Ctx, Feedback, IconId, InputEvent, Manifest, Outcome, SwipeDirection,
    TouchAccess, ViewId,
};
use ui::{Shell, SonosGroupCard, SonosState};

/// Commands sent from the UI to the background Sonos network worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SonosCommand {
    /// Toggle Play/Pause on the specified group coordinator.
    TogglePlayPause { group_idx: usize },
    /// Skip to next track.
    NextTrack { group_idx: usize },
    /// Skip to previous track.
    PreviousTrack { group_idx: usize },
    /// Adjust volume by relative delta (signed).
    AdjustVolume { group_idx: usize, delta: i32 },
    /// Set the active group for monitoring.
    SelectGroup { group_idx: usize },
}

/// Discovered speaker group summary for the Level 1 carousel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSummary {
    pub name: heapless::String<32>,
    pub members: heapless::String<64>,
    pub playing_summary: heapless::String<64>,
    pub is_playing: bool,
}

impl GroupSummary {
    /// Creates a new group summary card.
    #[must_use]
    pub fn new(name: &str, members: &str, playing_summary: &str, is_playing: bool) -> Self {
        let mut n = heapless::String::new();
        let _ = n.push_str(name);
        let mut m = heapless::String::new();
        let _ = m.push_str(members);
        let mut p = heapless::String::new();
        let _ = p.push_str(playing_summary);
        Self {
            name: n,
            members: m,
            playing_summary: p,
            is_playing,
        }
    }
}

/// Currently playing track metadata and transport info.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlayingData {
    pub track_title: heapless::String<64>,
    pub track_artist: heapless::String<64>,
    pub track_album: heapless::String<64>,
    pub elapsed_seconds: u32,
    pub duration_seconds: u32,
    pub volume: u8,
    pub is_playing: bool,
}

impl Default for NowPlayingData {
    fn default() -> Self {
        Self {
            track_title: heapless::String::new(),
            track_artist: heapless::String::new(),
            track_album: heapless::String::new(),
            elapsed_seconds: 0,
            duration_seconds: 0,
            volume: 20,
            is_playing: false,
        }
    }
}

/// Thread-safe snapshot shared from the background network worker to the UI.
#[derive(Debug, Clone, Default)]
pub struct SonosSnapshot {
    pub groups: Vec<GroupSummary>,
    pub active_now_playing: NowPlayingData,
    pub revision: u32,
}

/// Function pointer type providing current Sonos snapshot.
pub type SnapshotGetter = fn() -> SonosSnapshot;

/// Function pointer type sending a command to the Sonos worker.
pub type CommandSink = fn(SonosCommand);

fn default_snapshot_getter() -> SonosSnapshot {
    SonosSnapshot::default()
}

fn default_command_sink(_cmd: SonosCommand) {}

/// Builds [`Sonos`] app instances.
pub struct SonosFactory {
    manifest: Manifest,
    shell: slint::Weak<Shell>,
    get_snapshot: SnapshotGetter,
    send_command: CommandSink,
}

impl SonosFactory {
    /// Creates the factory with default mock bridge.
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> Self {
        Self::with_bridge(shell, default_snapshot_getter, default_command_sink)
    }

    /// Creates the factory with an injected bridge provider and command sink.
    #[must_use]
    pub fn with_bridge(
        shell: slint::Weak<Shell>,
        get_snapshot: SnapshotGetter,
        send_command: CommandSink,
    ) -> Self {
        Self {
            manifest: Manifest {
                name: "Sonos",
                icon: IconId(4),
                view: ViewId(5),
                accent: embedded_graphics::pixelcolor::Rgb565::new(31, 28, 0),
                touch: TouchAccess::AllGestures,
                requires_ble: false,
            },
            shell,
            get_snapshot,
            send_command,
        }
    }
}

impl AppFactory for SonosFactory {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn create(&self) -> Box<dyn App + '_> {
        Box::new(Sonos::new(
            self.shell.clone(),
            self.get_snapshot,
            self.send_command,
        ))
    }
}

/// Active mode of the Sonos controller application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Level 1: Speaker Group Selection carousel.
    GroupSelector,
    /// Level 2: Now Playing & Playback Controls for active group at index.
    NowPlaying(usize),
}

/// A running Sonos controller app instance.
pub struct Sonos {
    shell: slint::Weak<Shell>,
    mode: Mode,
    selected_group: usize,
    snapshot: SonosSnapshot,
    get_snapshot: SnapshotGetter,
    send_command: CommandSink,
    optimistic_volume: u8,
    optimistic_playing: bool,
    volume_toast_expires_at_ms: u64,
    last_revision: u32,
    marquee_start_ms: u64,
    last_title: heapless::String<64>,
    title_scroll_offset: f32,
    title_scroll_active: bool,
}

impl Sonos {
    /// Creates a new `Sonos` app instance.
    #[must_use]
    pub fn new(
        shell: slint::Weak<Shell>,
        get_snapshot: SnapshotGetter,
        send_command: CommandSink,
    ) -> Self {
        let snapshot = get_snapshot();
        let volume = snapshot.active_now_playing.volume;
        let is_playing = snapshot.active_now_playing.is_playing;
        let last_revision = snapshot.revision;
        Self {
            shell,
            mode: Mode::GroupSelector,
            selected_group: 0,
            snapshot,
            get_snapshot,
            send_command,
            optimistic_volume: volume,
            optimistic_playing: is_playing,
            volume_toast_expires_at_ms: 0,
            last_revision,
            marquee_start_ms: 0,
            last_title: heapless::String::new(),
            title_scroll_offset: 0.0,
            title_scroll_active: false,
        }
    }

    fn handle_group_selector(&mut self, event: InputEvent) -> Outcome {
        match event {
            InputEvent::Rotate(delta) => {
                let count = self.snapshot.groups.len();
                if count == 0 {
                    return Outcome::NONE;
                }
                let last = count.saturating_sub(1);
                let cur = i64::try_from(self.selected_group).unwrap_or(0);
                let next = cur
                    .saturating_add(i64::from(delta))
                    .clamp(0, i64::try_from(last).unwrap_or(0));
                let next_usize = usize::try_from(next).unwrap_or(0);
                if next_usize == self.selected_group {
                    return Outcome::NONE;
                }
                self.selected_group = next_usize;
                Outcome::CHANGED
            }
            InputEvent::Select | InputEvent::Tap { .. } => {
                if self.snapshot.groups.is_empty() {
                    return Outcome::NONE;
                }
                self.mode = Mode::NowPlaying(self.selected_group);
                self.optimistic_volume = self.snapshot.active_now_playing.volume;
                self.optimistic_playing = self.snapshot.active_now_playing.is_playing;
                self.volume_toast_expires_at_ms = 0;
                (self.send_command)(SonosCommand::SelectGroup {
                    group_idx: self.selected_group,
                });
                Outcome::buzz(Feedback::Haptic)
            }
            // Swiping up or left from group selection exits to the launcher carousel.
            InputEvent::Swipe(SwipeDirection::Up | SwipeDirection::Left) => Outcome {
                changed: false,
                action: Action::Exit,
                feedback: None,
                keys: None,
            },
            _ => Outcome::NONE,
        }
    }

    fn handle_now_playing(&mut self, event: InputEvent, group_idx: usize, ctx: &Ctx) -> Outcome {
        match event {
            InputEvent::Rotate(delta) => {
                let cur = i32::from(self.optimistic_volume);
                let next = (cur.saturating_add(delta)).clamp(0, 100);
                let new_volume = u8::try_from(next).unwrap_or(0);
                self.optimistic_volume = new_volume;
                self.volume_toast_expires_at_ms = ctx.now_ms.saturating_add(1500);
                (self.send_command)(SonosCommand::AdjustVolume { group_idx, delta });
                Outcome::CHANGED
            }
            InputEvent::Select | InputEvent::Tap { .. } => {
                self.optimistic_playing = !self.optimistic_playing;
                (self.send_command)(SonosCommand::TogglePlayPause { group_idx });
                Outcome::buzz(Feedback::Haptic)
            }
            InputEvent::Swipe(SwipeDirection::Left) => {
                // Swipe left: pulls next song in from right
                (self.send_command)(SonosCommand::NextTrack { group_idx });
                Outcome::buzz(Feedback::Beep)
            }
            InputEvent::Swipe(SwipeDirection::Right) => {
                // Swipe right: pulls previous song back
                (self.send_command)(SonosCommand::PreviousTrack { group_idx });
                Outcome::buzz(Feedback::Beep)
            }
            InputEvent::Swipe(SwipeDirection::Up) => {
                // Swipe up: return to Level 1 Group Selection
                self.mode = Mode::GroupSelector;
                Outcome::buzz(Feedback::Haptic)
            }
            _ => Outcome::NONE,
        }
    }
}

impl App for Sonos {
    fn handle(&mut self, event: InputEvent, ctx: &Ctx) -> Outcome {
        match self.mode {
            Mode::GroupSelector => self.handle_group_selector(event),
            Mode::NowPlaying(group_idx) => self.handle_now_playing(event, group_idx, ctx),
        }
    }

    fn tick(&mut self, ctx: &Ctx) -> Outcome {
        let mut changed = false;

        if self.volume_toast_expires_at_ms > 0 && ctx.now_ms >= self.volume_toast_expires_at_ms {
            self.volume_toast_expires_at_ms = 0;
            changed = true;
        }

        let fresh = (self.get_snapshot)();
        if fresh.revision != self.last_revision {
            if self.volume_toast_expires_at_ms == 0 {
                self.optimistic_volume = fresh.active_now_playing.volume;
            }
            self.optimistic_playing = fresh.active_now_playing.is_playing;
            self.snapshot = fresh;
            self.last_revision = self.snapshot.revision;
            if !self.snapshot.groups.is_empty() {
                self.selected_group = self
                    .selected_group
                    .min(self.snapshot.groups.len().saturating_sub(1));
            }
            changed = true;
        }

        // Track title marquee calculation
        let title = &self.snapshot.active_now_playing.track_title;
        if title.as_str() != self.last_title.as_str() {
            self.last_title = title.clone();
            self.marquee_start_ms = ctx.now_ms;
            self.title_scroll_offset = 0.0;
        }

        let char_count = title.chars().count();
        if char_count > 14 {
            self.title_scroll_active = true;
            let char_width_px = 15;
            let total_width = char_count.saturating_mul(char_width_px);
            let max_scroll = u32::try_from(total_width.saturating_sub(230)).unwrap_or(0);

            let scroll_ms = u64::from(max_scroll).saturating_mul(28);
            let cycle_ms = 2000 + scroll_ms + 2000 + 500;

            let elapsed = ctx.now_ms.saturating_sub(self.marquee_start_ms);
            let pos_in_cycle = elapsed % cycle_ms;

            let new_offset = if pos_in_cycle < 2000 {
                0.0
            } else if pos_in_cycle < 2000 + scroll_ms {
                let scroll_elapsed = pos_in_cycle - 2000;
                let fraction = f32::from(u16::try_from(scroll_elapsed).unwrap_or(u16::MAX))
                    / f32::from(u16::try_from(scroll_ms).unwrap_or(1).max(1));
                fraction * f32::from(u16::try_from(max_scroll).unwrap_or(0))
            } else if pos_in_cycle < 2000 + scroll_ms + 2000 {
                f32::from(u16::try_from(max_scroll).unwrap_or(0))
            } else {
                0.0
            };

            if (new_offset - self.title_scroll_offset).abs() > 0.5 {
                self.title_scroll_offset = new_offset;
                changed = true;
            }
        } else if self.title_scroll_active {
            self.title_scroll_active = false;
            self.title_scroll_offset = 0.0;
            changed = true;
        }

        if changed {
            Outcome::CHANGED
        } else {
            Outcome::NONE
        }
    }

    fn sync(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };

        let group_cards: Vec<SonosGroupCard> = self
            .snapshot
            .groups
            .iter()
            .map(|g| SonosGroupCard {
                name: g.name.as_str().into(),
                members: g.members.as_str().into(),
                playing_summary: g.playing_summary.as_str().into(),
                is_playing: g.is_playing,
            })
            .collect();

        let active_name = match self.mode {
            Mode::NowPlaying(idx) => self
                .snapshot
                .groups
                .get(idx)
                .map_or("", |g| g.name.as_str()),
            Mode::GroupSelector => self
                .snapshot
                .groups
                .get(self.selected_group)
                .map_or("", |g| g.name.as_str()),
        };

        let elapsed = self.snapshot.active_now_playing.elapsed_seconds;
        let duration = self.snapshot.active_now_playing.duration_seconds;
        let progress_ratio = if duration > 0 {
            f32::from(u16::try_from(elapsed).unwrap_or(u16::MAX))
                / f32::from(u16::try_from(duration).unwrap_or(u16::MAX).max(1))
        } else {
            0.0
        };

        let elapsed_str = format_time(elapsed);
        let duration_str = format_time(duration);

        let state = SonosState {
            mode: match self.mode {
                Mode::GroupSelector => 0,
                Mode::NowPlaying(_) => 1,
            },
            groups: slint::ModelRc::new(slint::VecModel::from(group_cards)),
            selected_group: i32::try_from(self.selected_group).unwrap_or(0),
            active_group_name: active_name.into(),
            track_title: self.snapshot.active_now_playing.track_title.as_str().into(),
            track_artist: self
                .snapshot
                .active_now_playing
                .track_artist
                .as_str()
                .into(),
            track_album: self.snapshot.active_now_playing.track_album.as_str().into(),
            elapsed_str: elapsed_str.as_str().into(),
            duration_str: duration_str.as_str().into(),
            progress_ratio,
            volume: i32::from(self.optimistic_volume),
            volume_visible: self.volume_toast_expires_at_ms > 0,
            is_playing: self.optimistic_playing,
            accent: slint::Color::from_rgb_u8(0xF8, 0x70, 0x00),
            status: slint::SharedString::new(),
            title_scroll_offset: self.title_scroll_offset,
            title_scroll_active: self.title_scroll_active,
        };

        shell.set_sonos(state);
    }
}

fn format_time(total_seconds: u32) -> heapless::String<16> {
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    let mut s = heapless::String::new();
    let _ = write!(s, "{minutes:02}:{seconds:02}");
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::sync::atomic::{AtomicU32, Ordering};

    static LAST_CMD_CODE: AtomicU32 = AtomicU32::new(0);
    static LAST_CMD_ARG: AtomicU32 = AtomicU32::new(0);
    static TEST_REVISION: AtomicU32 = AtomicU32::new(1);

    fn test_command_sink(cmd: SonosCommand) {
        match cmd {
            SonosCommand::SelectGroup { group_idx } => {
                LAST_CMD_CODE.store(1, Ordering::SeqCst);
                LAST_CMD_ARG.store(u32::try_from(group_idx).unwrap_or(0), Ordering::SeqCst);
            }
            SonosCommand::AdjustVolume {
                group_idx: _,
                delta,
            } => {
                LAST_CMD_CODE.store(2, Ordering::SeqCst);
                LAST_CMD_ARG.store(delta.cast_unsigned(), Ordering::SeqCst);
            }
            SonosCommand::TogglePlayPause { group_idx } => {
                LAST_CMD_CODE.store(3, Ordering::SeqCst);
                LAST_CMD_ARG.store(u32::try_from(group_idx).unwrap_or(0), Ordering::SeqCst);
            }
            SonosCommand::NextTrack { group_idx } => {
                LAST_CMD_CODE.store(4, Ordering::SeqCst);
                LAST_CMD_ARG.store(u32::try_from(group_idx).unwrap_or(0), Ordering::SeqCst);
            }
            SonosCommand::PreviousTrack { group_idx } => {
                LAST_CMD_CODE.store(5, Ordering::SeqCst);
                LAST_CMD_ARG.store(u32::try_from(group_idx).unwrap_or(0), Ordering::SeqCst);
            }
        }
    }

    fn test_snapshot_getter() -> SonosSnapshot {
        let groups = vec![
            GroupSummary::new("Kitchen", "Kitchen", "NEVER NEVER - Marc Moon", true),
            GroupSummary::new("Office", "Office", "Paused", false),
            GroupSummary::new("Living Room", "Living Room", "", false),
        ];

        let mut title = heapless::String::new();
        let _ = title.push_str("NEVER NEVER");
        let mut artist = heapless::String::new();
        let _ = artist.push_str("Marc Moon");
        let mut album = heapless::String::new();
        let _ = album.push_str("NEVER NEVER");

        let now_playing = NowPlayingData {
            track_title: title,
            track_artist: artist,
            track_album: album,
            elapsed_seconds: 45,
            duration_seconds: 180,
            volume: 25,
            is_playing: true,
        };

        SonosSnapshot {
            groups,
            active_now_playing: now_playing,
            revision: TEST_REVISION.load(Ordering::SeqCst),
        }
    }

    #[test]
    fn test_group_selector_rotation_and_selection() {
        LAST_CMD_CODE.store(0, Ordering::SeqCst);
        let mut app = Sonos::new(
            slint::Weak::default(),
            test_snapshot_getter,
            test_command_sink,
        );
        let ctx = Ctx {
            now_ms: 1000,
            ble_linked: false,
        };

        assert_eq!(app.mode, Mode::GroupSelector);
        assert_eq!(app.selected_group, 0);

        // Rotate clockwise
        let outcome = app.handle(InputEvent::Rotate(1), &ctx);
        assert!(outcome.changed);
        assert_eq!(app.selected_group, 1);

        // Rotate clockwise again
        app.handle(InputEvent::Rotate(1), &ctx);
        assert_eq!(app.selected_group, 2);

        // Clamp at last group
        let outcome = app.handle(InputEvent::Rotate(1), &ctx);
        assert!(!outcome.changed);
        assert_eq!(app.selected_group, 2);

        // Rotate counter-clockwise
        app.handle(InputEvent::Rotate(-1), &ctx);
        assert_eq!(app.selected_group, 1);

        // Select active group -> transitions to NowPlaying
        let outcome = app.handle(InputEvent::Select, &ctx);
        assert_eq!(app.mode, Mode::NowPlaying(1));
        assert_eq!(outcome.feedback, Some(Feedback::Haptic));

        assert_eq!(LAST_CMD_CODE.load(Ordering::SeqCst), 1);
        assert_eq!(LAST_CMD_ARG.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_group_selector_exit_gestures() {
        let mut app = Sonos::new(
            slint::Weak::default(),
            test_snapshot_getter,
            test_command_sink,
        );
        let ctx = Ctx {
            now_ms: 1000,
            ble_linked: false,
        };

        // Swipe up in group selection exits to launcher
        let outcome = app.handle(InputEvent::Swipe(SwipeDirection::Up), &ctx);
        assert_eq!(outcome.action, Action::Exit);

        // Swipe left in group selection also exits to launcher
        let outcome = app.handle(InputEvent::Swipe(SwipeDirection::Left), &ctx);
        assert_eq!(outcome.action, Action::Exit);

        // Swipe right stays in app
        let outcome = app.handle(InputEvent::Swipe(SwipeDirection::Right), &ctx);
        assert_eq!(outcome.action, Action::None);
    }

    #[test]
    fn test_now_playing_controls() {
        LAST_CMD_CODE.store(0, Ordering::SeqCst);
        let mut app = Sonos::new(
            slint::Weak::default(),
            test_snapshot_getter,
            test_command_sink,
        );
        let ctx = Ctx {
            now_ms: 1000,
            ble_linked: false,
        };

        // Enter NowPlaying
        app.handle(InputEvent::Select, &ctx);
        assert_eq!(app.mode, Mode::NowPlaying(0));

        // Volume rotation
        let outcome = app.handle(InputEvent::Rotate(5), &ctx);
        assert!(outcome.changed);
        assert_eq!(app.optimistic_volume, 30);
        assert!(app.volume_toast_expires_at_ms > ctx.now_ms);

        assert_eq!(LAST_CMD_CODE.load(Ordering::SeqCst), 2);
        assert_eq!(LAST_CMD_ARG.load(Ordering::SeqCst), 5);

        // Tap toggles Play/Pause
        let outcome = app.handle(InputEvent::Tap { x: 195, y: 195 }, &ctx);
        assert!(!app.optimistic_playing);
        assert_eq!(outcome.feedback, Some(Feedback::Haptic));

        assert_eq!(LAST_CMD_CODE.load(Ordering::SeqCst), 3);
        assert_eq!(LAST_CMD_ARG.load(Ordering::SeqCst), 0);

        // Swipe left skips track
        let outcome = app.handle(InputEvent::Swipe(SwipeDirection::Left), &ctx);
        assert_eq!(outcome.feedback, Some(Feedback::Beep));
        assert_eq!(LAST_CMD_CODE.load(Ordering::SeqCst), 4);
        assert_eq!(LAST_CMD_ARG.load(Ordering::SeqCst), 0);

        // Swipe right goes to previous track
        let outcome = app.handle(InputEvent::Swipe(SwipeDirection::Right), &ctx);
        assert_eq!(outcome.feedback, Some(Feedback::Beep));
        assert_eq!(LAST_CMD_CODE.load(Ordering::SeqCst), 5);
        assert_eq!(LAST_CMD_ARG.load(Ordering::SeqCst), 0);

        // Swipe up returns to Level 1 Group Selection without exiting app
        let outcome = app.handle(InputEvent::Swipe(SwipeDirection::Up), &ctx);
        assert_eq!(app.mode, Mode::GroupSelector);
        assert_eq!(outcome.action, Action::None);
        assert_eq!(outcome.feedback, Some(Feedback::Haptic));
    }

    #[test]
    fn test_format_time() {
        assert_eq!(format_time(0).as_str(), "00:00");
        assert_eq!(format_time(65).as_str(), "01:05");
        assert_eq!(format_time(3600).as_str(), "60:00");
    }

    #[test]
    fn test_marquee_scrolling() {
        let mut app = Sonos::new(
            slint::Weak::default(),
            test_snapshot_getter,
            test_command_sink,
        );
        let mut ctx = Ctx {
            now_ms: 1000,
            ble_linked: false,
        };

        // Short title "NEVER NEVER" has 11 chars, so marquee is inactive
        let _ = app.tick(&ctx);
        assert!(!app.title_scroll_active);
        assert!(app.title_scroll_offset.abs() < f32::EPSILON);

        // Update to long track title (> 14 chars)
        let mut long_title = heapless::String::new();
        let _ = long_title.push_str("A Very Long Track Title That Definitely Exceeds The Limit");
        app.snapshot.active_now_playing.track_title = long_title;

        // At t = 1000ms: cycle start, pause at 0
        let _ = app.tick(&ctx);
        assert!(app.title_scroll_active);
        assert!(app.title_scroll_offset.abs() < f32::EPSILON);

        // At t = 3500ms: midway through scroll
        ctx.now_ms = 3500;
        let outcome = app.tick(&ctx);
        assert!(outcome.changed);
        assert!(app.title_scroll_offset > 0.0);
    }
}
