//! Logical input, independent of window libraries and physical key mappings.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    Action,
    Cancel,
}

impl Button {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "action" => Self::Action,
            "cancel" => Self::Cancel,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buttons(u8);

impl Buttons {
    pub fn new(buttons: impl IntoIterator<Item = Button>) -> Self {
        let mut result = Self::default();
        for button in buttons {
            result.0 |= 1 << button as u8;
        }
        result
    }

    pub fn contains(self, button: Button) -> bool {
        self.0 & (1 << button as u8) != 0
    }
}

/// A poll result or a tick snapshot. Explicit edges can report a quick tap even
/// when held state is unchanged. Pending repeated edges coalesce to booleans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputSnapshot {
    pub held: Buttons,
    pub pressed: Buttons,
    pub released: Buttons,
}

impl InputSnapshot {
    pub fn held(buttons: impl IntoIterator<Item = Button>) -> Self {
        Self {
            held: Buttons::new(buttons),
            ..Self::default()
        }
    }
}

#[derive(Default)]
#[cfg(feature = "scripting")]
pub(crate) struct InputQueue {
    held: Buttons,
    pressed: Buttons,
    released: Buttons,
}

#[cfg(feature = "scripting")]
impl InputQueue {
    /// Queue tick edges and return this frame's independent draw snapshot.
    pub(crate) fn sample(&mut self, input: InputSnapshot) -> InputSnapshot {
        let pressed = Buttons(input.pressed.0 | (input.held.0 & !self.held.0));
        let released = Buttons(input.released.0 | (self.held.0 & !input.held.0));
        self.held = input.held;
        self.pressed.0 |= pressed.0;
        self.released.0 |= released.0;
        InputSnapshot {
            held: self.held,
            pressed,
            released,
        }
    }

    pub(crate) fn consume(&mut self) -> InputSnapshot {
        InputSnapshot {
            held: self.held,
            pressed: std::mem::take(&mut self.pressed),
            released: std::mem::take(&mut self.released),
        }
    }
}
