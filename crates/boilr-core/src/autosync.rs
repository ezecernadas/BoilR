//! Settings for automatic sync, the `[autosync]` block of the config file.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Below this, the prompt would restart Steam before anyone could read it.
pub const MIN_COUNTDOWN_SECONDS: u64 = 10;

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
    /// Time between two checks. A hand-edited 0 would make the check loop spin.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_minutes.max(1).saturating_mul(60))
    }

    /// Time between the prompt appearing and Steam being restarted.
    pub fn countdown(&self) -> Duration {
        Duration::from_secs(self.countdown_seconds.max(MIN_COUNTDOWN_SECONDS))
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
