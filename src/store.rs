use crate::model::Track;
use indexmap::IndexMap;
use lofty::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::UNIX_EPOCH,
};

pub const THEMES: [(&str, [u8; 3]); 13] = [
    ("blue", [10, 132, 255]), ("cyan", [50, 212, 224]), ("teal", [31, 199, 160]),
    ("green", [48, 209, 88]), ("lime", [182, 232, 46]), ("yellow", [255, 240, 74]),
    ("amber", [255, 176, 0]), ("orange", [255, 122, 47]), ("red", [255, 69, 58]),
    ("pink", [255, 110, 199]), ("purple", [191, 90, 242]), ("indigo", [128, 128, 255]),
    ("white", [232, 232, 232]),
];
pub const CROSSFADES: [f64; 5] = [0.0, 1.0, 3.0, 6.0, 10.0];
pub const SLEEP_MINUTES: [u32; 5] = [0, 15, 30, 45, 60];
pub const COVER_MODES: [&str; 3] = ["blue", "color", "off"];

pub const EQ_PRESETS: [(&str, [f32; 10]); 6] = [
    ("flat", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("bass", [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("treble", [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 5.0, 6.0, 6.0]),
    ("vocal", [-2.0, -2.0, 0.0, 3.0, 5.0, 5.0, 3.0, 1.0, 0.0, -1.0]),
    ("rock", [5.0, 3.0, 1.0, -2.0, -3.0, -1.0, 2.0, 4.0, 5.0, 5.0]),
    ("dance", [5.0, 4.0, 1.0, 0.0, -2.0, 2.0, 1.0, 1.0, 4.0, 5.0]),
];

/// Write JSON through a temp file so a crash never leaves a half-written file.
pub fn write_json<T: Serialize>(path: &Path, value: &T) {
    let tmp = path.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string(value) {
        if fs::write(&tmp, s).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

// ---------------------------------------------------------------- settings
#[derive(Clone, Serialize, Deserialize)]
pub struct Eq {
    pub preamp: f32,
    pub bands: Vec<f32>,
    pub preset: String,
}

impl Default for Eq {
    fn default() -> Self {
        Eq { preamp: 0.0, bands: vec![0.0; 10], preset: "flat".into() }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub eq: Eq,
    pub loudness: bool,
    pub theme: String,
    pub cover: String,
    pub crossfade: f64,
    pub offline: bool,
    #[serde(skip)]
    path: PathBuf,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            eq: Eq::default(), loudness: false, theme: "blue".into(), cover: "blue".into(),
            crossfade: 0.0, offline: false, path: PathBuf::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut s: Settings = read_json(path).unwrap_or_default();
        s.path = path.to_path_buf();
        if s.eq.bands.len() != 10 {
            s.eq = Eq::default();
        }
        if !THEMES.iter().any(|(n, _)| *n == s.theme) {
            s.theme = "blue".into();
        }
        if !COVER_MODES.contains(&s.cover.as_str()) {
            s.cover = "blue".into();
        }
        if !CROSSFADES.contains(&s.crossfade) {
            s.crossfade = 0.0;
        }
        s
    }
    pub fn save(&self) {
        write_json(&self.path, self);
    }
}

// --------------------------------------------------------------- playlists
pub struct Playlists {
    path: PathBuf,
    pub data: IndexMap<String, Vec<Track>>,
}

impl Playlists {
    pub fn load(path: &Path) -> Self {
        Playlists { path: path.to_path_buf(), data: read_json(path).unwrap_or_default() }
    }
    fn save(&self) {
        write_json(&self.path, &self.data);
    }
    /// New playlist; an empty name becomes 'Playlist 1', 'Playlist 2'...
    pub fn create(&mut self, name: &str, tracks: &[Track]) -> String {
        let mut name: String = name.split_whitespace().collect::<Vec<_>>().join(" ");
        name.truncate(40);
        if name.is_empty() {
            let mut n = 1;
            while self.data.contains_key(&format!("Playlist {n}")) {
                n += 1;
            }
            name = format!("Playlist {n}");
        }
        let base = name.clone();
        let mut n = 2;
        while self.data.contains_key(&name) {
            name = format!("{base} ({n})");
            n += 1;
        }
        self.data.insert(name.clone(), vec![]);
        self.add(&name, tracks);
        name
    }
    pub fn add(&mut self, name: &str, tracks: &[Track]) -> usize {
        let Some(list) = self.data.get_mut(name) else { return 0 };
        let mut added = 0;
        for t in tracks {
            if !list.iter().any(|x| x.id == t.id) {
                list.push(t.clone());
                added += 1;
            }
        }
        self.save();
        added
    }
    pub fn remove_at(&mut self, name: &str, i: usize) {
        if let Some(list) = self.data.get_mut(name) {
            if i < list.len() {
                list.remove(i);
                self.save();
            }
        }
    }
    pub fn delete(&mut self, name: &str) {
        self.data.shift_remove(name);
        self.save();
    }
    pub fn export_m3u(&self, name: &str, folder: &Path) -> std::io::Result<PathBuf> {
        fs::create_dir_all(folder)?;
        let safe: String = name.chars().filter(|c| !"\\/:*?\"<>|".contains(*c) && !c.is_control()).collect();
        let safe = safe.trim_matches(|c| c == ' ' || c == '.');
        let path = folder.join(format!("{}.m3u", if safe.is_empty() { "playlist" } else { safe }));
        let mut out = String::from("#EXTM3U\n");
        for t in self.data.get(name).into_iter().flatten() {
            out += &format!("#EXTINF:{},{} - {}\n", t.duration, t.artist, t.title);
            match t.id.strip_prefix("local:") {
                Some(p) => out += p,
                None => out += &format!("https://music.youtube.com/watch?v={}", t.id),
            }
            out.push('\n');
        }
        fs::write(&path, out)?;
        Ok(path)
    }
}

// ----------------------------------------------------------------- library
const LIB_EXT: [&str; 9] = ["m4a", "mp4", "opus", "webm", "mp3", "ogg", "flac", "wav", "aac"];

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct LibRec {
    mtime: f64,
    size: u64,
    title: String,
    artist: String,
    album: String,
    duration: u32,
}

fn probe_tags(path: &Path) -> (Option<String>, Option<String>, Option<String>, u32) {
    let Ok(file) = lofty::read_from_path(path) else { return (None, None, None, 0) };
    let secs = file.properties().duration().as_secs() as u32;
    let tag = file.primary_tag().or_else(|| file.first_tag());
    let get = |f: fn(&lofty::tag::Tag) -> Option<std::borrow::Cow<'_, str>>| {
        tag.and_then(|t| f(t)).map(|s| s.into_owned()).filter(|s| !s.is_empty())
    };
    (get(|t| t.title()), get(|t| t.artist()), get(|t| t.album()), secs)
}

/// Your saved songs as Tracks (id 'local:<path>'), sorted by artist then title.
/// Tags are read once per file and remembered in library.json.
pub fn scan_library(folder: &Path, cache_path: &Path) -> Vec<Track> {
    let cache: IndexMap<String, LibRec> = read_json(cache_path).unwrap_or_default();
    let mut seen: IndexMap<String, LibRec> = IndexMap::new();
    let mut tracks = vec![];
    let mut names: Vec<String> = fs::read_dir(folder)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    for name in names {
        let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if !LIB_EXT.contains(&ext.as_str()) {
            continue;
        }
        let path = folder.join(&name);
        let Ok(meta) = fs::metadata(&path) else { continue };
        let mtime = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let rec = match cache.get(&name) {
            Some(r) if r.mtime == mtime && r.size == meta.len() => r.clone(),
            _ => {
                let (title, artist, album, secs) = probe_tags(&path);
                let stem = Path::new(&name).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let (file_artist, file_title) = match stem.split_once(" - ") {
                    Some((a, t)) => (a.to_string(), t.to_string()),
                    None => (String::new(), stem),
                };
                LibRec {
                    mtime, size: meta.len(), title: title.unwrap_or(file_title),
                    artist: artist.unwrap_or(file_artist), album: album.unwrap_or_default(), duration: secs,
                }
            }
        };
        tracks.push(Track {
            id: format!("local:{}", path.display()), title: rec.title.clone(), artist: rec.artist.clone(),
            duration: rec.duration, album: rec.album.clone(), thumb: String::new(),
        });
        seen.insert(name, rec);
    }
    if seen != cache {
        write_json(cache_path, &seen);
    }
    tracks.sort_by_key(|t| (t.artist.to_lowercase(), t.title.to_lowercase()));
    tracks
}

// ---------------------------------------------------------------- loudness
/// Measured loudness (LUFS) of songs, remembered in loudness.json.
pub struct Loudness {
    path: PathBuf,
    map: Mutex<IndexMap<String, f64>>,
}

impl Loudness {
    pub fn load(path: &Path) -> Self {
        Loudness { path: path.to_path_buf(), map: Mutex::new(read_json(path).unwrap_or_default()) }
    }
    pub fn get(&self, key: &str) -> Option<f64> {
        self.map.lock().unwrap().get(key).copied()
    }
    pub fn get_or_measure(&self, key: &str, file: &Path) -> Option<f64> {
        if let Some(v) = self.get(key) {
            return Some(v);
        }
        let v = crate::ytm::measure_loudness(file)?;
        let mut m = self.map.lock().unwrap();
        m.insert(key.to_string(), v);
        while m.len() > 3000 {
            m.shift_remove_index(0);
        }
        write_json(&self.path, &*m);
        Some(v)
    }
}
