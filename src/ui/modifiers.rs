//! Current keyboard modifiers, for mouse handlers (iced's mouse events don't
//! carry them).  Every custom widget calls [`observe`] first thing in
//! `update`, so the value is current before any click is handled.

use std::sync::atomic::{AtomicU32, Ordering};

use iced::Event;
use iced::keyboard::{self, Modifiers};

static MODS: AtomicU32 = AtomicU32::new(0);

pub fn observe(event: &Event) {
    if let Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
        MODS.store(m.bits(), Ordering::Relaxed);
    }
}

pub fn get() -> Modifiers {
    Modifiers::from_bits_truncate(MODS.load(Ordering::Relaxed))
}
