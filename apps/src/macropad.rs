//! BLE macropad.
//!
//! Multi-profile macro engine with rotary, tap, and swipe actions.
//! Outputs [`KeyChord`] reports over BLE HID.

use core::fmt::Write;
use launcher::{
    App, AppFactory, Ctx, Feedback, IconId, InputEvent, KeyChord, Manifest, Outcome,
    SwipeDirection, TouchAccess, ViewId,
};
use ui::{MacropadState, Shell};

/// Maximum number of configurable profiles.
pub const MAX_PROFILES: usize = 8;

/// Configurable key binding for a macropad action.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct MacroBinding {
    pub label: heapless::String<24>,
    pub modifiers: u8,
    pub usage: u8,
}

impl MacroBinding {
    /// Creates a new binding with the given label, HID modifiers, and usage ID.
    #[must_use]
    pub fn new(label: &str, modifiers: u8, usage: u8) -> Self {
        let mut str_label = heapless::String::new();
        let _ = str_label.push_str(label);
        Self {
            label: str_label,
            modifiers,
            usage,
        }
    }
}

/// A named set of 8 macro bindings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub name: heapless::String<24>,
    pub rotate_cw: MacroBinding,
    pub rotate_ccw: MacroBinding,
    pub press: MacroBinding,
    pub tap: MacroBinding,
    pub swipe_left: MacroBinding,
    pub swipe_right: MacroBinding,
    pub swipe_up: MacroBinding,
    pub swipe_down: MacroBinding,
}

/// Full macropad settings containing up to [`MAX_PROFILES`] configurations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct MacropadSettings {
    pub profiles: heapless::Vec<Profile, MAX_PROFILES>,
    pub active_index: usize,
}

/// HID modifier bits.
pub const CTRL: u8 = 0x01;
pub const SHIFT: u8 = 0x02;
pub const ALT: u8 = 0x04;
pub const GUI: u8 = 0x08;

impl Default for MacropadSettings {
    fn default() -> Self {
        let mut profiles = heapless::Vec::new();

        // 1. Web Browser
        let mut browser_name = heapless::String::new();
        let _ = browser_name.push_str("Web Browser");
        let browser = Profile {
            name: browser_name,
            rotate_cw: MacroBinding::new("Scroll Down", 0, 0x51), // Down Arrow
            rotate_ccw: MacroBinding::new("Scroll Up", 0, 0x52),  // Up Arrow
            press: MacroBinding::new("Reload", GUI, 0x15),        // ⌘R
            tap: MacroBinding::new("New Tab", GUI, 0x17),         // ⌘T
            swipe_left: MacroBinding::new("Back", GUI, 0x2F),     // ⌘[
            swipe_right: MacroBinding::new("Forward", GUI, 0x30), // ⌘]
            swipe_up: MacroBinding::new("Close Tab", GUI, 0x1A),  // ⌘W
            swipe_down: MacroBinding::new("Reopen Tab", GUI | SHIFT, 0x17), // ⇧⌘T
        };
        let _ = profiles.push(browser);

        // 2. Media Player
        let mut media_name = heapless::String::new();
        let _ = media_name.push_str("Media Player");
        let media = Profile {
            name: media_name,
            rotate_cw: MacroBinding::new("Vol Up", 0, 0x80), // Volume Up
            rotate_ccw: MacroBinding::new("Vol Down", 0, 0x81), // Volume Down
            press: MacroBinding::new("Play/Pause", 0, 0x2C), // Space
            tap: MacroBinding::new("Mute", 0, 0x7F),         // Mute
            swipe_left: MacroBinding::new("Prev Track", GUI, 0x50), // ⌘←
            swipe_right: MacroBinding::new("Next Track", GUI, 0x4F), // ⌘→
            swipe_up: MacroBinding::new("Vol Max", 0, 0x45), // F12
            swipe_down: MacroBinding::new("Vol Min", 0, 0x44), // F11
        };
        let _ = profiles.push(media);

        // 3. Mac Shortcuts
        let mut mac_name = heapless::String::new();
        let _ = mac_name.push_str("Mac Shortcuts");
        let mac = Profile {
            name: mac_name,
            rotate_cw: MacroBinding::new("Redo", GUI | SHIFT, 0x1D), // ⇧⌘Z
            rotate_ccw: MacroBinding::new("Undo", GUI, 0x1D),        // ⌘Z
            press: MacroBinding::new("Copy", GUI, 0x06),             // ⌘C
            tap: MacroBinding::new("Paste", GUI, 0x19),              // ⌘V
            swipe_left: MacroBinding::new("Space Left", CTRL, 0x50), // ⌃←
            swipe_right: MacroBinding::new("Space Right", CTRL, 0x4F), // ⌃→
            swipe_up: MacroBinding::new("Mission Ctrl", CTRL, 0x52), // ⌃↑
            swipe_down: MacroBinding::new("App Expose", CTRL, 0x51), // ⌃↓
        };
        let _ = profiles.push(mac);

        Self {
            profiles,
            active_index: 0,
        }
    }
}

/// Formats modifier bits and HID key usage into a readable chord string (e.g. "⇧⌘Z").
#[must_use]
pub fn format_chord(modifiers: u8, usage: u8) -> heapless::String<16> {
    let mut s = heapless::String::new();
    if modifiers & CTRL != 0 {
        let _ = s.push_str("⌃");
    }
    if modifiers & ALT != 0 {
        let _ = s.push_str("⌥");
    }
    if modifiers & SHIFT != 0 {
        let _ = s.push_str("⇧");
    }
    if modifiers & GUI != 0 {
        let _ = s.push_str("⌘");
    }

    match usage {
        0x04..=0x1D => {
            let offset = usage.saturating_sub(0x04);
            let byte = b'A'.saturating_add(offset);
            let ch = char::from(byte);
            let mut buf = [0u8; 4];
            let str_slice = ch.encode_utf8(&mut buf);
            let _ = s.push_str(str_slice);
        }
        0x1E..=0x26 => {
            let offset = usage.saturating_sub(0x1E);
            let byte = b'1'.saturating_add(offset);
            let ch = char::from(byte);
            let mut buf = [0u8; 4];
            let str_slice = ch.encode_utf8(&mut buf);
            let _ = s.push_str(str_slice);
        }
        0x27 => {
            let _ = s.push_str("0");
        }
        0x28 => {
            let _ = s.push_str("Enter");
        }
        0x29 => {
            let _ = s.push_str("Esc");
        }
        0x2A => {
            let _ = s.push_str("⌫");
        }
        0x2B => {
            let _ = s.push_str("Tab");
        }
        0x2C => {
            let _ = s.push_str("Space");
        }
        0x2F => {
            let _ = s.push_str("[");
        }
        0x30 => {
            let _ = s.push_str("]");
        }
        0x3A..=0x45 => {
            let num = usage.saturating_sub(0x3A).saturating_add(1);
            let _ = write!(s, "F{num}");
        }
        0x4F => {
            let _ = s.push_str("→");
        }
        0x50 => {
            let _ = s.push_str("←");
        }
        0x51 => {
            let _ = s.push_str("↓");
        }
        0x52 => {
            let _ = s.push_str("↑");
        }
        0x80 => {
            let _ = s.push_str("Vol+");
        }
        0x81 => {
            let _ = s.push_str("Vol-");
        }
        0x7F => {
            let _ = s.push_str("Mute");
        }
        0 => {}
        other => {
            let _ = write!(s, "0x{other:02X}");
        }
    }
    s
}

/// Function pointer type providing current persisted settings.
pub type SettingsGetter = fn() -> MacropadSettings;

fn default_settings_getter() -> MacropadSettings {
    MacropadSettings::default()
}

/// Builds [`Macropad`] instances.
pub struct MacropadFactory {
    manifest: Manifest,
    shell: slint::Weak<Shell>,
    get_settings: SettingsGetter,
}

impl MacropadFactory {
    /// Creates the factory with default settings getter.
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> MacropadFactory {
        Self::with_settings_getter(shell, default_settings_getter)
    }

    /// Creates the factory with an injected settings provider.
    #[must_use]
    pub fn with_settings_getter(
        shell: slint::Weak<Shell>,
        get_settings: SettingsGetter,
    ) -> MacropadFactory {
        MacropadFactory {
            manifest: Manifest {
                name: "Macropad",
                icon: IconId(1),
                view: ViewId(2),
                accent: embedded_graphics::pixelcolor::Rgb565::new(8, 34, 31),
                touch: TouchAccess::AllGestures,
                requires_ble: true,
            },
            shell,
            get_settings,
        }
    }
}

impl AppFactory for MacropadFactory {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn create(&self) -> alloc::boxed::Box<dyn App + '_> {
        alloc::boxed::Box::new(Macropad::new(self.shell.clone(), self.get_settings))
    }
}

/// Active mode of the Macropad application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Profile selection carousel (Landing Page).
    Selector,
    /// Active execution of bindings for profile at index.
    Active(usize),
}

/// A running macropad app instance.
pub struct Macropad {
    shell: slint::Weak<Shell>,
    mode: Mode,
    settings: MacropadSettings,
    get_settings: SettingsGetter,
    selected_profile: usize,
    last_action_label: heapless::String<24>,
    last_action_chord: heapless::String<16>,
    toast_message: heapless::String<32>,
    toast_expires_at_ms: u64,
    hold_progress: f32,
    linked: bool,
}

impl Macropad {
    fn new(shell: slint::Weak<Shell>, get_settings: SettingsGetter) -> Macropad {
        let settings = get_settings();
        let selected_profile = settings
            .active_index
            .min(settings.profiles.len().saturating_sub(1));
        Macropad {
            shell,
            mode: Mode::Selector,
            settings,
            get_settings,
            selected_profile,
            last_action_label: heapless::String::new(),
            last_action_chord: heapless::String::new(),
            toast_message: heapless::String::new(),
            toast_expires_at_ms: 0,
            hold_progress: 0.0,
            linked: false,
        }
    }

    fn enter_active(&mut self, profile_idx: usize, now_ms: u64) -> Outcome {
        self.mode = Mode::Active(profile_idx);
        self.last_action_label.clear();
        self.last_action_chord.clear();
        self.toast_message.clear();
        let _ = self.toast_message.push_str("Touch and hold to go back");
        self.toast_expires_at_ms = now_ms.saturating_add(1000);
        self.hold_progress = 0.0;
        Outcome::buzz(Feedback::Haptic)
    }

    fn trigger_binding(&mut self, binding: MacroBinding) -> Outcome {
        self.last_action_label = binding.label;
        self.last_action_chord = format_chord(binding.modifiers, binding.usage);
        Outcome::send_keys(
            KeyChord {
                modifiers: binding.modifiers,
                usage: binding.usage,
            },
            Feedback::Beep,
        )
    }

    fn handle_selector(&mut self, event: InputEvent, ctx: &Ctx) -> Outcome {
        match event {
            InputEvent::Rotate(delta) => {
                let count = self.settings.profiles.len();
                if count == 0 {
                    return Outcome::NONE;
                }
                let last = count.saturating_sub(1);
                let cur = i64::try_from(self.selected_profile).unwrap_or(0);
                let next = cur
                    .saturating_add(i64::from(delta))
                    .clamp(0, i64::try_from(last).unwrap_or(0));
                let next_usize = usize::try_from(next).unwrap_or(0);
                if next_usize == self.selected_profile {
                    return Outcome::NONE;
                }
                self.selected_profile = next_usize;
                Outcome::CHANGED
            }
            InputEvent::Select | InputEvent::Tap { .. } => {
                if self.settings.profiles.is_empty() {
                    return Outcome::NONE;
                }
                self.enter_active(self.selected_profile, ctx.now_ms)
            }
            _ => Outcome::NONE,
        }
    }

    fn handle_active(&mut self, event: InputEvent, profile_idx: usize) -> Outcome {
        match event {
            InputEvent::HoldProgress(pct) => {
                self.hold_progress = f32::from(pct) / 100.0;
                Outcome::CHANGED
            }
            InputEvent::Hold => {
                self.hold_progress = 0.0;
                self.mode = Mode::Selector;
                Outcome::buzz(Feedback::Haptic)
            }
            _ => {
                let Some(profile) = self.settings.profiles.get(profile_idx) else {
                    self.mode = Mode::Selector;
                    return Outcome::CHANGED;
                };

                let action = match event {
                    InputEvent::Rotate(delta) => {
                        if delta > 0 {
                            profile.rotate_cw.clone()
                        } else {
                            profile.rotate_ccw.clone()
                        }
                    }
                    InputEvent::Select => profile.press.clone(),
                    InputEvent::Tap { .. } => profile.tap.clone(),
                    InputEvent::Swipe(dir) => match dir {
                        SwipeDirection::Left => profile.swipe_left.clone(),
                        SwipeDirection::Right => profile.swipe_right.clone(),
                        SwipeDirection::Up => profile.swipe_up.clone(),
                        SwipeDirection::Down => profile.swipe_down.clone(),
                    },
                    InputEvent::HoldProgress(_) | InputEvent::Hold => return Outcome::NONE,
                };

                self.trigger_binding(action)
            }
        }
    }
}

impl App for Macropad {
    fn handle(&mut self, event: InputEvent, ctx: &Ctx) -> Outcome {
        match self.mode {
            Mode::Selector => self.handle_selector(event, ctx),
            Mode::Active(profile_idx) => self.handle_active(event, profile_idx),
        }
    }

    fn tick(&mut self, ctx: &Ctx) -> Outcome {
        let mut changed = false;

        let linked = ctx.ble_linked;
        if linked != self.linked {
            self.linked = linked;
            changed = true;
        }

        if self.toast_expires_at_ms > 0 && ctx.now_ms >= self.toast_expires_at_ms {
            self.toast_expires_at_ms = 0;
            changed = true;
        }

        let fresh = (self.get_settings)();
        if fresh != self.settings {
            self.settings = fresh;
            self.selected_profile = self
                .selected_profile
                .min(self.settings.profiles.len().saturating_sub(1));
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

        let profile_names: alloc::vec::Vec<slint::SharedString> = self
            .settings
            .profiles
            .iter()
            .map(|p| slint::SharedString::from(p.name.as_str()))
            .collect();

        let (mode_num, active_name) = match self.mode {
            Mode::Selector => (0, slint::SharedString::default()),
            Mode::Active(idx) => {
                let name = self
                    .settings
                    .profiles
                    .get(idx)
                    .map_or("", |p| p.name.as_str());
                (1, slint::SharedString::from(name))
            }
        };

        shell.set_macropad(MacropadState {
            mode: mode_num,
            profiles: slint::ModelRc::new(slint::VecModel::from(profile_names)),
            selected_profile: i32::try_from(self.selected_profile).unwrap_or(0),
            active_profile_name: active_name,
            last_action_label: slint::SharedString::from(self.last_action_label.as_str()),
            last_action_chord: slint::SharedString::from(self.last_action_chord.as_str()),
            toast_message: slint::SharedString::from(self.toast_message.as_str()),
            toast_visible: self.toast_expires_at_ms > 0,
            hold_progress: self.hold_progress,
            accent: slint::Color::from_rgb_u8(64, 140, 255),
            linked: self.linked,
            status: if self.linked { "Connected" } else { "Offline" }.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_chord() {
        assert_eq!(format_chord(GUI, 0x06).as_str(), "⌘C");
        assert_eq!(format_chord(GUI | SHIFT, 0x1D).as_str(), "⇧⌘Z");
        assert_eq!(format_chord(CTRL, 0x50).as_str(), "⌃←");
        assert_eq!(format_chord(0, 0x2C).as_str(), "Space");
        assert_eq!(format_chord(0, 0x45).as_str(), "F12");
    }

    #[test]
    fn test_default_settings() {
        let settings = MacropadSettings::default();
        assert_eq!(settings.profiles.len(), 3);
        assert_eq!(
            settings.profiles.first().map(|p| p.name.as_str()),
            Some("Web Browser")
        );
        assert_eq!(
            settings.profiles.get(1).map(|p| p.name.as_str()),
            Some("Media Player")
        );
        assert_eq!(
            settings.profiles.get(2).map(|p| p.name.as_str()),
            Some("Mac Shortcuts")
        );
    }

    #[test]
    fn test_macropad_mode_transitions() {
        let shell = slint::Weak::default();
        let mut macropad = Macropad::new(shell, default_settings_getter);
        let ctx = Ctx {
            now_ms: 1000,
            ble_linked: false,
        };

        assert_eq!(macropad.mode, Mode::Selector);

        // Rotate to select next profile
        let outcome = macropad.handle(InputEvent::Rotate(1), &ctx);
        assert_eq!(outcome, Outcome::CHANGED);
        assert_eq!(macropad.selected_profile, 1);

        // Press to enter Active mode
        let outcome = macropad.handle(InputEvent::Select, &ctx);
        assert_eq!(outcome, Outcome::buzz(Feedback::Haptic));
        assert_eq!(macropad.mode, Mode::Active(1));
        assert!(macropad.toast_expires_at_ms > ctx.now_ms);

        // Rotate in active mode triggers configured keys
        let outcome = macropad.handle(InputEvent::Rotate(1), &ctx);
        let Some(keys) = outcome.keys else {
            panic!("expected Keys outcome");
        };
        assert_eq!(keys.usage, 0x80); // Vol Up on Media Player

        // Hold progress
        let outcome = macropad.handle(InputEvent::HoldProgress(50), &ctx);
        assert_eq!(outcome, Outcome::CHANGED);
        assert!((macropad.hold_progress - 0.5).abs() < 0.01);

        // Complete Hold returns to Selector mode
        let outcome = macropad.handle(InputEvent::Hold, &ctx);
        assert_eq!(outcome, Outcome::buzz(Feedback::Haptic));
        assert_eq!(macropad.mode, Mode::Selector);
        assert!(macropad.hold_progress.abs() < f32::EPSILON);
    }
}
