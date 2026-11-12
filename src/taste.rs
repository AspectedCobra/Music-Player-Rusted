use crate::model::*;
use crate::store::write_json;
use indexmap::IndexMap;
use rand::{distributions::WeightedIndex, prelude::*};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_TRACKS: usize = 500;

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Rec {
    #[serde(default)] pub title: String,
    #[serde(default)] pub artist: String,
    #[serde(default)] pub album: String,
    #[serde(default)] pub thumb: String,
    #[serde(default)] pub duration: u32,
    #[serde(default)] pub score: f64,
    #[serde(default)] pub plays: u32,
    #[serde(default)] pub skips: u32,
    #[serde(default)] pub saved: bool,
    #[serde(default)] pub last: f64,
    #[serde(default)] pub liked: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Data {
    #[serde(default)] tracks: IndexMap<String, Rec>,
    #[serde(default)] artists: IndexMap<String, f64>,
    #[serde(default)] heard: IndexMap<String, String>,
    #[serde(default)] banned: Vec<String>,
    #[serde(default)] log: Vec<(i64, String, String, i64)>, // [time, id, artist, seconds listened]
}

pub struct Stats {
    pub today: (i64, usize),
    pub week: (i64, usize),
    pub all: (i64, usize),
    pub top_artists: Vec<(String, i64)>,
    pub skipped: Vec<(String, u32)>,
    pub top_songs: Vec<(u32, String, String)>,
}

/// What the app has learned from your listening (taste.json). Same file format as the Python version.
pub struct Taste {
    path: PathBuf,
    d: Data,
}

impl Taste {
    pub fn load(path: &Path) -> Self {
        let d: Data = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let mut t = Taste { path: path.to_path_buf(), d };
        for (vid, r) in t.d.tracks.iter() {
            t.d.heard.entry(vid.clone()).or_insert_with(|| song_key(&r.title, &r.artist));
        }
        t.d.banned = t.d.banned.iter().map(|b| b.to_lowercase()).collect();
        t
    }

    fn save(&self) {
        write_json(&self.path, &self.d);
    }

    fn as_track(id: &str, r: &Rec) -> Track {
        Track { id: id.into(), title: r.title.clone(), artist: r.artist.clone(), duration: r.duration, album: r.album.clone(), thumb: r.thumb.clone() }
    }

    fn bump(&mut self, t: &Track, delta: f64) -> &mut Rec {
        for name in split_artists(&t.artist) {
            let v = self.d.artists.get(&name).copied().unwrap_or(0.0) + delta;
            self.d.artists.insert(name, round2(v.clamp(-10.0, 10.0)));
        }
        let rec = self.d.tracks.entry(t.id.clone()).or_insert_with(|| Rec {
            title: t.title.clone(), artist: t.artist.clone(), album: t.album.clone(),
            thumb: t.thumb.clone(), duration: t.duration, ..Default::default()
        });
        rec.score = round2(rec.score + delta);
        rec.last = now();
        rec
    }

    /// Finishing or saving a song counts for it, skipping early counts against it.
    pub fn record(&mut self, t: &Track, listened: f64, length: f64, completed: bool) {
        let secs = if completed { length } else { listened };
        self.d.log.push((now() as i64, t.id.clone(), t.artist.clone(), secs.max(0.0) as i64));
        if self.d.log.len() > 8000 {
            let n = self.d.log.len() - 8000;
            self.d.log.drain(..n);
        }
        let frac = if length > 0.0 { listened / length } else { 0.0 };
        if completed || frac >= 0.9 {
            self.bump(t, 1.0).plays += 1;
        } else if frac >= 0.5 {
            self.bump(t, 0.4);
        } else if listened < 30.0 && length > 60.0 {
            self.bump(t, -1.0).skips += 1;
        } else {
            self.save();
            return;
        }
        self.prune();
        self.save();
    }

    fn prune(&mut self) {
        let n = self.d.tracks.len();
        if n <= MAX_TRACKS {
            return;
        }
        let mut order: Vec<(bool, f64, f64, String)> =
            self.d.tracks.iter().map(|(k, r)| (r.saved, r.score, r.last, k.clone())).collect();
        order.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        });
        for (_, _, _, k) in order.into_iter().take(n - (MAX_TRACKS - 50)) {
            self.d.tracks.shift_remove(&k);
        }
    }

    pub fn stats(&self) -> Stats {
        use chrono::{Local, TimeZone};
        let now_ts = now() as i64;
        let today = Local::now().date_naive().and_hms_opt(0, 0, 0)
            .and_then(|d| Local.from_local_datetime(&d).earliest()).map(|d| d.timestamp()).unwrap_or(now_ts - 86400);
        let week = now_ts - 7 * 86400;
        let total = |since: i64| {
            let items: Vec<_> = self.d.log.iter().filter(|e| e.0 >= since).collect();
            (items.iter().map(|e| e.3).sum::<i64>(), items.len())
        };
        let lead = |a: &str| split_artists(a).into_iter().next().unwrap_or_else(|| "unknown".into());
        let mut per: IndexMap<String, i64> = IndexMap::new();
        for (ts, _, artist, secs) in &self.d.log {
            if *ts >= week {
                *per.entry(lead(artist)).or_insert(0) += secs;
            }
        }
        let mut skipped: IndexMap<String, u32> = IndexMap::new();
        for r in self.d.tracks.values().filter(|r| r.skips > 0) {
            *skipped.entry(lead(&r.artist)).or_insert(0) += r.skips;
        }
        let mut songs: Vec<(u32, String, String)> =
            self.d.tracks.values().filter(|r| r.plays > 0).map(|r| (r.plays, r.title.clone(), r.artist.clone())).collect();
        songs.sort_by(|a, b| b.cmp(a));
        let mut top: Vec<_> = per.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        let mut sk: Vec<_> = skipped.into_iter().collect();
        sk.sort_by(|a, b| b.1.cmp(&a.1));
        Stats {
            today: total(today), week: total(week), all: total(0),
            top_artists: top.into_iter().take(6).collect(), skipped: sk.into_iter().take(5).collect(),
            top_songs: songs.into_iter().take(5).collect(),
        }
    }

    /// Remember a song was played so autoplay never brings it back.
    pub fn mark_heard(&mut self, t: &Track) {
        self.d.heard.shift_remove(&t.id);
        self.d.heard.insert(t.id.clone(), song_key(&t.title, &t.artist));
        while self.d.heard.len() > 3000 {
            self.d.heard.shift_remove_index(0);
        }
        self.save();
    }
    pub fn heard_ids(&self) -> std::collections::HashSet<String> {
        self.d.heard.keys().cloned().collect()
    }
    pub fn heard_keys(&self) -> std::collections::HashSet<String> {
        self.d.heard.values().cloned().collect()
    }

    pub fn scores(&self) -> Scores {
        Scores { artists: self.d.artists.iter().map(|(k, v)| (k.clone(), *v)).collect(), banned: self.d.banned.iter().cloned().collect() }
    }

    pub fn is_liked(&self, id: &str) -> bool {
        self.d.tracks.get(id).map(|r| r.liked).unwrap_or(false)
    }
    pub fn toggle_like(&mut self, t: &Track) -> bool {
        let liked = !self.is_liked(&t.id);
        let rec = self.bump(t, if liked { 3.0 } else { -3.0 });
        rec.liked = liked;
        self.save();
        liked
    }
    pub fn is_banned(&self, artist: &str) -> bool {
        self.d.banned.contains(&artist.to_lowercase())
    }
    pub fn toggle_ban(&mut self, artist: &str) -> bool {
        let name = artist.to_lowercase();
        let banned = if let Some(i) = self.d.banned.iter().position(|b| *b == name) {
            self.d.banned.remove(i);
            false
        } else {
            self.d.banned.push(name);
            true
        };
        self.save();
        banned
    }
    pub fn banned(&self) -> Vec<String> {
        let mut v = self.d.banned.clone();
        v.sort();
        v
    }
    pub fn record_save(&mut self, t: &Track) {
        self.bump(t, 3.0).saved = true;
        self.save();
    }

    pub fn favourites(&self) -> Vec<Track> {
        let s = self.scores();
        self.d.tracks.iter()
            .filter(|(_, r)| (r.saved || r.score >= 2.0) && s.affinity(&r.artist) > -2.0)
            .map(|(k, r)| Self::as_track(k, r)).collect()
    }

    /// Songs you finished or saved, with a weight (saved/liked ones weigh most).
    pub fn liked(&self) -> Vec<(Track, f64)> {
        let s = self.scores();
        self.d.tracks.iter()
            .filter(|(_, r)| (r.score > 0.0 || r.saved) && s.affinity(&r.artist) > -2.0)
            .map(|(k, r)| (Self::as_track(k, r), r.score.max(0.0) + if r.saved || r.liked { 2.0 } else { 0.0 }))
            .collect()
    }

    pub fn liked_tracks(&self) -> Vec<Track> {
        let mut v: Vec<(&String, &Rec)> = self.d.tracks.iter().filter(|(_, r)| r.liked).collect();
        v.sort_by(|a, b| b.1.last.partial_cmp(&a.1.last).unwrap_or(std::cmp::Ordering::Equal));
        v.into_iter().map(|(k, r)| Self::as_track(k, r)).collect()
    }

    pub fn top_artists(&self, n: usize) -> Vec<(String, f64)> {
        let mut v: Vec<(String, f64)> = self.d.artists.iter()
            .filter(|(k, v)| **v > 0.0 && !self.d.banned.contains(&k.to_lowercase())).map(|(k, v)| (k.clone(), *v)).collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(n);
        v
    }

    pub fn pick_seeds(&self, n: usize) -> Vec<Track> {
        let s = self.scores();
        let liked: Vec<(&String, &Rec)> =
            self.d.tracks.iter().filter(|(_, r)| r.score > 0.0 && s.affinity(&r.artist) > -2.0).collect();
        if liked.is_empty() {
            return vec![];
        }
        let Ok(dist) = WeightedIndex::new(liked.iter().map(|(_, r)| r.score)) else { return vec![] };
        let mut rng = thread_rng();
        let mut picks: IndexMap<String, Track> = IndexMap::new();
        for _ in 0..n * 3 {
            let (k, r) = liked[dist.sample(&mut rng)];
            picks.insert(k.clone(), Self::as_track(k, r));
            if picks.len() >= n {
                break;
            }
        }
        picks.into_values().collect()
    }
}
