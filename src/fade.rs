use std::time::{Duration, Instant};

pub struct VolumeFade {
    from: f32,
    current: f32,
    target: u8,
    duration: Duration,
    started: Instant,
    last_reported: u8,
}

impl VolumeFade {
    pub fn new(target: u8, duration: Duration) -> Self {
        let initial = if duration.is_zero() { target } else { 0 };
        Self {
            from: f32::from(initial),
            current: f32::from(initial),
            target,
            duration,
            started: Instant::now(),
            last_reported: initial,
        }
    }

    pub fn set_target(&mut self, target: u8) {
        if self.target == target {
            return;
        }
        self.from = self.current;
        self.target = target;
        self.started = Instant::now();
    }

    pub fn tick(&mut self) -> bool {
        self.tick_at(Instant::now())
    }

    fn tick_at(&mut self, now: Instant) -> bool {
        let progress = if self.duration.is_zero() {
            1.0
        } else {
            (now.saturating_duration_since(self.started).as_secs_f32()
                / self.duration.as_secs_f32())
            .clamp(0.0, 1.0)
        };
        self.current = self.from + (f32::from(self.target) - self.from) * progress;
        let volume = self.volume();
        let changed = volume != self.last_reported;
        self.last_reported = volume;
        changed
    }

    pub fn volume(&self) -> u8 {
        self.current.round_ties_even() as u8
    }

    pub fn is_complete(&self) -> bool {
        self.current == f32::from(self.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FADE: Duration = Duration::from_millis(500);

    #[test]
    fn reaches_full_volume_in_half_a_second() {
        let mut fade = VolumeFade::new(60, TEST_FADE);
        let started = fade.started;
        fade.tick_at(started + Duration::from_millis(250));
        assert!(!fade.is_complete());
        assert_eq!(30, fade.volume());
        fade.tick_at(started + Duration::from_millis(499));
        assert!(!fade.is_complete());
        fade.tick_at(started + Duration::from_millis(500));
        assert!(fade.is_complete());
        assert_eq!(60, fade.volume());
    }

    #[test]
    fn fades_out_to_zero_in_half_a_second() {
        let mut fade = VolumeFade::new(79, TEST_FADE);
        let started = fade.started;
        fade.tick_at(started + TEST_FADE);
        fade.set_target(0);
        let stopped = fade.started;
        fade.tick_at(stopped + Duration::from_millis(250));
        assert!(fade.volume() > 0);
        fade.tick_at(stopped + TEST_FADE);
        assert!(fade.is_complete());
        assert_eq!(0, fade.volume());
    }

    #[test]
    fn route_activity_can_reverse_an_active_fade() {
        let mut fade = VolumeFade::new(60, TEST_FADE);
        let started = fade.started;
        fade.tick_at(started + Duration::from_millis(250));
        assert_eq!(30, fade.volume());
        fade.set_target(0);
        let stopped = fade.started;
        fade.tick_at(stopped + Duration::from_millis(250));
        assert_eq!(15, fade.volume());
        fade.set_target(60);
        let resumed = fade.started;
        fade.tick_at(resumed + TEST_FADE);
        assert_eq!(60, fade.volume());
    }

    #[test]
    fn zero_duration_disables_fading() {
        let mut fade = VolumeFade::new(60, Duration::ZERO);
        assert_eq!(60, fade.volume());
        assert!(fade.is_complete());
        assert!(!fade.tick());
        fade.set_target(0);
        fade.tick();
        assert_eq!(0, fade.volume());
        assert!(fade.is_complete());
    }
}
