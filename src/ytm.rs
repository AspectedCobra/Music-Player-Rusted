use crate::model::*;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0";

static CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
    reqwest::blocking::Client::builder().user_agent(UA).timeout(Duration::from_secs(15)).build().unwrap()
});

/// A child process that does not flash a console window on Windows.
pub fn command(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

pub fn http_bytes(url: &str) -> Result<Vec<u8>, String> {
    let r = CLIENT.get(url).send().map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?;
    r.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
}

// ------------------------------------------------- YouTube Music (InnerTube)
fn innertube(endpoint: &str, mut body: Value) -> Result<Value, String> {
    body["context"] = json!({"client": {"clientName": "WEB_REMIX", "clientVersion": "1.20241127.01.00", "hl": "en", "gl": "US"}});
    CLIENT
        .post(format!("https://music.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"))
        .header("Origin", "https://music.youtube.com")
        .header("Cookie", "SOCS=CAI")
        .json(&body)
        .send().map_err(|e| e.to_string())?
        .error_for_status().map_err(|e| e.to_string())?
        .json::<Value>().map_err(|e| e.to_string())
}

fn find_all<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == key { out.push(x) } else { find_all(x, key, out) }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| find_all(x, key, out)),
        _ => {}
    }
}

fn text_of(v: &Value) -> String {
    v["text"].as_str().unwrap_or("").to_string()
}
/// Split a list of text runs on the ' • ' separators.
fn segments(runs: &[Value]) -> Vec<Vec<&Value>> {
    let mut out: Vec<Vec<&Value>> = vec![vec![]];
    for r in runs {
        if r["text"].as_str() == Some(" • ") { out.push(vec![]) } else { out.last_mut().unwrap().push(r) }
    }
    out
}
fn seg_text(seg: &[&Value]) -> String {
    seg.iter().map(|r| text_of(r)).collect()
}
/// Artists are the linked runs ('A', 'B' -> 'A, B'); plain text if nothing is linked.
fn seg_artists(seg: &[&Value]) -> String {
    let linked: Vec<String> = seg.iter().filter(|r| r.get("navigationEndpoint").is_some()).map(|r| text_of(r)).collect();
    if linked.is_empty() { seg_text(seg) } else { linked.join(", ") }
}
fn parse_dur(s: &str) -> Option<u32> {
    if !s.contains(':') {
        return None;
    }
    let mut t = 0u32;
    for p in s.split(':') {
        t = t * 60 + p.trim().parse::<u32>().ok()?;
    }
    Some(t)
}
fn best_thumb(thumbs: &Value) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"=w\d+-h\d+[^/]*$").unwrap());
    let url = thumbs.as_array().and_then(|a| a.last()).and_then(|t| t["url"].as_str()).unwrap_or("");
    RE.replace(url, "=w600-h600-l90-rj").into_owned()
}

fn parse_search_item(r: &Value) -> Option<Track> {
    let id = r["playlistItemData"]["videoId"].as_str()?.to_string();
    let cols = r["flexColumns"].as_array()?;
    let runs = |i: usize| -> Vec<Value> {
        cols.get(i).and_then(|c| c["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"].as_array()).cloned().unwrap_or_default()
    };
    let title = text_of(runs(0).first()?);
    let second = runs(1);
    let mut segs = segments(&second);
    if segs.len() > 1 && matches!(seg_text(&segs[0]).as_str(), "Song" | "Video") {
        segs.remove(0);
    }
    let artist = seg_artists(&segs[0]);
    let duration = segs.last().and_then(|s| parse_dur(&seg_text(s))).unwrap_or(0);
    let album = segs.get(1).map(|s| seg_text(s)).filter(|s| parse_dur(s).is_none()).unwrap_or_default();
    let thumb = best_thumb(&r["thumbnail"]["musicThumbnailRenderer"]["thumbnail"]["thumbnails"]);
    Some(Track { id, title, artist, duration, album, thumb })
}

fn parse_radio_item(p: &Value) -> Option<Track> {
    let id = p["videoId"].as_str()?.to_string();
    let title = text_of(p["title"]["runs"].as_array()?.first()?);
    let byline = p["longBylineText"]["runs"].as_array().cloned().unwrap_or_default();
    let segs = segments(&byline);
    let artist = segs.first().map(|s| seg_artists(s)).unwrap_or_default();
    let album = segs.get(1).map(|s| seg_text(s)).unwrap_or_default();
    let duration = p["lengthText"]["runs"].as_array().and_then(|a| a.first()).and_then(|r| parse_dur(&text_of(r))).unwrap_or(0);
    Some(Track { id, title, artist, duration, album, thumb: best_thumb(&p["thumbnail"]["thumbnails"]) })
}

pub fn search(query: &str, limit: usize) -> Result<Vec<Track>, String> {
    let data = innertube("search", json!({"query": query, "params": "EgWKAQIIAWoKEAkQBRAKEAMQBA=="}))?;
    let mut items = vec![];
    find_all(&data, "musicResponsiveListItemRenderer", &mut items);
    let mut out: Vec<Track> = vec![];
    for t in items.into_iter().filter_map(parse_search_item) {
        if !out.iter().any(|x| x.id == t.id) {
            out.push(t);
        }
    }
    out.truncate(limit);
    Ok(out)
}

/// Songs YouTube Music suggests after `track` (its 'radio').
pub fn radio(track: &Track, limit: usize) -> Result<Vec<Track>, String> {
    let mut track = track.clone();
    if track.id.starts_with("local:") {
        // a saved file: find the same song on YouTube Music first
        match search(&format!("{} {}", track.artist, track.title), 1)?.into_iter().next() {
            Some(found) => track = found,
            None => return Ok(vec![]),
        }
    }
    let data = innertube("next", json!({
        "videoId": track.id, "playlistId": format!("RDAMVM{}", track.id), "params": "wAEB",
        "isAudioOnly": true, "enablePersistentPlaylistPanel": true, "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
        "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"hasPersistentPlaylistPanel": true, "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}}
    }))?;
    let mut items = vec![];
    find_all(&data, "playlistPanelVideoRenderer", &mut items);
    let mut out: Vec<Track> = vec![];
    for t in items.into_iter().filter_map(parse_radio_item) {
        if t.id != track.id && !out.iter().any(|x| x.id == t.id) {
            out.push(t);
        }
    }
    out.truncate(limit);
    Ok(out)
}

// ------------------------------------------------------------ downloading
static LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn find_cached(tmp: &Path, id: &str) -> Option<PathBuf> {
    let prefix = format!("{id}.");
    std::fs::read_dir(tmp).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| {
        let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        n.starts_with(&prefix) && !n.ends_with(".part") && !n.ends_with(".ytdl")
    })
}

/// Download the song's audio into the temp folder (once) and return its path. Safe to call
/// from several threads: a second call for the same song waits for the first.
/// Needs yt-dlp on PATH (or MUSIC_YTDLP=full\path\yt-dlp.exe).
/// Cookies: MUSIC_COOKIES_BROWSER=firefox|chrome|edge|brave... or MUSIC_COOKIES_FILE=cookies.txt
pub fn fetch(track: &Track, tmp: &Path) -> Result<PathBuf, String> {
    if let Some(p) = track.id.strip_prefix("local:") {
        return Ok(PathBuf::from(p));
    }
    let lock = LOCKS.lock().unwrap().entry(track.id.clone()).or_default().clone();
    let _guard = lock.lock().unwrap();
    if let Some(p) = find_cached(tmp, &track.id) {
        return Ok(p);
    }
    let program = std::env::var("MUSIC_YTDLP").unwrap_or_else(|_| "yt-dlp".into());
    let mut cmd = command(&program);
    cmd.args(["-f", "bestaudio[ext=m4a]/bestaudio/best", "--no-playlist", "--quiet", "--no-warnings"]);
    if let Ok(file) = std::env::var("MUSIC_COOKIES_FILE") {
        cmd.arg("--cookies").arg(file);
    } else if let Ok(b) = std::env::var("MUSIC_COOKIES_BROWSER") {
        if !b.eq_ignore_ascii_case("none") && !b.is_empty() {
            cmd.arg("--cookies-from-browser").arg(b);
        }
    }
    cmd.arg("-o").arg(tmp.join("%(id)s.%(ext)s"));
    cmd.arg(format!("https://www.youtube.com/watch?v={}", track.id));
    let out = cmd.output().map_err(|e| format!("could not run yt-dlp ({e}); is it installed and on PATH?"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("yt-dlp failed").to_string());
    }
    find_cached(tmp, &track.id).ok_or_else(|| "download finished but the file was not found".into())
}

/// Integrated loudness of (the first 90 s of) an audio file in LUFS, via ffmpeg.
pub fn measure_loudness(path: &Path) -> Option<f64> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"I:\s+(-?\d+(?:\.\d+)?)\s+LUFS").unwrap());
    let out = command("ffmpeg")
        .args(["-nostats", "-hide_banner", "-t", "90", "-i"]).arg(path)
        .args(["-vn", "-af", "ebur128=framelog=quiet", "-f", "null", "-"]).output().ok()?;
    let err = String::from_utf8_lossy(&out.stderr);
    RE.captures_iter(&err).last().and_then(|c| c[1].parse().ok())
}

/// Save the downloaded song to dest_dir as 'Artist - Title.ext' with tags and cover art (ffmpeg).
/// Returns (path, status) with status 'tagged', 'untagged' or 'exists'.
pub fn save_track(track: &Track, tmp: &Path, dest_dir: &Path) -> Result<(PathBuf, &'static str), String> {
    if let Some(p) = track.id.strip_prefix("local:") {
        return Ok((PathBuf::from(p), "exists"));
    }
    let src = find_cached(tmp, &track.id).ok_or("song file not found")?;
    let src_ext = src.extension().and_then(|e| e.to_str()).unwrap_or("m4a").to_lowercase();
    let ext = match src_ext.as_str() { "m4a" | "mp4" => "m4a", "webm" | "opus" => "opus", other => other }.to_string();
    let raw = if track.artist.is_empty() { track.title.clone() } else { format!("{} - {}", track.artist, track.title) };
    let name: String = raw.chars().filter(|c| !"\\/:*?\"<>|".contains(*c) && !c.is_control()).collect();
    let name: String = name.trim_matches(|c| c == ' ' || c == '.').chars().take(150).collect();
    let name = if name.is_empty() { track.id.clone() } else { name };
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let dest = dest_dir.join(format!("{name}.{ext}"));
    if dest.exists() {
        return Ok((dest, "exists"));
    }
    if ext == "m4a" || ext == "opus" {
        let out = tmp.join(format!("tagged.{ext}"));
        let cover = tmp.join("cover.jpg");
        let with_cover = ext == "m4a" && !track.thumb.is_empty() && {
            crate::cover::load_image(track).map(|im| crate::cover::square(&im).resize(600, 600, image::imageops::FilterType::Lanczos3).to_rgb8().save(&cover).is_ok()).unwrap_or(false)
        };
        let mut cmd = command("ffmpeg");
        cmd.args(["-y", "-loglevel", "error", "-i"]).arg(&src);
        if with_cover {
            cmd.arg("-i").arg(&cover);
        }
        cmd.args(["-map", "0:a"]);
        if with_cover {
            cmd.args(["-map", "1:v", "-disposition:v:0", "attached_pic"]);
        }
        cmd.args(["-c", "copy", "-metadata"]).arg(format!("title={}", track.title));
        if !track.artist.is_empty() {
            cmd.arg("-metadata").arg(format!("artist={}", track.artist));
        }
        if !track.album.is_empty() {
            cmd.arg("-metadata").arg(format!("album={}", track.album));
        }
        cmd.arg(&out);
        if cmd.output().map(|o| o.status.success()).unwrap_or(false) && std::fs::copy(&out, &dest).is_ok() {
            let _ = std::fs::remove_file(&out);
            return Ok((dest, "tagged"));
        }
        let _ = std::fs::remove_file(&out);
    }
    let dest = dest_dir.join(format!("{name}.{src_ext}"));
    if dest.exists() {
        return Ok((dest, "exists"));
    }
    std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
    Ok((dest, "untagged"))
}

// ------------------------------------------------------------------ lyrics
const LRCLIB: &str = "https://lrclib.net/api";

#[derive(Clone, PartialEq)]
pub enum LState { Loading, Ok, NotFound, Instrumental, Error }

#[derive(Clone)]
pub struct Lyrics {
    pub id: String,
    pub state: LState,
    pub lines: Vec<(Option<f64>, String)>,
    pub synced: bool,
}

impl Lyrics {
    pub fn new(id: &str, state: LState) -> Self {
        Lyrics { id: id.into(), state, lines: vec![], synced: false }
    }
}

fn lrclib(path: &str, query: &[(&str, String)]) -> Result<Option<Value>, String> {
    let r = CLIENT.get(format!("{LRCLIB}/{path}")).query(query).timeout(Duration::from_secs(8)).send().map_err(|e| e.to_string())?;
    if r.status().as_u16() == 404 {
        return Ok(None);
    }
    r.error_for_status().map_err(|e| e.to_string())?.json::<Value>().map(Some).map_err(|e| e.to_string())
}

fn parse_lrc(text: &str) -> Vec<(Option<f64>, String)> {
    static STAMP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(\d+):(\d+(?:\.\d+)?)\]").unwrap());
    static ANY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]").unwrap());
    let mut out = vec![];
    for line in text.lines() {
        let words = ANY.replace_all(line, "").trim().to_string();
        for c in STAMP.captures_iter(line) {
            let (m, s): (f64, f64) = (c[1].parse().unwrap_or(0.0), c[2].parse().unwrap_or(0.0));
            out.push((Some(m * 60.0 + s), words.clone()));
        }
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn lyrics_result(id: &str, d: &Value) -> Option<Lyrics> {
    if d["instrumental"].as_bool() == Some(true) {
        return Some(Lyrics::new(id, LState::Instrumental));
    }
    let synced = parse_lrc(d["syncedLyrics"].as_str().unwrap_or(""));
    if !synced.is_empty() {
        return Some(Lyrics { id: id.into(), state: LState::Ok, lines: synced, synced: true });
    }
    let plain = d["plainLyrics"].as_str().unwrap_or("").trim();
    if !plain.is_empty() {
        return Some(Lyrics { id: id.into(), state: LState::Ok, lines: plain.lines().map(|l| (None, l.trim().to_string())).collect(), synced: false });
    }
    None
}

/// Lyrics from lrclib.net (time-synced when available): exact match, then the title without
/// '(feat. ...)', then a search.
pub fn fetch_lyrics(track: &Track) -> Lyrics {
    match try_lyrics(track) {
        Ok(l) => l,
        Err(_) => Lyrics::new(&track.id, LState::Error),
    }
}

fn try_lyrics(track: &Track) -> Result<Lyrics, String> {
    let artist = split_artists(&track.artist).into_iter().next().unwrap_or_default();
    let mut tried: Vec<String> = vec![];
    for title in [track.title.clone(), clean_title(&track.title)] {
        if title.is_empty() || tried.contains(&title) {
            continue;
        }
        tried.push(title.clone());
        let mut q = vec![("track_name", title), ("artist_name", artist.clone())];
        if track.duration > 0 {
            q.push(("duration", track.duration.to_string()));
        }
        if let Some(d) = lrclib("get", &q)? {
            if let Some(l) = lyrics_result(&track.id, &d) {
                return Ok(l);
            }
        }
    }
    let name = clean_title(&track.title);
    let found = lrclib("search", &[("track_name", if name.is_empty() { track.title.clone() } else { name }), ("artist_name", artist)])?;
    let mut found: Vec<Value> = found.and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let dur = track.duration as f64;
    if track.duration > 0 {
        found.retain(|d| (d["duration"].as_f64().unwrap_or(0.0) - dur).abs() <= 6.0);
    }
    found.sort_by(|a, b| {
        let key = |d: &Value| (d["syncedLyrics"].as_str().unwrap_or("").is_empty(), ((d["duration"].as_f64().unwrap_or(0.0) - dur).abs() * 10.0) as i64);
        key(a).cmp(&key(b))
    });
    for d in &found {
        if let Some(l) = lyrics_result(&track.id, d) {
            return Ok(l);
        }
    }
    Ok(Lyrics::new(&track.id, LState::NotFound))
}

// ------------------------------------------------------- playable format
/// rodio 0.20 cannot open mp4/m4a files whose index sits at the end (what yt-dlp and ffmpeg
/// write), so those are converted to WAV with ffmpeg first. Converted files are cached.
pub fn playable(src: &Path, tmp: &Path) -> Result<PathBuf, String> {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if matches!(ext.as_str(), "wav" | "flac" | "mp3" | "ogg") {
        return Ok(src.to_path_buf());
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    let key = format!("{:016x}", h.finish());
    let dir = tmp.join("play");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(format!("{key}.wav"));
    let lock = LOCKS.lock().unwrap().entry(format!("play:{key}")).or_default().clone();
    let _guard = lock.lock().unwrap();
    if out.exists() {
        return Ok(out);
    }
    let part = dir.join(format!("{key}.part.wav"));
    let res = command("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"]).arg(src)
        .args(["-vn", "-c:a", "pcm_s16le"]).arg(&part)
        .output().map_err(|e| format!("could not run ffmpeg ({e})"))?;
    if !res.status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("ffmpeg could not convert the song: {}", String::from_utf8_lossy(&res.stderr).lines().last().unwrap_or("")));
    }
    std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
    Ok(out)
}
