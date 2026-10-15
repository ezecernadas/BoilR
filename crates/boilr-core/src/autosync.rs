//! Settings for automatic sync, the `[autosync]` block of the config file.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Below this, the prompt would restart Steam before anyone could read it.
pub const MIN_COUNTDOWN_SECONDS: u64 = 10;
/// A day: past it, autosync would effectively be off.
pub const MAX_INTERVAL_MINUTES: u64 = 1440;
/// An hour: past it, the prompt would sit on screen forever.
pub const MAX_COUNTDOWN_SECONDS: u64 = 3600;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AutosyncSettings {
    pub enabled: bool,
    pub interval_minutes: u64,
    pub countdown_seconds: u64,
    pub start_with_windows: bool,
}

impl Default for AutosyncSettings {
    /// The same values as the `[autosync]` block of `defaultconfig.toml`.
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 5,
            countdown_seconds: 60,
            start_with_windows: true,
        }
    }
}

impl AutosyncSettings {
    /// Time between two checks, 1 minute to a day. A hand-edited 0 would make the check
    /// loop spin.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_minutes.clamp(1, MAX_INTERVAL_MINUTES) * 60)
    }

    /// Time between the prompt appearing and Steam being restarted, 10 s to an hour.
    pub fn countdown(&self) -> Duration {
        Duration::from_secs(
            self.countdown_seconds
                .clamp(MIN_COUNTDOWN_SECONDS, MAX_COUNTDOWN_SECONDS),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_zero_interval_still_waits_a_minute() {
        let settings = AutosyncSettings {
            interval_minutes: 0,
            ..AutosyncSettings::default()
        };
        assert_eq!(settings.interval(), Duration::from_secs(60));
    }

    #[test]
    fn a_huge_interval_is_capped_at_a_day() {
        let settings = AutosyncSettings {
            interval_minutes: u64::MAX,
            ..AutosyncSettings::default()
        };
        assert_eq!(
            settings.interval(),
            Duration::from_secs(MAX_INTERVAL_MINUTES * 60)
        );
    }

    #[test]
    fn a_huge_countdown_is_capped_at_an_hour() {
        let settings = AutosyncSettings {
            countdown_seconds: u64::MAX,
            ..AutosyncSettings::default()
        };
        assert_eq!(
            settings.countdown(),
            Duration::from_secs(MAX_COUNTDOWN_SECONDS)
        );
    }

    #[test]
    fn values_inside_the_bounds_are_kept() {
        let settings = AutosyncSettings {
            interval_minutes: 1440,
            countdown_seconds: 3600,
            ..AutosyncSettings::default()
        };
        assert_eq!(settings.interval(), Duration::from_secs(1440 * 60));
        assert_eq!(settings.countdown(), Duration::from_secs(3600));
    }

    #[test]
    fn a_short_countdown_is_raised_to_the_minimum() {
        let settings = AutosyncSettings {
            countdown_seconds: 0,
            ..AutosyncSettings::default()
        };
        assert_eq!(
            settings.countdown(),
            Duration::from_secs(MIN_COUNTDOWN_SECONDS)
        );
    }
}
