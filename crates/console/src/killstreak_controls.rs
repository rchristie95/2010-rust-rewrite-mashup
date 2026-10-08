use bevy::input::keyboard::{KeyCode, KeyboardFocusLost, KeyboardInput};
use bevy::prelude::*;

#[derive(Default)]
pub(crate) struct OrderedDigitState {
    control_down: [bool; 2],
    digit_down: [bool; 10],
    control_captured: [bool; 10],
}

impl OrderedDigitState {
    fn begin_frame(&self, out: &mut frame::KeyboardDigitInput) {
        out.clear_edges();
        out.control_captured = self.control_captured;
    }

    fn clear(&mut self, out: &mut frame::KeyboardDigitInput) {
        *self = Self::default();
        *out = frame::KeyboardDigitInput::default();
    }

    fn control(&mut self, side: usize, pressed: bool) {
        self.control_down[side] = pressed;
    }

    fn digit(
        &mut self,
        digit: usize,
        pressed: bool,
        repeat: bool,
        out: &mut frame::KeyboardDigitInput,
    ) {
        if pressed {
            if !self.digit_down[digit] {
                let control = self.control_down.iter().any(|&down| down);
                self.control_captured[digit] = control;
                if !repeat {
                    if control {
                        out.control_pressed[digit] = true;
                    } else {
                        out.plain_pressed[digit] = true;
                    }
                }
            }
            self.digit_down[digit] = true;
        } else {
            self.digit_down[digit] = false;
            self.control_captured[digit] = false;
        }
        out.control_captured[digit] = self.control_captured[digit];
    }
}

fn digit_index(key: KeyCode) -> Option<usize> {
    Some(match key {
        KeyCode::Digit0 => 0,
        KeyCode::Digit1 => 1,
        KeyCode::Digit2 => 2,
        KeyCode::Digit3 => 3,
        KeyCode::Digit4 => 4,
        KeyCode::Digit5 => 5,
        KeyCode::Digit6 => 6,
        KeyCode::Digit7 => 7,
        KeyCode::Digit8 => 8,
        KeyCode::Digit9 => 9,
        _ => return None,
    })
}

pub(crate) fn collect(
    mut events: MessageReader<KeyboardInput>,
    mut focus_lost: MessageReader<KeyboardFocusLost>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut out: ResMut<frame::KeyboardDigitInput>,
    mut state: Local<OrderedDigitState>,
) {
    state.begin_frame(&mut out);
    let lost = focus_lost.read().count() != 0;
    let focused = windows.single().map_or(true, |window| window.focused);
    if lost || !focused {
        events.read().for_each(drop);
        state.clear(&mut out);
        return;
    }
    for event in events.read() {
        let pressed = event.state.is_pressed();
        match event.key_code {
            KeyCode::ControlLeft => state.control(0, pressed),
            KeyCode::ControlRight => state.control(1, pressed),
            key => {
                if let Some(digit) = digit_index(key) {
                    state.digit(digit, pressed, event.repeat, &mut out);
                }
            }
        }
    }
}

pub(crate) fn pressed_reward(input: &frame::KeyboardDigitInput, captured: bool) -> Option<usize> {
    if captured {
        return None;
    }
    input.control_pressed[1..]
        .iter()
        .position(|&pressed| pressed)
}

pub(crate) fn binding(
    button: crate::BindButton,
    command: u32,
    enabled: bool,
    input: Option<&frame::KeyboardDigitInput>,
) -> u32 {
    if !enabled {
        return command;
    }
    if let crate::BindButton::Key(key) = button {
        if matches!(key, KeyCode::ControlLeft | KeyCode::ControlRight)
            || digit_index(key)
                .is_some_and(|digit| input.is_some_and(|input| input.suppresses_plain(digit)))
        {
            return 0;
        }
    }
    command
}
