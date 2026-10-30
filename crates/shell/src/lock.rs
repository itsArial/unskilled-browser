//! The lock screen shown on every launch, before Chromium starts.

use crate::{clock, theme};
use ui::{Rect, Scene, Spring, SpringConfig, Weight};
use vault::{Protection, State};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, NamedKey},
};
use zeroize::Zeroizing;

const MAX_LEN: usize = 64;
const DOT_GAP: f32 = 18.0;

enum Mode {
    Create,
    Confirm { first: Zeroizing<String> },
    Unlock,
    /// Unlocked with a passphrase on a machine whose TPM is now usable: offer a PIN instead.
    Upgrade,
    UpgradeConfirm { first: Zeroizing<String> },
}

pub enum Submit {
    Create(Zeroizing<String>),
    Unlock(Zeroizing<String>),
    Upgrade(Zeroizing<String>),
    /// Keep the passphrase.
    SkipUpgrade,
}

pub struct LockScreen {
    mode: Mode,
    protection: Protection,
    input: Zeroizing<String>,
    message: String,
    error: bool,
    busy: bool,
    /// Horizontal shake on a wrong PIN.
    shake: Spring,
    /// Each typed character's dot pops in.
    dots: Vec<Spring>,
    appear: Spring,
    spin: f32,
    tpm_denied: bool,
}

impl LockScreen {
    pub fn new(state: State, tpm_denied: bool) -> Self {
        let (mode, protection) = match state {
            State::New { protection } => (Mode::Create, protection),
            State::Locked { protection } => (Mode::Unlock, protection),
        };
        let mut appear = Spring::new(0.0, SpringConfig::GLIDE);
        appear.set(1.0);
        Self {
            mode,
            protection,
            input: Zeroizing::new(String::new()),
            message: String::new(),
            error: false,
            busy: false,
            shake: Spring::new(0.0, SpringConfig::from_response(0.35, 0.22)),
            dots: Vec::new(),
            appear,
            spin: 0.0,
            tpm_denied,
        }
    }

    fn prompt(&self) -> &'static str {
        match (&self.mode, self.protection) {
            (Mode::Create, Protection::Tpm) => "create a pin",
            (Mode::Create, Protection::Passphrase) => "create a passphrase",
            (Mode::Confirm { .. }, _) => "repeat it",
            (Mode::Unlock, Protection::Tpm) => "enter pin",
            (Mode::Unlock, Protection::Passphrase) => "enter passphrase",
            (Mode::Upgrade, _) => "choose a pin",
            (Mode::UpgradeConfirm { .. }, _) => "repeat the pin",
        }
    }

    pub fn key(&mut self, event: &KeyEvent) -> Option<Submit> {
        if event.state != ElementState::Pressed || self.busy {
            return None;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => return self.submit(),
            Key::Named(NamedKey::Backspace) => {
                self.input.pop();
                self.dots.pop();
            }
            Key::Named(NamedKey::Escape) if matches!(self.mode, Mode::Upgrade | Mode::UpgradeConfirm { .. }) => {
                self.input.clear();
                self.dots.clear();
                self.busy = true;
                self.message = "unlocking".into();
                return Some(Submit::SkipUpgrade);
            }
            Key::Named(NamedKey::Escape) => {
                self.input.clear();
                self.dots.clear();
            }
            _ => {
                let Some(text) = &event.text else { return None };
                for c in text.chars().filter(|c| !c.is_control()) {
                    if self.input.chars().count() < MAX_LEN {
                        self.input.push(c);
                        let mut dot = Spring::new(0.0, SpringConfig::SETTLE);
                        dot.set(1.0);
                        self.dots.push(dot);
                    }
                }
                self.error = false;
            }
        }
        None
    }

    /// The passphrase worked and the TPM is usable: offer to switch to a PIN.
    pub fn offer_upgrade(&mut self) {
        self.mode = Mode::Upgrade;
        self.protection = Protection::Tpm;
        self.busy = false;
        self.error = false;
        self.message.clear();
        self.input.clear();
        self.dots.clear();
    }

    fn submit(&mut self) -> Option<Submit> {
        let min = self.protection.min_len();
        if self.input.chars().count() < min {
            self.fail(format!("at least {min} characters"));
            return None;
        }
        let input = std::mem::replace(&mut self.input, Zeroizing::new(String::new()));
        self.dots.clear();
        match std::mem::replace(&mut self.mode, Mode::Unlock) {
            Mode::Create => {
                self.mode = Mode::Confirm { first: input };
                self.message.clear();
                None
            }
            Mode::Confirm { first } => {
                if *first != *input {
                    self.mode = Mode::Create;
                    self.fail("didn't match — start over".into());
                    return None;
                }
                self.busy = true;
                self.message = "sealing".into();
                Some(Submit::Create(input))
            }
            Mode::Unlock => {
                self.busy = true;
                self.message = "unlocking".into();
                Some(Submit::Unlock(input))
            }
            Mode::Upgrade => {
                self.mode = Mode::UpgradeConfirm { first: input };
                self.message.clear();
                None
            }
            Mode::UpgradeConfirm { first } => {
                if *first != *input {
                    self.mode = Mode::Upgrade;
                    self.fail("didn't match — start over".into());
                    return None;
                }
                self.busy = true;
                self.message = "sealing".into();
                Some(Submit::Upgrade(input))
            }
        }
    }

    fn fail(&mut self, message: String) {
        self.message = message;
        self.error = true;
        self.busy = false;
        self.shake.velocity = 320.0;
    }

    /// The unlock (or create) attempt failed.
    pub fn failed(&mut self, error: &vault::Error, creating: bool) {
        if creating {
            self.mode = Mode::Create;
        }
        self.fail(error.to_string());
    }

    pub fn frame(&mut self, dt: f32, width: f32, height: f32, scene: &mut Scene) -> bool {
        let mut moving = self.shake.step(dt) | self.appear.step(dt);
        for dot in &mut self.dots {
            moving |= dot.step(dt);
        }
        if self.busy {
            self.spin = (self.spin + dt * 1.4).fract();
            moving = true;
        }
        let a = self.appear.value.clamp(0.0, 1.0);
        let cx = width * 0.5;
        let cy = height * 0.5;

        scene.logo(20.0, 23.0, 22.0, theme::TEXT.with_alpha(a));
        let w = scene.text(50.0, 34.0, "unskilled", 16.0, Weight::Medium, theme::TEXT.with_alpha(a));
        scene.circle(50.0 + w + 4.0, 38.5, 2.2, theme::ACCENT.with_alpha(a));

        // Clock, like a phone's lock screen.
        let now = clock::now();
        let size = (width / 8.0).clamp(64.0, 128.0);
        let lift = (1.0 - a) * 12.0;
        scene.text_centered(cx, cy - size * 0.75 + lift, &format!("{:02}:{:02}", now.hour, now.minute), size, Weight::Light, theme::TEXT.with_alpha(a));
        scene.text_centered(cx, cy - size * 0.75 + size * 0.62 + lift, &now.date_label(), 14.0, Weight::Regular, theme::TEXT_DIM.with_alpha(a));

        // Input pill with one dot per character.
        let pill = Rect::new(cx - 160.0 + self.shake.value, cy + 40.0, 320.0, 48.0);
        let border = if self.error { theme::ACCENT } else { theme::HAIRLINE_HOVER };
        scene.fill(pill, 24.0, theme::RAISED.with_alpha(a));
        scene.stroke(pill, 24.0, 1.0, border.with_alpha(a));
        let (px, py) = pill.center();
        let shown = self.dots.len().min(14);
        if shown == 0 {
            let hint = if self.busy { self.message.as_str() } else { self.prompt() };
            scene.text_centered(px, py, hint, 13.0, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
        } else {
            let start = px - (shown as f32 - 1.0) * DOT_GAP * 0.5;
            for (i, dot) in self.dots.iter().rev().take(shown).rev().enumerate() {
                let pop = dot.value.max(0.0);
                let pulse = if self.busy {
                    let phase = (i as f32 / shown as f32 - self.spin).rem_euclid(1.0);
                    0.35 + 0.65 * (1.0 - phase)
                } else {
                    1.0
                };
                scene.circle(start + i as f32 * DOT_GAP, py, 4.5 * pop, theme::TEXT.with_alpha(a * pulse));
            }
        }

        // Status line.
        let status = if self.error || (self.busy && shown > 0) {
            self.message.as_str()
        } else if shown > 0 {
            self.prompt()
        } else {
            ""
        };
        let color = if self.error { theme::ACCENT } else { theme::TEXT_DIM };
        scene.text_centered(cx, pill.bottom() + 28.0, status, 12.5, Weight::Regular, color.with_alpha(a));

        let note = match (self.protection, &self.mode, self.tpm_denied) {
            (_, Mode::Upgrade | Mode::UpgradeConfirm { .. }, _) => {
                "this laptop's tpm is available · a pin can replace your passphrase · esc keeps the passphrase"
            }
            (Protection::Tpm, _, _) => "sealed in this laptop's tpm · repeated wrong pins lock it",
            (Protection::Passphrase, Mode::Create | Mode::Confirm { .. }, true) => {
                "tpm found but not accessible · for a short pin: sudo usermod -aG tss $USER, log in again"
            }
            (Protection::Passphrase, _, _) => "no tpm access · protected by your passphrase alone",
        };
        scene.text_centered(cx, height - 30.0, note, 11.0, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
        moving
    }
}
