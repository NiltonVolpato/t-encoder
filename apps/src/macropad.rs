//! BLE macropad.
//!
//! Rotate to pick a macro, press to send it. The chords are HID modifier and
//! usage values; this app has no idea a radio exists — it returns a
//! [`KeyChord`] and the firmware decides whether anything is listening. That is
//! the same split as the buzzer.

use launcher::{
    App, AppFactory, Ctx, Feedback, IconId, InputEvent, KeyChord, Manifest, Outcome, TouchAccess,
    ViewId,
};
use ui::{MacroEntry, MacropadState, Shell};

/// HID modifier bits.
const CTRL: u8 = 0x01;
const SHIFT: u8 = 0x02;
const GUI: u8 = 0x08;

/// HID keyboard usage ids for the letters and digits we send.
const KEY_C: u8 = 0x06;
const KEY_Q: u8 = 0x14;
const KEY_V: u8 = 0x19;
const KEY_Z: u8 = 0x1D;
const KEY_4: u8 = 0x21;

/// One entry on the wheel: what it is called, what it looks like, what it sends.
struct Macro {
    label: &'static str,
    chord: &'static str,
    keys: KeyChord,
}

/// The macros, in wheel order. Adding one is a line here — the UI is driven by
/// this table, so nothing else needs to change.
const MACROS: [Macro; 6] = [
    Macro {
        label: "Copy",
        chord: "⌘C",
        keys: KeyChord {
            modifiers: GUI,
            usage: KEY_C,
        },
    },
    Macro {
        label: "Paste",
        chord: "⌘V",
        keys: KeyChord {
            modifiers: GUI,
            usage: KEY_V,
        },
    },
    Macro {
        label: "Undo",
        chord: "⌘Z",
        keys: KeyChord {
            modifiers: GUI,
            usage: KEY_Z,
        },
    },
    Macro {
        label: "Redo",
        chord: "⇧⌘Z",
        keys: KeyChord {
            modifiers: GUI | SHIFT,
            usage: KEY_Z,
        },
    },
    Macro {
        label: "Screenshot",
        chord: "⇧⌘4",
        keys: KeyChord {
            modifiers: GUI | SHIFT,
            usage: KEY_4,
        },
    },
    Macro {
        label: "Lock",
        chord: "⌃⌘Q",
        keys: KeyChord {
            modifiers: GUI | CTRL,
            usage: KEY_Q,
        },
    },
];

/// Builds [`Macropad`] instances.
pub struct MacropadFactory {
    manifest: Manifest,
    shell: slint::Weak<Shell>,
}

impl MacropadFactory {
    /// Creates the factory against the shared Slint tree.
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> MacropadFactory {
        MacropadFactory {
            manifest: Manifest {
                name: "Macropad",
                icon: IconId(1),
                view: ViewId(2),
                // Cool blue — the "connected to something" app, as far from the
                // pomodoro's warm red as the palette goes.
                accent: embedded_graphics::pixelcolor::Rgb565::new(8, 34, 31),
                touch: TouchAccess::Gestures,
            },
            shell,
        }
    }
}

impl AppFactory for MacropadFactory {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn create(&self) -> alloc::boxed::Box<dyn App + '_> {
        alloc::boxed::Box::new(Macropad::new(self.shell.clone()))
    }
}

/// A running macropad.
pub struct Macropad {
    shell: slint::Weak<Shell>,
    selected: usize,
    /// Last seen BLE link state. Cached in `tick` because `sync` takes `&self`
    /// and so cannot read shared state itself.
    linked: bool,
}

impl Macropad {
    fn new(shell: slint::Weak<Shell>) -> Macropad {
        Macropad {
            shell,
            selected: 0,
            linked: false,
        }
    }
}

impl App for Macropad {
    fn handle(&mut self, event: InputEvent, _ctx: &Ctx) -> Outcome {
        match event {
            InputEvent::Rotate(delta) => {
                let last = MACROS.len().saturating_sub(1);
                let next = i64::from(delta)
                    .saturating_add(i64::try_from(self.selected).unwrap_or(0))
                    .clamp(0, i64::try_from(last).unwrap_or(0));
                let next = usize::try_from(next).unwrap_or(0);
                if next == self.selected {
                    return Outcome::NONE;
                }
                self.selected = next;
                Outcome::CHANGED
            }
            InputEvent::Select => match MACROS.get(self.selected) {
                // Beep rather than buzz: this is a keystroke leaving the
                // device, and it should sound like a key.
                Some(entry) => Outcome::send_keys(entry.keys, Feedback::Beep),
                None => Outcome::NONE,
            },
        }
    }

    fn tick(&mut self, ctx: &Ctx) -> Outcome {
        // Adopt the radio's view of the world. Repaint only on a change, so a
        // paired macropad sitting idle costs nothing.
        let linked = ctx.ble_linked;
        if linked == self.linked {
            return Outcome::NONE;
        }
        self.linked = linked;
        Outcome::CHANGED
    }

    fn sync(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let entries: alloc::vec::Vec<MacroEntry> = MACROS
            .iter()
            .map(|entry| MacroEntry {
                label: entry.label.into(),
                chord: entry.chord.into(),
            })
            .collect();
        shell.set_macropad(MacropadState {
            entries: slint::ModelRc::new(slint::VecModel::from(entries)),
            selected: i32::try_from(self.selected).unwrap_or(0),
            accent: slint::Color::from_rgb_u8(64, 140, 255),
            linked: self.linked,
            // "connected", not "paired": a link can be up while bonding is
            // still in progress, and claiming otherwise is how the last round
            // of testing got confusing.
            status: if self.linked {
                "connected"
            } else {
                "waiting for a host"
            }
            .into(),
        });
    }
}
