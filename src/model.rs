use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Track {
    pub id: String, // YouTube videoId, or "local:<path>" for a saved file
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub duration: u32,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub thumb: String,
}

pub fn fmt(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

pub fn fmt_span(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{:.1} h", s as f64 / 3600.0)
    }
}

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("{n} {word}") } else { format!("{n} {word}s") }
}

pub fn split_artists(text: &str) -> Vec<String> {
    text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

static RE_BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\(\[].*?[\)\]]").unwrap());
static RE_DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s[-\u{2013}\u{2014}]\s.*$").unwrap());
static RE_FEAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(feat|ft)\b\.?.*$").unwrap());
static RE_FEAT_CI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s+(feat|ft)\b\.?.*$").unwrap());
static RE_NONWORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w]+").unwrap());
static RE_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// 'Song (feat. X) - Remastered' -> 'Song' (used to look up lyrics)
pub fn clean_title(title: &str) -> String {
    let t = RE_BRACKET.replace_all(title, " ");
    let t = RE_DASH.replace(&t, "");
    let t = RE_FEAT_CI.replace(&t, "");
    RE_SPACE.replace_all(&t, " ").trim().to_string()
}

/// 'Song (feat. X) - Remastered' / 'Song [Live]' -> 'song'
pub fn norm_title(title: &str) -> String {
    let t = RE_BRACKET.replace_all(&title.to_lowercase(), " ").into_owned();
    let t = RE_DASH.replace(&t, "").into_owned();
    let t = RE_FEAT.replace(&t, "").into_owned();
    RE_NONWORD.replace_all(&t, " ").trim().to_string()
}

pub fn song_key(title: &str, artist: &str) -> String {
    let names = split_artists(artist);
    format!("{}|{}", norm_title(title), names.first().map(|n| n.to_lowercase()).unwrap_or_default())
}

/// Learned artist scores plus the 'never' list, used to filter songs.
#[derive(Clone, Default)]
pub struct Scores {
    pub artists: HashMap<String, f64>,
    pub banned: HashSet<String>,
}

impl Scores {
    /// Average score of the artists on a song (0 = unknown). A banned artist gives -99.
    pub fn affinity(&self, artist: &str) -> f64 {
        let names = split_artists(artist);
        if names.iter().any(|n| self.banned.contains(&n.to_lowercase())) {
            return -99.0;
        }
        if names.is_empty() {
            return 0.0;
        }
        names.iter().map(|n| self.artists.get(n).copied().unwrap_or(0.0)).sum::<f64>() / names.len() as f64
    }
}

pub const LOUDNESS_TARGET: f64 = -14.0;

pub fn loudness_gain_db(lufs: f64) -> f64 {
    (LOUDNESS_TARGET - lufs).clamp(-15.0, 6.0)
}
