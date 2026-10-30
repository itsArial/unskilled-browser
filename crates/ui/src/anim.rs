//! Spring physics. Every moving thing in the UI is a spring, never a fixed-duration tween:
//! retargeting mid-flight keeps the current velocity, so motion never jumps.

/// Integration step. Fixed so motion is identical at 60, 120 or 144 Hz.
const STEP: f32 = 1.0 / 480.0;
/// Frames longer than this (window hidden, debugger) are clamped instead of simulated.
const MAX_FRAME: f32 = 1.0 / 20.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringConfig {
    pub stiffness: f32,
    pub damping: f32,
}

impl SpringConfig {
    /// Critically damped, quick. Default for layout.
    pub const GLIDE: Self = Self::from_response(0.32, 1.0);
    /// Slight overshoot. For things that "land": tabs appearing, the active pill.
    pub const SETTLE: Self = Self::from_response(0.42, 0.82);
    /// Fast, no overshoot. Hover and press feedback.
    pub const SNAP: Self = Self::from_response(0.16, 1.0);

    /// `response` is roughly the time in seconds to reach the target,
    /// `damping_ratio` 1.0 = no overshoot, < 1.0 = bounce.
    pub const fn from_response(response: f32, damping_ratio: f32) -> Self {
        let omega = std::f32::consts::TAU / response;
        Self {
            stiffness: omega * omega,
            damping: 2.0 * damping_ratio * omega,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub config: SpringConfig,
}

impl Spring {
    pub fn new(value: f32, config: SpringConfig) -> Self {
        Self { value, velocity: 0.0, target: value, config }
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Jump without animating.
    pub fn snap_to(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    pub fn is_settled(&self) -> bool {
        let scale = self.target.abs().max(1.0);
        (self.value - self.target).abs() < 0.001 * scale && self.velocity.abs() < 0.01 * scale
    }

    /// Advances the simulation. Returns `true` while still moving.
    pub fn step(&mut self, dt: f32) -> bool {
        if self.is_settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        let mut remaining = dt.min(MAX_FRAME);
        while remaining > 0.0 {
            let h = remaining.min(STEP);
            // Semi-implicit Euler: stable for stiff springs at this step size.
            let force = -self.config.stiffness * (self.value - self.target)
                - self.config.damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
            remaining -= h;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critically_damped_never_overshoots() {
        let mut s = Spring::new(0.0, SpringConfig::GLIDE);
        s.set(100.0);
        for _ in 0..240 {
            s.step(1.0 / 120.0);
            assert!(s.value <= 100.0 + 1e-3, "overshot to {}", s.value);
        }
        assert!(s.is_settled());
    }

    #[test]
    fn frame_rate_independent() {
        let run = |hz: f32| {
            let mut s = Spring::new(0.0, SpringConfig::SETTLE);
            s.set(1.0);
            for _ in 0..(hz * 0.25) as usize {
                s.step(1.0 / hz);
            }
            s.value
        };
        assert!((run(60.0) - run(120.0)).abs() < 0.01);
    }

    #[test]
    fn retarget_keeps_velocity() {
        let mut s = Spring::new(0.0, SpringConfig::GLIDE);
        s.set(100.0);
        s.step(0.05);
        let v = s.velocity;
        s.set(-100.0);
        assert_eq!(s.velocity, v);
    }
}
