//! Small shared types.

use serde::{Deserialize, Serialize};

use crate::i18n::Strings;

/// What the user wants out of a link.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Audio,
}

/// Preferred audio track language.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioLang {
    Ru,
    En,
}

impl AudioLang {
    pub const ALL: [AudioLang; 2] = [AudioLang::Ru, AudioLang::En];

    pub fn code(self) -> &'static str {
        match self {
            AudioLang::Ru => "ru",
            AudioLang::En => "en",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "ru" => Some(AudioLang::Ru),
            "en" => Some(AudioLang::En),
            _ => None,
        }
    }

    pub fn button(self) -> &'static str {
        match self {
            AudioLang::Ru => "🇷🇺 RU",
            AudioLang::En => "🇺🇸 EN",
        }
    }
}

/// A quality choice: what the picker buttons and `/quality` offer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// Best that fits the limits.
    Max,
    /// At most this many pixels high.
    Height(u32),
    Audio,
}

impl Quality {
    pub const HEIGHTS: [u32; 5] = [1440, 1080, 720, 480, 360];

    /// Approximate bitrate in bytes per second for a given height (video + audio stream).
    pub fn estimated_bytes_per_second(height: u32) -> u64 {
        match height {
            h if h >= 2160 => 3_000_000, // 4K: ~24 Mbps = 3.0 MB/s
            h if h >= 1440 => 1_500_000, // 2K (1440p): ~12 Mbps = 1.5 MB/s
            h if h >= 1080 => 600_000,   // 1080p: ~4.8 Mbps = 600 KB/s
            h if h >= 720 => 275_000,    // 720p: ~2.2 Mbps = 275 KB/s
            h if h >= 480 => 125_000,    // 480p: ~1.0 Mbps = 125 KB/s
            _ => 65_000,                 // 360p / lower: ~520 kbps = 65 KB/s
        }
    }

    /// Estimated size in bytes for a video of `duration_secs` at `height`.
    pub fn estimate_size_bytes(height: u32, duration_secs: u64) -> u64 {
        duration_secs.saturating_mul(Self::estimated_bytes_per_second(height))
    }

    /// Heights from `Quality::HEIGHTS` that are `<= max_height` and whose estimated size
    /// for `duration_secs` does not exceed `max_bytes`.
    /// When `duration_secs` is None or 0, only checks `max_height`.
    pub fn heights_for_duration(max_height: u32, duration_secs: Option<u64>, max_bytes: u64) -> Vec<u32> {
        Self::HEIGHTS
            .into_iter()
            .filter(|&h| h <= max_height)
            .filter(|&h| {
                if let Some(d) = duration_secs.filter(|&d| d > 0) {
                    Self::estimate_size_bytes(h, d) <= max_bytes
                } else {
                    true
                }
            })
            .collect()
    }

    pub fn code(self) -> String {
        match self {
            Quality::Max => "max".to_string(),
            Quality::Height(h) => h.to_string(),
            Quality::Audio => "audio".to_string(),
        }
    }

    pub fn parse(code: &str) -> Option<Quality> {
        match code {
            "max" => Some(Quality::Max),
            "audio" => Some(Quality::Audio),
            h => h.parse::<u32>().ok().filter(|h| (144..=4320).contains(h)).map(Quality::Height),
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Quality::Audio => Kind::Audio,
            _ => Kind::Video,
        }
    }

    pub fn cap(self) -> Option<u32> {
        match self {
            Quality::Height(h) => Some(h),
            _ => None,
        }
    }

    pub fn label(self, t: &Strings) -> String {
        match self {
            Quality::Max => t.btn_max.to_string(),
            Quality::Height(h) => format!("{h}p"),
            Quality::Audio => t.btn_audio.to_string(),
        }
    }
}

/// Where a job currently is; rendered into the status message.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Queued,
    Waiting,
    Probing,
    Downloading {
        pct: Option<f32>,
        /// bytes per second
        speed: Option<f64>,
        /// seconds left
        eta: Option<u64>,
    },
    Processing,
    Transcoding {
        pct: Option<f32>,
    },
    Uploading {
        total: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_codes() {
        for q in [Quality::Max, Quality::Audio, Quality::Height(720)] {
            assert_eq!(Quality::parse(&q.code()), Some(q));
        }
        assert_eq!(Quality::parse("99999"), None);
        assert_eq!(Quality::parse("ask"), None);
        assert_eq!(Quality::Height(480).cap(), Some(480));
        assert_eq!(Quality::Audio.kind(), Kind::Audio);
    }

    #[test]
    fn quality_formula_and_heights() {
        assert_eq!(Quality::HEIGHTS, [1440, 1080, 720, 480, 360]);
        let max_2gb = 2000 * 1024 * 1024;
        // 10 minutes (600s): 1440p fits (900 MB <= 2000 MB)
        let hs_10m = Quality::heights_for_duration(1440, Some(600), max_2gb);
        assert!(hs_10m.contains(&1440));
        assert!(hs_10m.contains(&1080));

        // 30 minutes (1800s): 1440p (2700 MB) exceeds 2000 MB, 1080p (1080 MB) fits
        let hs_30m = Quality::heights_for_duration(1440, Some(1800), max_2gb);
        assert!(!hs_30m.contains(&1440));
        assert!(hs_30m.contains(&1080));
        assert!(hs_30m.contains(&720));

        // 70 minutes (4200s): 1080p (2520 MB) exceeds 2000 MB, 720p (1155 MB) fits
        let hs_70m = Quality::heights_for_duration(1440, Some(4200), max_2gb);
        assert!(!hs_70m.contains(&1440));
        assert!(!hs_70m.contains(&1080));
        assert!(hs_70m.contains(&720));

        // 3 hours (10800s): 720p (2970 MB) exceeds 2000 MB, 480p fits
        let hs_3h = Quality::heights_for_duration(1440, Some(10800), max_2gb);
        assert!(!hs_3h.contains(&1440));
        assert!(!hs_3h.contains(&1080));
        assert!(!hs_3h.contains(&720));
        assert!(hs_3h.contains(&480));
        assert!(hs_3h.contains(&360));

        // None duration: keeps all up to max_height
        assert_eq!(Quality::heights_for_duration(1440, None, max_2gb), Quality::HEIGHTS);
    }
}
