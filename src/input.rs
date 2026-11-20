//! Keyboard and mouse as a PS5 controller: rebindable keys, mouse buttons and wheel, mouse aim on the right stick.

use crate::remote;
use eframe::egui::{self, Event, InputState, PointerButton, Vec2};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Actions: the 16 PS5 buttons (remote::BUTTONS order), then these.
const EXTRA: [&str; 6] = ["L2", "R2", "Move forward", "Move back", "Move left", "Move right"];

/// Defaults copied from GTA V's PC on-foot keys: each PS5 button gets the key PC GTA V uses for the same action.
pub const DEFAULT: [&str; 22] = [
    "Shift", "R", "Space", "F", // Cross sprint, Circle reload/melee, Square jump, Triangle enter vehicle
    "G", "E", "ArrowUp", "Alt", // D-pad left detonate, right context action, up phone, down character wheel (hold)
    "Tab", "Q", "Ctrl", "C", // L1 weapon wheel, R1 cover, L3 stealth, R3 look behind
    "P", "Backspace", "V", "Home", // Options pause (Esc leaves nagito), Create, Touchpad camera view, PS
    "Mouse Right", "Mouse Left", // L2 aim, R2 shoot
    "W", "S", "A", "D",
];

pub fn action_name(i: usize) -> &'static str {
    remote::BUTTONS.get(i).copied().unwrap_or_else(|| EXTRA[i - 16])
}

pub fn defaults() -> Vec<String> {
    DEFAULT.iter().map(|k| k.to_string()).collect()
}

const MOUSE_BUTTONS: [(PointerButton, &str); 5] = [
    (PointerButton::Primary, "Mouse Left"),
    (PointerButton::Secondary, "Mouse Right"),
    (PointerButton::Middle, "Mouse Middle"),
    (PointerButton::Extra1, "Mouse 4"),
    (PointerButton::Extra2, "Mouse 5"),
];

/// A wheel notch holds its binding this long, so the console sees a real press.
const WHEEL_PRESS: Duration = Duration::from_millis(60);
/// Mouse speed is averaged over this window: long enough to bridge a 125 Hz mouse, short enough to stop fast.
const AIM_WINDOW: Duration = Duration::from_millis(16);
/// Raw mouse counts per second (sensitivity 1) that push the right stick all the way.
const FULL_TILT: f32 = 1000.0;

#[derive(Default)]
pub struct Kbm {
    motion: VecDeque<(Instant, Vec2)>,
    wheel_up_until: Option<Instant>,
    wheel_down_until: Option<Instant>,
}

pub struct Aim {
    /// Stick deflection per unit of mouse speed.
    pub sensitivity: f32,
    /// Smallest deflection sent, to get past the game's own stick dead zone (0..0.5).
    pub dead_zone: f32,
    pub invert_y: bool,
}

impl Kbm {
    pub fn read(&mut self, i: &InputState, keys: &[String], aim: &Aim) -> remote::Input {
        let now = Instant::now();
        let mut motion = Vec2::ZERO;
        for e in &i.events {
            match e {
                Event::MouseMoved(d) => motion += *d,
                Event::MouseWheel { delta, .. } if delta.y > 0.0 => self.wheel_up_until = Some(now + WHEEL_PRESS),
                Event::MouseWheel { delta, .. } if delta.y < 0.0 => self.wheel_down_until = Some(now + WHEEL_PRESS),
                _ => {}
            }
        }
        self.motion.push_back((now, motion));
        while self.motion.front().is_some_and(|(t, _)| now.duration_since(*t) > AIM_WINDOW) {
            self.motion.pop_front();
        }

        let down = |a: usize| keys.get(a).is_some_and(|k| self.bound_down(k, i, now));
        let mut inp = remote::Input::default();
        for b in 0..16 {
            if down(b) {
                inp.buttons |= 1 << b;
            }
        }
        inp.l2 = if down(16) { 255 } else { 0 };
        inp.r2 = if down(17) { 255 } else { 0 };
        let axis = |neg: usize, pos: usize| ((down(pos) as i32 - down(neg) as i32) * 32767) as i16;
        inp.ly = axis(18, 19);
        inp.lx = axis(20, 21);
        let total: Vec2 = self.motion.iter().map(|(_, d)| *d).fold(Vec2::ZERO, |a, b| a + b);
        (inp.rx, inp.ry) = stick(total / AIM_WINDOW.as_secs_f32(), aim);
        inp
    }

    fn bound_down(&self, name: &str, i: &InputState, now: Instant) -> bool {
        match name {
            "" => false,
            "Shift" => key_down(VK_SHIFT),
            "Ctrl" => key_down(VK_CONTROL),
            "Alt" => key_down(VK_MENU),
            "Wheel Up" => self.wheel_up_until.is_some_and(|t| now < t),
            "Wheel Down" => self.wheel_down_until.is_some_and(|t| now < t),
            _ => match MOUSE_BUTTONS.iter().find(|(_, n)| *n == name) {
                Some((b, _)) => i.pointer.button_down(*b),
                None => egui::Key::from_name(name).is_some_and(|k| vk(k).map_or_else(|| i.key_down(k), key_down)),
            },
        }
    }
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(vk: i32) -> i16;
}

const VK_SHIFT: i32 = 0x10;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;

/// The key's real state from Windows. egui's key events can't be trusted for a game: holding Ctrl turns
/// C, V and X into copy/paste and drops them, and keys tracked by character can stick when Shift changes.
fn key_down(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) < 0 }
}

/// Windows virtual-key code for an egui key, or None to fall back to egui's own state.
fn vk(k: egui::Key) -> Option<i32> {
    use egui::Key::*;
    let name = k.name();
    match name.as_bytes() {
        [c] if c.is_ascii_uppercase() || c.is_ascii_digit() => return Some(*c as i32),
        [b'F', rest @ ..] if !rest.is_empty() => {
            return std::str::from_utf8(rest).ok()?.parse::<i32>().ok().filter(|n| (1..=24).contains(n)).map(|n| 0x6F + n);
        }
        _ => {}
    }
    Some(match k {
        Backspace => 0x08,
        Tab => 0x09,
        Enter => 0x0D,
        Escape => 0x1B,
        Space => 0x20,
        PageUp => 0x21,
        PageDown => 0x22,
        End => 0x23,
        Home => 0x24,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        Insert => 0x2D,
        Delete => 0x2E,
        Exclamationmark => b'1' as i32,
        Semicolon | Colon => 0xBA,
        Equals | Plus => 0xBB,
        Comma => 0xBC,
        Minus => 0xBD,
        Period => 0xBE,
        Slash | Questionmark => 0xBF,
        Backtick => 0xC0,
        OpenBracket | OpenCurlyBracket => 0xDB,
        Backslash | Pipe => 0xDC,
        CloseBracket | CloseCurlyBracket => 0xDD,
        Quote => 0xDE,
        _ => return None,
    })
}

/// Mouse speed (counts per second) to right-stick deflection. Keeps the direction, scales the length,
/// and lifts any movement past the dead zone so small aim corrections register.
pub fn stick(velocity: Vec2, aim: &Aim) -> (i16, i16) {
    let speed = velocity.length();
    if speed < 1.0 {
        return (0, 0);
    }
    let m = (speed * aim.sensitivity / FULL_TILT).min(1.0);
    let out = aim.dead_zone + (1.0 - aim.dead_zone) * m;
    let dir = velocity / speed;
    let y = if aim.invert_y { -dir.y } else { dir.y };
    ((dir.x * out * 32767.0) as i16, (y * out * 32767.0) as i16)
}

/// While rebinding: the first key, mouse button, wheel notch or modifier pressed, as a binding name.
/// Some("") means Esc (clear). `armed` is false until the click that started rebinding is released.
pub fn capture(i: &InputState, armed: bool) -> Option<String> {
    for e in &i.events {
        match e {
            Event::Key { key: egui::Key::Escape, pressed: true, .. } => return Some(String::new()),
            Event::Key { key, pressed: true, repeat: false, .. } => return Some(key.name().to_owned()),
            Event::PointerButton { button, pressed: true, .. } if armed => {
                return MOUSE_BUTTONS.iter().find(|(b, _)| b == button).map(|(_, n)| n.to_string());
            }
            Event::MouseWheel { delta, .. } if delta.y != 0.0 => return Some(if delta.y > 0.0 { "Wheel Up" } else { "Wheel Down" }.into()),
            _ => {}
        }
    }
    let m = i.modifiers;
    [(m.shift, "Shift"), (m.ctrl, "Ctrl"), (m.alt, "Alt")].iter().find(|(on, _)| *on).map(|(_, n)| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_key_is_read_from_windows() {
        for name in DEFAULT.iter().filter(|n| !n.starts_with("Mouse") && !["Shift", "Ctrl", "Alt"].contains(n)) {
            let key = egui::Key::from_name(name).unwrap_or_else(|| panic!("{name} is not a key"));
            assert!(vk(key).is_some(), "{name} has no virtual-key code");
        }
        assert_eq!(vk(egui::Key::W), Some(0x57));
        assert_eq!(vk(egui::Key::F5), Some(0x74));
        assert_eq!(vk(egui::Key::ArrowUp), Some(0x26));
    }

    const AIM: Aim = Aim { sensitivity: 1.0, dead_zone: 0.2, invert_y: false };

    #[test]
    fn still_mouse_centres_the_stick() {
        assert_eq!(stick(Vec2::ZERO, &AIM), (0, 0));
    }

    #[test]
    fn slow_motion_clears_the_dead_zone_and_keeps_direction() {
        let (x, y) = stick(Vec2::new(10.0, 0.0), &AIM);
        assert!(x as f32 > 0.2 * 32767.0 * 0.99 && y == 0, "{x},{y}");
        let (x, y) = stick(Vec2::new(-300.0, 300.0), &AIM);
        assert!(x < 0 && y > 0 && (x as i32 + y as i32).abs() < 2, "diagonal stays diagonal: {x},{y}");
    }

    #[test]
    fn fast_motion_saturates_and_invert_flips_y() {
        let (x, _) = stick(Vec2::new(1e6, 0.0), &AIM);
        assert_eq!(x, 32767);
        let inv = Aim { invert_y: true, ..AIM };
        assert!(stick(Vec2::new(0.0, 500.0), &inv).1 < 0);
    }
}
