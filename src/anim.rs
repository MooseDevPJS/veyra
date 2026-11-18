//! Tiny tween system. Values ease toward a target over a duration; while any
//! tween runs the window ticks at ~60 fps, otherwise nothing is redrawn.

use std::time::{Duration, Instant};

/// Respect "Show animations in Windows" (off: transitions become instant
/// except short fades, which help orientation).
pub fn animations_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut on = windows::core::BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some((&mut on as *mut windows::core::BOOL).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    on.as_bool()
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur: Duration,
    /// Quintic ease-out: a quicker start and a longer, softer landing (big
    /// movements such as the sidebar unfolding).
    soft: bool,
}

impl Tween {
    pub fn new(value: f32) -> Self {
        Tween { from: value, to: value, start: Instant::now(), dur: Duration::ZERO, soft: false }
    }

    /// Starts moving toward `to` from wherever the value is now.
    pub fn go(&mut self, to: f32, ms: u64) {
        if (to - self.to).abs() < f32::EPSILON {
            return;
        }
        let now = Instant::now();
        self.from = self.at(now);
        self.to = to;
        self.start = now;
        self.dur = Duration::from_millis(ms);
        self.soft = false;
    }

    /// Like `go`, with the softer quintic landing.
    pub fn glide(&mut self, to: f32, ms: u64) {
        self.go(to, ms);
        self.soft = true;
    }

    pub fn set(&mut self, value: f32) {
        *self = Tween::new(value);
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    pub fn at(&self, now: Instant) -> f32 {
        if self.dur.is_zero() {
            return self.to;
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32()).min(1.0);
        // Exponential-ish ease-out: fast start, soft landing.
        let e = 1.0 - (1.0 - t).powi(if self.soft { 5 } else { 3 });
        self.from + (self.to - self.from) * e
    }

    pub fn value(&self) -> f32 {
        self.at(Instant::now())
    }

    pub fn running(&self) -> bool {
        !self.dur.is_zero() && Instant::now() < self.start + self.dur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_to_target() {
        let mut t = Tween::new(0.0);
        t.go(1.0, 100);
        let start = t.start;
        assert!(t.at(start) < 0.01);
        let mid = t.at(start + Duration::from_millis(50));
        assert!(mid > 0.5 && mid < 1.0, "ease-out is past halfway at half time: {mid}");
        assert_eq!(t.at(start + Duration::from_millis(200)), 1.0);
    }

    #[test]
    fn same_target_does_not_restart() {
        let mut t = Tween::new(0.0);
        t.go(1.0, 100);
        let start = t.start;
        t.go(1.0, 100);
        assert_eq!(t.start, start);
    }
}
