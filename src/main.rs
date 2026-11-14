mod audio;
mod cover;
mod mix;
mod model;
mod store;
mod taste;
mod ytm;

use audio::{PState, Player};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, FontId, Key, Layout, Rect, RichText, Sense, Stroke};
use model::*;
use rand::seq::SliceRandom;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use store::*;
use taste::Taste;
use global_hotkey::{hotkey::{Code, HotKey}, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};
use ytm::{LState, Lyrics};

const BG: Color32 = Color32::BLACK;
const WHITE: Color32 = Color32::from_rgb(232, 232, 232);
const DIM: Color32 = Color32::from_rgb(122, 122, 122);
const BORDER: Color32 = Color32::from_rgb(46, 46, 46);
const HOVER_BG: Color32 = Color32::from_rgb(20, 20, 20);
const TRACK: Color32 = Color32::from_rgb(38, 38, 38);
const ROW: f32 = 30.0;

#[derive(Clone, Copy, PartialEq)]
enum View { Results, Queue, Lyrics, Library, Playlists, Taste, Stats, Audio, Settings }
const TABS: [(View, &str); 9] = [
    (View::Results, "results"), (View::Queue, "queue"), (View::Lyrics, "lyrics"), (View::Library, "library"),
    (View::Playlists, "playlists"), (View::Taste, "taste"), (View::Stats, "stats"), (View::Audio, "audio"),
    (View::Settings, "settings"),
];
#[derive(Clone, Copy, PartialEq)]
enum QueueKind { Auto, List }
#[derive(Clone, Copy, PartialEq)]
enum Repeat { Off, All, One }
#[derive(Clone, Copy)]
enum MediaKey { PlayPause, Play, Pause, Next, Prev, Stop }

/// Windows 'now playing' overlay (volume pop-up, lock screen): title, artist, cover, and its buttons.
fn setup_smtc(cc: &eframe::CreationContext<'_>, tx: Sender<Msg>, ctx: egui::Context) -> Option<MediaControls> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::Win32(h) = cc.window_handle().ok()?.as_raw() else { return None };
    let mut c = MediaControls::new(PlatformConfig {
        dbus_name: "music_player", display_name: "Music Player", hwnd: Some(h.hwnd.get() as *mut std::ffi::c_void),
    }).ok()?;
    c.attach(move |e| {
        let k = match e {
            MediaControlEvent::Play => MediaKey::Play,
            MediaControlEvent::Pause => MediaKey::Pause,
            MediaControlEvent::Toggle => MediaKey::PlayPause,
            MediaControlEvent::Next => MediaKey::Next,
            MediaControlEvent::Previous => MediaKey::Prev,
            MediaControlEvent::Stop => MediaKey::Stop,
            _ => return,
        };
        let _ = tx.send(Msg::Media(k));
        ctx.request_repaint();
    }).ok()?;
    Some(c)
}

/// Register the keyboard media keys (play/pause, next, previous, stop) with Windows.
fn setup_media_keys() -> (Option<GlobalHotKeyManager>, HashMap<u32, MediaKey>) {
    let mut ids = HashMap::new();
    let Ok(m) = GlobalHotKeyManager::new() else { return (None, ids) };
    for (code, key) in [(Code::MediaPlayPause, MediaKey::PlayPause), (Code::MediaTrackNext, MediaKey::Next),
                        (Code::MediaTrackPrevious, MediaKey::Prev), (Code::MediaStop, MediaKey::Stop)] {
        let hk = HotKey::new(None, code);
        if m.register(hk).is_ok() { ids.insert(hk.id(), key); }
    }
    (Some(m), ids)
}

/// Results of background work, handled on the UI thread.
enum Msg {
    Results(Result<Vec<Track>, String>),
    Ready { gen: u64, path: PathBuf, lufs: Option<f64>, fade: f64 },
    Failed { gen: u64, err: String },
    Mix { qgen: u64, songs: Vec<Track> },
    Lyrics(String, Lyrics),
    Library(Vec<Track>),
    Cover { key: String, colors: Option<Vec<[u8; 3]>> },
    Gain(String, Option<f64>),
    Net(bool),
    Flash(String, f32),
    Saved(Track),
    Media(MediaKey),
}

#[derive(Clone)]
struct Bus { tx: Sender<Msg>, ctx: egui::Context }
impl Bus {
    fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
        self.ctx.request_repaint();
    }
}

struct Paths { save: PathBuf, library: PathBuf, export: PathBuf }

struct App {
    bus: Bus,
    rx: Receiver<Msg>,
    paths: Paths,
    player: Option<Player>,
    tmpdir: PathBuf,
    lufs: Arc<Loudness>,
    taste: Taste,
    settings: Settings,
    playlists: Playlists,

    results: Vec<Track>,
    list_msg: String,
    queue: Vec<Track>,
    queue_gen: u64,
    queue_kind: QueueKind,
    index: usize,
    has_track: bool,
    busy: bool,
    busy_text: String,
    play_gen: u64,
    state: PState,
    pos: f64,
    length: f64,
    flash: (String, Instant),
    autoplay: bool,
    autosave: bool,
    autosaved: bool,
    shuffle: bool,
    repeat: Repeat,
    volume: f32,
    logged: bool,
    finished: bool,
    crossfaded: bool,
    crossfade_pending: f64,
    mix_loading: bool,
    play_when_ready: bool,
    prefetched: HashSet<String>,
    recent_random: Vec<String>,
    recent_artists: Vec<Vec<String>>,

    view: View,
    search: String,
    search_id: egui::Id,
    drag_frac: Option<f32>,
    library: Vec<Track>,
    library_all: Vec<Track>,
    library_filter: String,
    library_loading: bool,
    lyrics: Option<Lyrics>,
    lyrics_cache: HashMap<String, Lyrics>,
    lyr_idx: i64,
    pl_open: Option<String>,
    pl_last: Option<String>,
    pl_note: String,
    pl_confirm: Option<(String, Instant)>,
    sleep_end: Option<Instant>,
    sleep_minutes: u32,
    last_sleep_f: f32,
    online: bool,
    net_last: Instant,
    cover_for: Option<String>,
    cover_cache: HashMap<String, Vec<[u8; 3]>>,
    cover_cells: Vec<[u8; 3]>,
    eq_dirty: bool,
    theme_applied: String,
    _hotkeys: Option<GlobalHotKeyManager>,
    media_ids: HashMap<u32, MediaKey>,
    smtc: Option<MediaControls>,
    smtc_key: (Option<String>, u8),
    smtc_last: Instant,
}

fn accent_rgb(theme: &str) -> [u8; 3] {
    THEMES.iter().find(|(n, _)| *n == theme).map(|(_, c)| *c).unwrap_or(THEMES[0].1)
}
fn rgb(c: [u8; 3]) -> Color32 { Color32::from_rgb(c[0], c[1], c[2]) }

fn apply_visuals(ctx: &egui::Context, acc: Color32) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = BG;
    v.faint_bg_color = BG;
    v.selection.bg_fill = acc;
    v.selection.stroke = Stroke::new(1.0, BG);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, WHITE);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.bg_fill = TRACK;
    v.widgets.inactive.weak_bg_fill = TRACK;
    v.widgets.hovered.bg_fill = acc;
    v.widgets.active.bg_fill = acc;
    ctx.set_visuals(v);
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // Windows system fonts as fallback for Japanese, Chinese, Korean and symbols
    for (name, file) in [("yugoth", "YuGothM.ttc"), ("msyh", "msyh.ttc"), ("malgun", "malgun.ttf"), ("segsym", "seguisym.ttf")] {
        if let Ok(bytes) = std::fs::read(format!(r"C:\Windows\Fonts\{file}")) {
            fonts.font_data.insert(name.into(), egui::FontData::from_owned(bytes));
            for fam in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
                fonts.families.entry(fam).or_default().push(name.into());
            }
        }
    }
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    for (_, f) in style.text_styles.iter_mut() {
        f.family = egui::FontFamily::Monospace;
    }
    style.text_styles.insert(egui::TextStyle::Body, FontId::monospace(14.0));
    style.text_styles.insert(egui::TextStyle::Button, FontId::monospace(14.0));
    style.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(14.0));
    style.text_styles.insert(egui::TextStyle::Small, FontId::monospace(12.0));
    style.spacing.item_spacing = vec2(8.0, 4.0);
    ctx.set_style(style);
}

// ------------------------------------------------------------- small widgets
fn boxed() -> egui::Frame {
    egui::Frame::none().fill(BG).stroke(Stroke::new(1.0, BORDER))
        .inner_margin(egui::Margin::same(10.0)).outer_margin(egui::Margin::symmetric(8.0, 3.0))
}

/// A '[button]' in terminal style: coloured text, accent colour on hover. `bg` draws it as a filled bar.
fn tbtn2(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Option<Color32>, acc: Color32) -> egui::Response {
    let font = FontId::monospace(14.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), fg);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(8.0, 6.0), Sense::click());
    if let Some(b) = bg {
        ui.painter().rect_filled(rect, 0.0, b);
    }
    let c = if bg.is_none() && resp.hovered() { acc } else { fg };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font, c);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
fn tbtn(ui: &mut egui::Ui, text: &str, fg: Color32, acc: Color32) -> egui::Response {
    tbtn2(ui, text, fg, None, acc)
}

fn ell(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars || max_chars == 0 {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[derive(PartialEq, Clone, Copy)]
enum RowAct { None, Play, Right, Up, Remove }

/// One song line: '01  title        artist     3:35'. The playing song is drawn in inverse
/// video. actions: 0 none, 1 = [^] [x], 2 = [x].
fn track_row(ui: &mut egui::Ui, t: &Track, label: &str, playing: bool, actions: u8, acc: Color32) -> RowAct {
    let font = FontId::monospace(14.0);
    let cw = ui.fonts(|f| f.glyph_width(&font, '0'));
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let p = ui.painter_at(rect);
    if playing {
        p.rect_filled(rect, 0.0, acc);
    } else if resp.hovered() {
        p.rect_filled(rect, 0.0, HOVER_BG);
    }
    let (main, dim) = if playing { (BG, BG) } else { (WHITE, DIM) };
    let cy = rect.center().y;
    let right = match actions { 1 => 7.0 * cw + 20.0, 2 => 3.0 * cw + 20.0, _ => 0.0 };
    let dur_x = rect.right() - 16.0 - right;
    let artist_w = (rect.width() * 0.28).max(120.0);
    let artist_x = dur_x - 5.0 * cw - 20.0 - artist_w;
    let title_x = rect.left() + 10.0 + 2.0 * cw + 18.0;
    p.text(pos2(rect.left() + 10.0, cy), Align2::LEFT_CENTER, label, font.clone(), dim);
    p.text(pos2(title_x, cy), Align2::LEFT_CENTER, ell(&t.title, ((artist_x - title_x - 16.0) / cw) as usize), font.clone(), main);
    p.text(pos2(artist_x, cy), Align2::LEFT_CENTER, ell(&t.artist, (artist_w / cw) as usize), font.clone(), dim);
    p.text(pos2(dur_x, cy), Align2::RIGHT_CENTER, fmt(t.duration as f64), font.clone(), dim);
    if actions > 0 {
        let xr = rect.right() - 16.0;
        p.text(pos2(xr, cy), Align2::RIGHT_CENTER, "[x]", font.clone(), dim);
        if actions == 1 {
            p.text(pos2(xr - 4.0 * cw, cy), Align2::RIGHT_CENTER, "[^]", font.clone(), dim);
        }
    }
    if resp.secondary_clicked() {
        return RowAct::Right;
    }
    if resp.clicked() {
        let x = resp.interact_pointer_pos().map(|p| p.x).unwrap_or(0.0);
        if actions > 0 {
            if x >= rect.right() - 16.0 - 3.0 * cw - 4.0 { return RowAct::Remove; }
            if actions == 1 && x >= rect.right() - 16.0 - 7.0 * cw - 4.0 { return RowAct::Up; }
        }
        return RowAct::Play;
    }
    RowAct::None
}

/// A clickable line with text on the left and dim text on the right.
fn line_row(ui: &mut egui::Ui, left: &str, right: &str) -> bool {
    let font = FontId::monospace(14.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, HOVER_BG);
    }
    ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, left, font.clone(), WHITE);
    ui.painter().text(pos2(rect.right() - 16.0, rect.center().y), Align2::RIGHT_CENTER, right, font, DIM);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn bar_row(ui: &mut egui::Ui, name: &str, frac: f32, value: &str, acc: Color32) {
    let font = FontId::monospace(14.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let p = ui.painter();
    let cy = rect.center().y;
    p.text(pos2(rect.left() + 10.0, cy), Align2::LEFT_CENTER, ell(name, 24), font.clone(), WHITE);
    let x0 = rect.left() + rect.width() * 0.45;
    p.rect_filled(Rect::from_min_size(pos2(x0, cy - 4.0), vec2(200.0 * frac.clamp(0.0, 1.0), 8.0)), 0.0, acc);
    p.text(pos2(rect.right() - 16.0, cy), Align2::RIGHT_CENTER, value, font, DIM);
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(WHITE));
}
fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(DIM).size(12.0));
}

/// 'label   [a] [b] [c]' with the chosen one lit up. Returns the index that was clicked.
fn opt_row(ui: &mut egui::Ui, label: &str, opts: &[(String, Option<Color32>)], cur: usize, acc: Color32, extra: &str) -> Option<usize> {
    let mut pick = None;
    ui.horizontal_wrapped(|ui| {
        ui.add_sized([150.0, 24.0], egui::Label::new(RichText::new(label).color(DIM)));
        for (i, (text, col)) in opts.iter().enumerate() {
            let shown = format!("[{text}]");
            let active = i == cur;
            let r = match col {
                Some(c) => tbtn2(ui, &shown, if active { BG } else { *c }, if active { Some(*c) } else { None }, *c),
                None => tbtn(ui, &shown, if active { acc } else { WHITE }, acc),
            };
            if r.clicked() {
                pick = Some(i);
            }
        }
        if !extra.is_empty() {
            note(ui, extra);
        }
    });
    pick
}

// ------------------------------------------------------------------- the app
impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        // data files (taste.json, settings.json, ...) live in MUSIC_HOME, or the folder you start the app from
        let home = std::env::var_os("MUSIC_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let paths = Paths { save: home.join("saved"), library: home.join("library.json"), export: home.join("playlists") };
        let settings = Settings::load(&home.join("settings.json"));
        let (tx, rx) = channel();
        let bus = Bus { tx, ctx: cc.egui_ctx.clone() };
        let mut list_msg = "Search for a song to get started".to_string();
        let player = match Player::new() {
            Ok(p) => {
                p.set_eq(settings.eq.preamp, &settings.eq.bands);
                Some(p)
            }
            Err(e) => {
                list_msg = format!("{e}");
                None
            }
        };
        let tmpdir = std::env::temp_dir().join(format!("musicapp_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmpdir);
        apply_visuals(&cc.egui_ctx, rgb(accent_rgb(&settings.theme)));
        let theme_applied = settings.theme.clone();
        let (hotkeys, media_ids) = setup_media_keys();
        let smtc = setup_smtc(cc, bus.tx.clone(), cc.egui_ctx.clone());
        App {
            _hotkeys: hotkeys, media_ids, smtc, smtc_key: (None, 0), smtc_last: Instant::now(),
            bus, rx, player, tmpdir,
            lufs: Arc::new(Loudness::load(&home.join("loudness.json"))),
            taste: Taste::load(&home.join("taste.json")),
            playlists: Playlists::load(&home.join("playlists.json")),
            paths, volume: 100.0,
            results: vec![], list_msg, queue: vec![], queue_gen: 0, queue_kind: QueueKind::Auto, index: 0,
            has_track: false, busy: false, busy_text: String::new(), play_gen: 0,
            state: PState::Idle, pos: 0.0, length: 0.0, flash: (String::new(), Instant::now()),
            autoplay: true, autosave: std::env::var("MUSIC_AUTOSAVE").map(|v| v == "1").unwrap_or(false), autosaved: false,
            shuffle: false, repeat: Repeat::Off, logged: false, finished: false, crossfaded: false, crossfade_pending: 0.0,
            mix_loading: false, play_when_ready: false, prefetched: HashSet::new(),
            recent_random: vec![], recent_artists: vec![],
            view: View::Results, search: String::new(), search_id: egui::Id::new("search_box"), drag_frac: None,
            library: vec![], library_all: vec![], library_filter: String::new(), library_loading: false,
            lyrics: None, lyrics_cache: HashMap::new(), lyr_idx: -2,
            pl_open: None, pl_last: None, pl_note: String::new(), pl_confirm: None,
            sleep_end: None, sleep_minutes: 0, last_sleep_f: 1.0,
            online: true, net_last: Instant::now() - Duration::from_secs(60),
            cover_for: None, cover_cache: HashMap::new(), cover_cells: vec![], eq_dirty: false, theme_applied,
            settings,
        }
    }

    fn accent(&self) -> Color32 { rgb(accent_rgb(&self.settings.theme)) }
    fn offline(&self) -> bool { self.settings.offline || !self.online }
    fn flash(&mut self, text: impl Into<String>, secs: f32) {
        self.flash = (text.into(), Instant::now() + Duration::from_secs_f32(secs));
    }
    fn current_track(&self) -> Option<&Track> {
        if self.index < self.queue.len() && (self.has_track || self.busy) { Some(&self.queue[self.index]) } else { None }
    }
    /// (index of the first row, rows): the playing song, then what is coming.
    fn queue_rows(&self) -> (usize, &[Track]) {
        let start = if self.current_track().is_some() { self.index.min(self.queue.len()) } else { 0 };
        (start, &self.queue[start..])
    }

    // ---- background messages
    fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Results(r) => {
                match r {
                    Ok(t) => { self.results = t; self.list_msg = "No results found".into(); }
                    Err(e) => { self.results.clear(); self.list_msg = format!("Search failed: {e}"); }
                }
            }
            Msg::Ready { gen, path, lufs, fade } => {
                if gen != self.play_gen { return; }
                let gain = if self.settings.loudness { lufs.map(loudness_gain_db).unwrap_or(0.0) } else { 0.0 };
                let res = match self.player.as_mut() {
                    Some(p) => p.play(&path, gain as f32, fade as f32),
                    None => Err("no audio output device".to_string()),
                };
                match res { Ok(()) => self.started(), Err(e) => self.play_failed(e) }
            }
            Msg::Failed { gen, err } => {
                if gen == self.play_gen { self.play_failed(err); }
            }
            Msg::Mix { qgen, songs } => self.mix_ready(qgen, songs),
            Msg::Lyrics(id, data) => {
                if data.state != LState::Error {
                    if self.lyrics_cache.len() > 40 { self.lyrics_cache.clear(); }
                    self.lyrics_cache.insert(id.clone(), data.clone());
                }
                if self.lyrics.as_ref().map(|l| l.id == id).unwrap_or(false) {
                    self.lyrics = Some(data);
                    self.lyr_idx = -2;
                }
            }
            Msg::Library(t) => {
                self.library_all = t;
                self.library_loading = false;
                self.apply_library_filter();
            }
            Msg::Cover { key, colors } => {
                if let Some(c) = colors {
                    if self.cover_cache.len() > 40 { self.cover_cache.clear(); }
                    self.cover_cache.insert(key.clone(), c.clone());
                    if self.cover_for.as_deref() == Some(key.as_str()) { self.cover_cells = c; }
                }
            }
            Msg::Gain(id, lufs) => {
                if self.settings.loudness && self.current_track().map(|t| t.id == id).unwrap_or(false) {
                    if let (Some(l), Some(p)) = (lufs, self.player.as_mut()) { p.set_gain(loudness_gain_db(l) as f32); }
                }
            }
            Msg::Net(ok) => {
                if ok != self.online {
                    self.online = ok;
                    self.flash(if ok { "Back online" } else { "Offline: playing your saved songs" }, 4.0);
                }
            }
            Msg::Flash(t, s) => self.flash(t, s),
            Msg::Saved(t) => self.taste.record_save(&t),
            Msg::Media(k) => self.media_action(k),
        }
    }

    // ---- 100 ms tick
    fn tick(&mut self, ctx: &egui::Context) {
        if let Some(p) = self.player.as_mut() { p.tick_fade(); }
        if self.has_track && !self.busy {
            let (st, pos) = self.player.as_ref().map(|p| p.status()).unwrap_or((PState::Idle, 0.0));
            self.state = st;
            self.pos = pos;
            if let Some(track) = self.current_track().cloned() {
                self.length = track.duration as f64;
                let cf = self.settings.crossfade;
                if self.autosave && !self.autosaved && (pos >= 30.0 || (self.length > 0.0 && pos >= self.length * 0.5) || st == PState::Ended) {
                    self.autosaved = true; // listened long enough: it is not a skip
                    self.autosave_track(&track);
                }
                if cf > 0.0 && !self.crossfaded && st == PState::Playing && self.length > 0.0
                    && self.length - pos <= cf + 0.3 && (self.autoplay || self.repeat == Repeat::One) && self.next_is_ready()
                {
                    self.crossfaded = true; // the next song starts now, overlapping this one
                    self.finished = true;
                    self.log_listen(true);
                    self.crossfade_pending = cf;
                    self.advance(true);
                } else if st == PState::Ended && !self.finished {
                    self.finished = true;
                    self.log_listen(true);
                    if self.autoplay || self.repeat == Repeat::One { self.advance(true); }
                }
            }
        }
        self.tick_sleep();
        if self.net_last.elapsed() > Duration::from_secs(30) {
            self.net_last = Instant::now();
            let bus = self.bus.clone();
            std::thread::spawn(move || {
                use std::net::ToSocketAddrs;
                let ok = ("music.youtube.com", 443).to_socket_addrs().ok()
                    .and_then(|mut a| a.next())
                    .map(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok()).unwrap_or(false);
                bus.send(Msg::Net(ok));
            });
        }
        if self.eq_dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.eq_dirty = false;
            self.settings.save();
        }
        if self.pl_confirm.as_ref().map(|(_, t)| Instant::now() > *t).unwrap_or(false) { self.pl_confirm = None; }
        if self.view == View::Lyrics { self.ensure_lyrics(); }
        self.sync_cover();
    }

    // ---- playback
    fn play_index(&mut self, i: usize) {
        if i >= self.queue.len() { self.stop(); return; }
        self.log_listen(false);
        self.play_when_ready = false;
        self.index = i;
        let track = self.queue[i].clone();
        self.taste.mark_heard(&track);
        self.recent_artists.push(split_artists(&track.artist).iter().map(|n| n.to_lowercase()).collect());
        if self.recent_artists.len() > 8 { self.recent_artists.remove(0); }
        self.busy = true;
        self.busy_text = "Downloading…".into();
        let fade = std::mem::take(&mut self.crossfade_pending);
        self.play_gen += 1;
        let gen = self.play_gen;
        let (bus, tmp, lufs, loud) = (self.bus.clone(), self.tmpdir.clone(), self.lufs.clone(), self.settings.loudness);
        std::thread::spawn(move || match ytm::fetch(&track, &tmp) {
            Ok(path) => {
                let l = if loud { lufs.get_or_measure(&track.id, &path) } else { lufs.get(&track.id) };
                match ytm::playable(&path, &tmp) {
                    Ok(path) => bus.send(Msg::Ready { gen, path, lufs: l, fade }),
                    Err(err) => bus.send(Msg::Failed { gen, err }),
                }
            }
            Err(err) => bus.send(Msg::Failed { gen, err }),
        });
    }

    fn started(&mut self) {
        self.has_track = true;
        self.busy = false;
        self.finished = false;
        self.logged = false;
        self.autosaved = false;
        self.crossfaded = false;
        if self.autoplay && self.index + 1 >= self.queue.len() {
            self.request_mix(false); // build the next songs now so there is no gap
        }
        self.prefetch_next();
    }

    fn play_failed(&mut self, err: String) {
        self.busy = false;
        if self.autoplay && self.index + 1 < self.queue.len() {
            self.play_index(self.index + 1); // skip the broken track
        } else if self.autoplay && self.offline() && self.current_track().is_some() {
            self.request_mix(true);
        } else {
            self.has_track = false;
            self.list_msg = format!("Could not play: {err}");
            self.flash(format!("Could not play: {err}"), 6.0);
        }
    }

    fn toggle(&mut self) {
        if self.busy { return; }
        if !self.has_track {
            if self.index < self.queue.len() { self.play_index(self.index); }
            else if !self.results.is_empty() { self.play_from_results(0); }
            return;
        }
        match self.state {
            PState::Paused => { if let Some(p) = &self.player { p.resume(); } }
            PState::Ended | PState::Idle => self.play_index(self.index),
            PState::Playing => { if let Some(p) = &self.player { p.pause(); } }
        }
    }

    fn next(&mut self) {
        if !self.queue.is_empty() && !self.busy { self.advance(false); }
    }

    fn prev(&mut self) {
        if self.busy || self.queue.is_empty() { return; }
        if self.has_track && self.pos > 3.0 {
            if let Some(p) = &self.player { p.seek(0.0); }
            self.pos = 0.0;
        } else {
            self.play_index(self.index.saturating_sub(1));
        }
    }

    fn stop(&mut self) {
        self.log_listen(false);
        self.play_gen += 1; // cancels a download that is still running
        if let Some(p) = self.player.as_mut() { p.stop(); }
        self.has_track = false;
        self.busy = false;
        self.play_when_ready = false;
        self.state = PState::Idle;
        self.pos = 0.0;
        self.length = 0.0;
    }

    fn seek_to(&mut self, secs: f64) {
        if !self.has_track || self.busy || self.length <= 0.0 { return; }
        self.pos = secs.clamp(0.0, (self.length - 1.0).max(0.0));
        if let Some(p) = &self.player { p.seek(self.pos); }
    }

    fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 100.0);
        if let Some(p) = self.player.as_mut() { p.set_volume(self.volume); }
    }

    fn log_listen(&mut self, completed: bool) {
        let Some(t) = self.current_track().cloned() else { return };
        if !self.has_track || self.logged { return; }
        self.logged = true;
        let len = if self.length > 0.0 { self.length } else { t.duration as f64 };
        self.taste.record(&t, self.pos, len, completed);
    }

    /// Play the next queued song. auto = the song just ended by itself ('repeat one' replays it).
    fn advance(&mut self, auto: bool) {
        if auto && self.repeat == Repeat::One {
            self.play_index(self.index);
        } else if self.index + 1 < self.queue.len() {
            self.play_index(self.index + 1);
        } else if self.repeat == Repeat::All && self.queue_kind == QueueKind::List && !self.queue.is_empty() {
            if self.shuffle { self.queue.shuffle(&mut rand::thread_rng()); }
            self.play_index(0);
        } else if self.current_track().is_some() {
            self.request_mix(true);
        }
    }

    fn next_is_ready(&self) -> bool {
        self.repeat == Repeat::One || self.index + 1 < self.queue.len()
            || (self.repeat == Repeat::All && self.queue_kind == QueueKind::List && !self.queue.is_empty())
    }

    fn set_queue(&mut self, q: Vec<Track>, kind: QueueKind) {
        self.queue = q;
        self.queue_kind = kind;
        self.queue_gen += 1;
    }
    fn play_from_results(&mut self, i: usize) {
        if let Some(t) = self.results.get(i).cloned() { self.play_single(t); }
    }
    fn play_single(&mut self, t: Track) {
        self.set_queue(vec![t], QueueKind::Auto);
        self.play_index(0);
    }
    /// Play a finite list starting at tracks[start]. Shuffle puts the chosen song first and mixes the rest.
    fn play_list(&mut self, tracks: Vec<Track>, start: usize, shuffle: Option<bool>) {
        if start >= tracks.len() { return; }
        let mut start = start;
        let q = if shuffle.unwrap_or(self.shuffle) {
            let first = tracks[start].clone();
            let mut rest: Vec<Track> = tracks.iter().enumerate().filter(|(i, _)| *i != start).map(|(_, t)| t.clone()).collect();
            rest.shuffle(&mut rand::thread_rng());
            start = 0;
            std::iter::once(first).chain(rest).collect()
        } else {
            tracks
        };
        self.set_queue(q, QueueKind::List);
        self.play_index(start);
    }

    fn queue_next(&mut self, t: Track) {
        if self.current_track().is_some() {
            let at = (self.index + 1).min(self.queue.len());
            self.flash(format!("Queued next: {}", t.title), 3.0);
            self.queue.insert(at, t);
            self.prefetch_next();
        } else {
            self.play_single(t);
        }
    }

    /// [random]: a random song from your liking; autoplay then continues with songs like it.
    fn play_random(&mut self) {
        if self.busy { return; }
        let cur = self.current_track().map(|t| t.id.clone());
        let scores = self.taste.scores();
        let liked = self.taste.liked();
        let mut track = None;
        // avoid the last 15 picks; with a small library shrink the window until something is found
        for keep in (0..=self.recent_random.len()).rev() {
            let mut avoid: HashSet<String> = self.recent_random[self.recent_random.len() - keep..].iter().cloned().collect();
            if let Some(c) = &cur { avoid.insert(c.clone()); }
            track = mix::pick_liked(&liked, &avoid, &scores);
            if track.is_some() { break; }
        }
        match track {
            None => self.flash(if liked.is_empty() { "No favorites yet: play or save a few songs first" } else { "That's your only favorite so far: play or save more songs" }, 5.0),
            Some(t) => {
                self.recent_random.push(t.id.clone());
                if self.recent_random.len() > 15 { self.recent_random.remove(0); }
                self.play_single(t);
            }
        }
    }

    fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        if self.shuffle && self.queue_kind == QueueKind::List && self.index < self.queue.len() {
            self.queue[self.index + 1..].shuffle(&mut rand::thread_rng());
        }
        self.flash(if self.shuffle { "Shuffle on (saved songs and playlists)" } else { "Shuffle off" }, 2.5);
    }
    fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat { Repeat::Off => Repeat::All, Repeat::All => Repeat::One, Repeat::One => Repeat::Off };
        self.flash(match self.repeat {
            Repeat::Off => "Repeat off",
            Repeat::All => "Repeat all: saved songs and playlists loop",
            Repeat::One => "Repeat one: this song again and again",
        }, 2.5);
    }

    fn like_current(&mut self) {
        let Some(t) = self.current_track().cloned() else { return self.flash("Play a song first", 3.0) };
        let liked = self.taste.toggle_like(&t);
        self.flash(if liked { format!("Liked: {}", t.title) } else { "Like removed".into() }, 3.0);
    }

    /// [never]: never play this song's artist again (press again to allow them).
    fn never_current(&mut self) {
        let names = self.current_track().map(|t| split_artists(&t.artist)).unwrap_or_default();
        let Some(artist) = names.first().cloned() else { return self.flash("Play a song first", 3.0) };
        if self.taste.is_banned(&artist) {
            self.taste.toggle_ban(&artist);
            self.flash(format!("{artist} is allowed again"), 3.0);
            return;
        }
        self.taste.toggle_ban(&artist);
        let scores = self.taste.scores();
        let keep = (self.index + 1).min(self.queue.len());
        let tail = self.queue.split_off(keep);
        self.queue.extend(tail.into_iter().filter(|t| scores.affinity(&t.artist) > -2.0));
        self.flash(format!("Never playing {artist} again"), 3.0);
        if self.has_track && !self.busy { self.next(); }
    }

    fn save_current(&mut self) {
        let Some(t) = self.current_track().cloned().filter(|_| self.has_track && !self.busy) else {
            return self.flash("Play a song first", 3.0);
        };
        let (bus, tmp, dir) = (self.bus.clone(), self.tmpdir.clone(), self.paths.save.clone());
        std::thread::spawn(move || {
            let msg = match ytm::save_track(&t, &tmp, &dir) {
                Ok((p, status)) => {
                    let base = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if status != "exists" { bus.send(Msg::Saved(t.clone())); }
                    match status {
                        "tagged" => format!("Saved with tags: {base}"),
                        "untagged" => format!("Saved, but tagging failed: {base}"),
                        _ => "Already saved".into(),
                    }
                }
                Err(e) => format!("Could not save: {e}"),
            };
            bus.send(Msg::Flash(msg, 3.0));
        });
    }

    fn autosave_track(&mut self, t: &Track) {
        let (bus, tmp, dir, t) = (self.bus.clone(), self.tmpdir.clone(), self.paths.save.clone(), t.clone());
        std::thread::spawn(move || {
            let msg = match ytm::save_track(&t, &tmp, &dir) {
                Ok((_, "exists")) => None,
                Ok((p, _)) => Some(format!("Autosaved: {}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
                Err(e) => Some(format!("Autosave failed: {e}")),
            };
            if let Some(m) = msg { bus.send(Msg::Flash(m, 2.5)); }
        });
    }

    // ---- autoplay
    fn prefetch_next(&mut self) {
        let i = self.index + 1;
        if !self.autoplay || i >= self.queue.len() { return; }
        let t = self.queue[i].clone();
        if !self.prefetched.insert(t.id.clone()) { return; }
        let (tmp, lufs, loud) = (self.tmpdir.clone(), self.lufs.clone(), self.settings.loudness);
        std::thread::spawn(move || {
            if let Ok(p) = ytm::fetch(&t, &tmp) {
                if loud { lufs.get_or_measure(&t.id, &p); }
                let _ = ytm::playable(&p, &tmp);
            }
        });
    }

    fn toggle_autoplay(&mut self) {
        self.autoplay = !self.autoplay;
        if self.autoplay && self.has_track && !self.busy {
            if self.index + 1 >= self.queue.len() { self.request_mix(false) } else { self.prefetch_next() }
        }
    }

    fn request_mix(&mut self, play_when_ready: bool) {
        let Some(track) = self.current_track().cloned() else { self.play_when_ready = false; return };
        if play_when_ready {
            self.play_when_ready = true;
            self.busy = true;
            self.busy_text = "Finding more songs…".into();
        }
        if self.mix_loading { return; }
        self.mix_loading = true;
        // everything that must not come back: this queue, the search results, every song ever
        // played here, and the songs you already like (those are for [random])
        let mut avoid_ids: HashSet<String> = self.queue.iter().chain(&self.results).map(|t| t.id.clone()).collect();
        avoid_ids.extend(self.taste.heard_ids());
        avoid_ids.extend(self.taste.favourites().into_iter().map(|t| t.id));
        let mut avoid_keys = self.taste.heard_keys();
        avoid_keys.extend(self.queue.iter().map(|t| song_key(&t.title, &t.artist)));
        let recent: HashSet<String> = self.recent_artists.iter().flatten().cloned().collect();
        let (seeds, scores, offline, qgen) = (self.taste.pick_seeds(4), self.taste.scores(), self.offline(), self.queue_gen);
        let (queue_ids, queue_keys): (HashSet<String>, HashSet<String>) =
            (self.queue.iter().map(|t| t.id.clone()).collect(), self.queue.iter().map(|t| song_key(&t.title, &t.artist)).collect());
        let (bus, lib_dir, lib_cache) = (self.bus.clone(), self.paths.save.clone(), self.paths.library.clone());
        std::thread::spawn(move || {
            let mut songs = vec![];
            if !offline {
                songs = mix::build_mix(&track, &avoid_ids, &avoid_keys, &recent, seeds, &scores, 20);
            }
            if songs.is_empty() {
                // offline, or nothing found: never go silent, use your saved songs
                let library = scan_library(&lib_dir, &lib_cache);
                songs = mix::pick_offline(&library, &queue_ids, &queue_keys, &scores, 15);
            }
            bus.send(Msg::Mix { qgen, songs });
        });
    }

    fn mix_ready(&mut self, qgen: u64, songs: Vec<Track>) {
        self.mix_loading = false;
        if qgen != self.queue_gen {
            // you moved on to a different list meanwhile
            if self.play_when_ready { self.request_mix(true); }
            return;
        }
        let seen: HashSet<String> = self.queue.iter().map(|t| t.id.clone()).collect();
        let fresh: Vec<Track> = songs.into_iter().filter(|t| !seen.contains(&t.id)).collect();
        let any = !fresh.is_empty();
        self.queue.extend(fresh);
        if self.play_when_ready {
            self.play_when_ready = false;
            self.busy = false;
            if any { self.play_index(self.index + 1); }
        } else {
            self.prefetch_next();
        }
    }

    // ---- sleep timer, loudness, EQ, theme
    fn set_sleep(&mut self, minutes: u32) {
        self.sleep_minutes = minutes;
        self.sleep_end = if minutes > 0 { Some(Instant::now() + Duration::from_secs(minutes as u64 * 60)) } else { None };
        self.last_sleep_f = 1.0;
        if let Some(p) = self.player.as_mut() { p.set_sleep_factor(1.0); }
        self.flash(if minutes > 0 { format!("Sleep timer: the music stops in {minutes} minutes") } else { "Sleep timer off".into() }, 3.0);
    }

    fn tick_sleep(&mut self) {
        let Some(end) = self.sleep_end else { return };
        let left = end.saturating_duration_since(Instant::now()).as_secs_f32();
        if left <= 0.0 {
            self.sleep_end = None;
            self.last_sleep_f = 1.0;
            self.stop();
            if let Some(p) = self.player.as_mut() { p.set_sleep_factor(1.0); }
            self.flash("Sleep timer: goodnight", 5.0);
            return;
        }
        let f = (left / 10.0).min(1.0); // fade out over the last 10 seconds
        if f != self.last_sleep_f {
            self.last_sleep_f = f;
            if let Some(p) = self.player.as_mut() { p.set_sleep_factor(f); }
        }
    }

    fn toggle_loudness(&mut self) {
        self.settings.loudness = !self.settings.loudness;
        self.settings.save();
        let on = self.settings.loudness;
        if !on {
            if let Some(p) = self.player.as_mut() { p.set_gain(0.0); }
        } else if let Some(t) = self.current_track().cloned().filter(|_| self.has_track) {
            let (bus, tmp, lufs) = (self.bus.clone(), self.tmpdir.clone(), self.lufs.clone());
            std::thread::spawn(move || {
                let l = ytm::fetch(&t, &tmp).ok().and_then(|p| lufs.get_or_measure(&t.id, &p));
                bus.send(Msg::Gain(t.id.clone(), l));
            });
        }
        self.flash(if on { "Loudness matching on: songs are evened out" } else { "Loudness matching off" }, 3.0);
    }

    fn apply_eq(&mut self) {
        let eq = &self.settings.eq;
        if let Some(p) = &self.player { p.set_eq(eq.preamp, &eq.bands); }
        self.eq_dirty = true;
    }

    fn set_preset(&mut self, i: usize) {
        let (name, bands) = EQ_PRESETS[i];
        self.settings.eq.bands = bands.to_vec();
        if name == "flat" { self.settings.eq.preamp = 0.0; }
        self.settings.eq.preset = name.into();
        self.apply_eq();
    }

    fn set_theme(&mut self, name: &str) {
        self.settings.theme = name.into();
        self.settings.save();
        self.cover_for = None;
    }

    fn sync_cover(&mut self) {
        let track = self.current_track().cloned();
        let mode = self.settings.cover.clone();
        let acc = accent_rgb(&self.settings.theme);
        let key = match &track {
            Some(t) if mode != "off" => Some(format!("{}|{}|{:?}", t.id, mode, acc)),
            _ => None,
        };
        if key == self.cover_for { return; }
        self.cover_for = key.clone();
        self.cover_cells.clear();
        let (Some(key), Some(t)) = (key, track) else { return };
        if let Some(c) = self.cover_cache.get(&key) {
            self.cover_cells = c.clone();
            return;
        }
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            let colors = cover::load_image(&t).map(|im| cover::mosaic(&im, &mode, acc));
            bus.send(Msg::Cover { key, colors });
        });
    }

    // ---- views, search, library, lyrics, playlists
    fn set_view(&mut self, v: View) {
        self.view = v;
        match v {
            View::Library => self.load_library(),
            View::Lyrics => self.ensure_lyrics(),
            _ => {}
        }
    }

    fn do_search(&mut self) {
        let q = self.search.trim().to_string();
        if self.view == View::Library {
            // in the library, the search box filters your saved songs
            self.library_filter = q;
            self.apply_library_filter();
            return;
        }
        if q.is_empty() { return; }
        if self.offline() {
            self.flash("Offline: searching your saved songs", 4.0);
            self.library_filter = q;
            return self.set_view(View::Library);
        }
        self.view = View::Results;
        self.results.clear();
        self.list_msg = "Searching…".into();
        let bus = self.bus.clone();
        std::thread::spawn(move || bus.send(Msg::Results(ytm::search(&q, 20))));
    }

    fn load_library(&mut self) {
        self.library_loading = true;
        let (bus, dir, cache) = (self.bus.clone(), self.paths.save.clone(), self.paths.library.clone());
        std::thread::spawn(move || bus.send(Msg::Library(scan_library(&dir, &cache))));
    }

    fn apply_library_filter(&mut self) {
        let q = self.library_filter.to_lowercase();
        self.library = self.library_all.iter()
            .filter(|t| format!("{} {} {}", t.title, t.artist, t.album).to_lowercase().contains(&q)).cloned().collect();
    }

    fn ensure_lyrics(&mut self) {
        let Some(t) = self.current_track().cloned() else { self.lyrics = None; return };
        if self.lyrics.as_ref().map(|l| l.id == t.id).unwrap_or(false) { return; }
        self.lyr_idx = -2;
        if let Some(c) = self.lyrics_cache.get(&t.id) {
            self.lyrics = Some(c.clone());
            return;
        }
        self.lyrics = Some(Lyrics::new(&t.id, LState::Loading));
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            let data = ytm::fetch_lyrics(&t);
            bus.send(Msg::Lyrics(t.id.clone(), data));
        });
    }

    fn pl_new(&mut self, from_queue: bool) {
        let tracks: Vec<Track> = if from_queue { self.queue_rows().1.to_vec() } else { vec![] };
        if from_queue && tracks.is_empty() { return self.flash("The queue is empty", 3.0); }
        let name = self.playlists.create(&self.search, &tracks);
        self.flash(format!("Created: {name}"), 3.0);
        self.pl_open = Some(name.clone());
        self.pl_last = Some(name);
    }
    fn pl_play(&mut self, name: &str, start: usize, shuffle: bool) {
        let mut tracks = self.playlists.data.get(name).cloned().unwrap_or_default();
        if shuffle {
            tracks.shuffle(&mut rand::thread_rng());
            self.play_list(tracks, 0, Some(false));
        } else {
            self.play_list(tracks, start, None);
        }
    }
    fn pl_add(&mut self, name: &str, tracks: &[Track], what: &str) {
        let added = if tracks.is_empty() { 0 } else { self.playlists.add(name, tracks) };
        self.flash(if added > 0 { format!("Added {added} to {name}") } else if tracks.is_empty() { "Nothing to add".into() } else { format!("{what} is already in {name}") }, 3.0);
    }

    fn media_keys(&mut self) {
        while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
            if ev.state != HotKeyState::Pressed { continue; }
            if let Some(k) = self.media_ids.get(&ev.id).copied() { self.media_action(k); }
        }
    }

    fn media_action(&mut self, k: MediaKey) {
        match k {
            MediaKey::PlayPause => self.toggle(),
            MediaKey::Play => { if self.state == PState::Paused || !self.has_track { self.toggle(); } }
            MediaKey::Pause => { if self.state == PState::Playing { self.toggle(); } }
            MediaKey::Next => self.next(),
            MediaKey::Prev => self.prev(),
            MediaKey::Stop => self.stop(),
        }
    }

    /// Tell Windows what is playing (overlay next to the volume pop-up, lock screen).
    fn sync_smtc(&mut self) {
        let track = self.current_track().cloned();
        let status: u8 = if track.is_none() { 0 } else if self.busy || self.state == PState::Playing { 1 } else if self.state == PState::Paused { 2 } else { 0 };
        let key = (track.as_ref().map(|t| t.id.clone()), status);
        let Some(c) = self.smtc.as_mut() else { return };
        if key == self.smtc_key && self.smtc_last.elapsed() < Duration::from_secs(5) { return; }
        let id_changed = key.0 != self.smtc_key.0;
        self.smtc_key = key;
        self.smtc_last = Instant::now();
        if let (true, Some(t)) = (id_changed, &track) {
            let cover = Some(t.thumb.as_str()).filter(|u| u.starts_with("http"));
            let _ = c.set_metadata(MediaMetadata {
                title: Some(&t.title), artist: Some(&t.artist), album: Some(&t.album), cover_url: cover,
                duration: (t.duration > 0).then(|| Duration::from_secs(t.duration as u64)),
            });
        }
        let prog = Some(MediaPosition(Duration::from_secs_f64(self.pos.max(0.0))));
        let _ = c.set_playback(match status {
            1 => MediaPlayback::Playing { progress: prog },
            2 => MediaPlayback::Paused { progress: prog },
            _ => MediaPlayback::Stopped,
        });
    }

    // ---- keyboard
    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            ctx.memory_mut(|m| m.surrender_focus(self.search_id));
            return;
        }
        if ctx.wants_keyboard_input() { return; }
        let all = [Key::Space, Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown, Key::N, Key::P, Key::L, Key::B,
                   Key::S, Key::R, Key::A, Key::Z, Key::Y, Key::C, Key::Slash, Key::Num1, Key::Num2, Key::Num3, Key::Num4,
                   Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        let (pressed, shift): (Vec<Key>, bool) = ctx.input(|i| (all.iter().copied().filter(|k| i.key_pressed(*k)).collect(), i.modifiers.shift));
        for k in pressed {
            match k {
                Key::Space => self.toggle(),
                Key::ArrowRight => self.seek_to(self.pos + if shift { 30.0 } else { 5.0 }),
                Key::ArrowLeft => self.seek_to(self.pos - if shift { 30.0 } else { 5.0 }),
                Key::ArrowUp => self.set_volume(self.volume + 5.0),
                Key::ArrowDown => self.set_volume(self.volume - 5.0),
                Key::N => self.next(),
                Key::P => self.prev(),
                Key::L => self.like_current(),
                Key::B => self.never_current(),
                Key::S => self.save_current(),
                Key::R => self.play_random(),
                Key::A => self.toggle_autoplay(),
                Key::Z => self.toggle_shuffle(),
                Key::Y => self.cycle_repeat(),
                Key::C => {
                    let i = THEMES.iter().position(|(n, _)| *n == self.settings.theme).unwrap_or(0);
                    let name = THEMES[(i + 1) % THEMES.len()].0;
                    self.set_theme(name);
                    self.flash(format!("Theme: {name}"), 2.0);
                }
                Key::Slash => {
                    ctx.memory_mut(|m| m.request_focus(self.search_id));
                    ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(s) if s == "/")));
                }
                n => {
                    if let Some(i) = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9].iter().position(|x| *x == n) {
                        self.set_view(TABS[i].0);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------------ UI
impl App {
    fn ui(&mut self, ctx: &egui::Context) {
        let acc = self.accent();
        egui::TopBottomPanel::top("search").frame(boxed()).show(ctx, |ui| self.search_bar(ui, acc));
        egui::TopBottomPanel::bottom("player").frame(boxed()).show(ctx, |ui| self.now_playing(ui, acc));
        egui::CentralPanel::default().frame(boxed()).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (v, label) in TABS {
                    let active = self.view == v;
                    let r = if active { tbtn2(ui, &format!(" {label} "), BG, Some(acc), acc) } else { tbtn(ui, &format!(" {label} "), DIM, acc) };
                    if r.clicked() { self.set_view(v); }
                }
            });
            ui.separator();
            match self.view {
                View::Results | View::Queue | View::Library => self.draw_tracks(ui, acc),
                View::Lyrics => self.draw_lyrics(ui, acc),
                View::Playlists => self.draw_playlists(ui, acc),
                View::Taste => self.draw_taste(ui, acc),
                View::Stats => self.draw_stats(ui, acc),
                View::Audio => self.draw_audio(ui, acc),
                View::Settings => self.draw_settings(ui, acc),
            }
        });
    }

    fn search_bar(&mut self, ui: &mut egui::Ui, acc: Color32) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(">").color(acc).strong());
            let w = (ui.available_width() - 190.0).max(100.0);
            let r = ui.add(egui::TextEdit::singleline(&mut self.search).id(self.search_id).frame(false).desired_width(w)
                .hint_text(RichText::new("search songs, or a playlist name   ( / to focus, esc to leave )").color(DIM)));
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) { self.do_search(); }
            if tbtn(ui, "[random]", WHITE, acc).clicked() { self.play_random(); }
            if tbtn(ui, "[search]", WHITE, acc).clicked() { self.do_search(); }
        });
    }

    fn now_playing(&mut self, ui: &mut egui::Ui, acc: Color32) {
        let track = self.current_track().cloned();
        ui.horizontal(|ui| {
            if self.settings.cover != "off" {
                let size = cover::COVER_N as f32 * 5.0;
                let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
                let p = ui.painter();
                p.rect_stroke(rect.expand(2.0), 0.0, Stroke::new(1.0, BORDER));
                for (i, c) in self.cover_cells.iter().enumerate() {
                    let (r, col) = (i as u32 / cover::COVER_N, i as u32 % cover::COVER_N);
                    p.rect_filled(Rect::from_min_size(rect.min + vec2(col as f32 * 5.0, r as f32 * 5.0), vec2(5.0, 5.0)), 0.0, rgb(*c));
                }
            }
            ui.vertical(|ui| {
                let title = track.as_ref().map(|t| t.title.clone()).unwrap_or_else(|| "not playing".into());
                ui.label(RichText::new(ell(&title, 60)).strong().size(16.0));
                let sub = if Instant::now() < self.flash.1 { self.flash.0.clone() }
                    else if self.busy { self.busy_text.clone() }
                    else { track.as_ref().map(|t| t.artist.clone()).unwrap_or_default() };
                ui.label(RichText::new(ell(&sub, 70)).color(DIM));
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                let first = track.as_ref().and_then(|t| split_artists(&t.artist).into_iter().next());
                let banned = first.as_ref().map(|a| self.taste.is_banned(a)).unwrap_or(false);
                let liked = track.as_ref().map(|t| self.taste.is_liked(&t.id)).unwrap_or(false);
                if tbtn(ui, &format!("[{} autosave]", if self.autosave { "x" } else { " " }), if self.autosave { WHITE } else { DIM }, acc).clicked() {
                    self.autosave = !self.autosave;
                    if self.autosave { self.flash("Autosave on: songs you listen to are saved", 3.0); }
                }
                if tbtn(ui, if banned { "[unban]" } else { "[never]" }, WHITE, acc).clicked() { self.never_current(); }
                if tbtn(ui, if liked { "[liked]" } else { "[like]" }, if liked { acc } else { WHITE }, acc).clicked() { self.like_current(); }
                let mut status = vec![];
                if self.offline() { status.push(if self.settings.offline { "offline mode".to_string() } else { "offline".into() }); }
                if let Some(end) = self.sleep_end { status.push(format!("sleep in {}", fmt(end.saturating_duration_since(Instant::now()).as_secs_f64()))); }
                if !status.is_empty() { ui.label(RichText::new(status.join("   ")).color(acc).size(12.0)); }
            });
        });

        // seek bar
        let len = if self.busy { 0.0 } else { self.length };
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click_and_drag());
        let ptr_frac = resp.interact_pointer_pos().map(|p| ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
        if resp.dragged() && self.has_track && len > 0.0 { self.drag_frac = ptr_frac; }
        let shown = self.drag_frac.map(|f| f as f64).unwrap_or(if len > 0.0 { (self.pos / len).min(1.0) } else { 0.0 });
        if resp.drag_stopped() {
            if let Some(f) = self.drag_frac.take() { self.seek_to(f as f64 * len); }
        } else if resp.clicked() {
            if let Some(f) = ptr_frac { self.seek_to(f as f64 * len); }
        }
        let cy = rect.center().y;
        let x = rect.left() + shown as f32 * rect.width();
        let p = ui.painter();
        p.rect_filled(Rect::from_min_max(pos2(rect.left(), cy - 3.0), pos2(rect.right(), cy + 3.0)), 0.0, TRACK);
        p.rect_filled(Rect::from_min_max(pos2(rect.left(), cy - 3.0), pos2(x, cy + 3.0)), 0.0, acc);
        p.rect_filled(Rect::from_min_max(pos2(x - 3.0, cy - 9.0), pos2(x + 3.0, cy + 9.0)), 0.0, WHITE);
        let pos = shown * len;
        let (r2, _) = ui.allocate_exact_size(vec2(rect.width(), 16.0), Sense::hover());
        let font = FontId::monospace(12.0);
        ui.painter().text(r2.left_center(), Align2::LEFT_CENTER, fmt(pos), font.clone(), DIM);
        ui.painter().text(r2.right_center(), Align2::RIGHT_CENTER, format!("-{}", fmt(len - pos)), font, DIM);

        // controls
        ui.horizontal(|ui| {
            if tbtn(ui, &format!("[{}] autoplay", if self.autoplay { "x" } else { " " }), if self.autoplay { WHITE } else { DIM }, acc).clicked() { self.toggle_autoplay(); }
            if tbtn(ui, "[prev]", WHITE, acc).clicked() { self.prev(); }
            let playing = self.has_track && matches!(self.state, PState::Playing) || self.busy;
            if tbtn(ui, if playing { "[pause]" } else { "[play]" }, WHITE, acc).clicked() { self.toggle(); }
            if tbtn(ui, "[next]", WHITE, acc).clicked() { self.next(); }
            if tbtn(ui, "[stop]", WHITE, acc).clicked() { self.stop(); }
            if tbtn(ui, "[save]", WHITE, acc).clicked() { self.save_current(); }
            if tbtn(ui, &format!("[shuffle: {}]", if self.shuffle { "on" } else { "off" }), if self.shuffle { WHITE } else { DIM }, acc).clicked() { self.toggle_shuffle(); }
            let rep = match self.repeat { Repeat::Off => "off", Repeat::All => "all", Repeat::One => "one" };
            if tbtn(ui, &format!("[repeat: {rep}]"), if self.repeat == Repeat::Off { DIM } else { WHITE }, acc).clicked() { self.cycle_repeat(); }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().slider_width = 110.0;
                let mut v = self.volume;
                if ui.add(egui::Slider::new(&mut v, 0.0..=100.0).integer().suffix("%")).changed() { self.set_volume(v); }
                ui.label(RichText::new("vol").color(DIM));
            });
        });
    }

    /// Results, queue and library share one list.
    fn draw_tracks(&mut self, ui: &mut egui::Ui, acc: Color32) {
        let cur_id = self.current_track().map(|t| t.id.clone());
        let playing_now = cur_id.is_some();
        let view = self.view;
        let act: Option<(usize, RowAct)> = {
            let (start, rows, msg): (usize, Vec<&Track>, String) = match view {
                View::Queue => {
                    let (s, r) = self.queue_rows();
                    (s, r.iter().collect(), "The queue is empty\nSongs you play, and the autoplay mix, show up here".into())
                }
                View::Library => (0, self.library.iter().collect(), if self.library_loading { "Scanning saved songs...".into() } else if !self.library_all.is_empty() {
                    format!("No saved songs match '{}'\nClear the search box and press Enter to see them all", self.library_filter)
                } else { "No saved songs yet\nPress [save] on a song, or turn on [autosave]".into() }),
                _ => (0, self.results.iter().collect(), self.list_msg.clone()),
            };
            let _ = start;
            if rows.is_empty() {
                for (n, line) in msg.lines().enumerate() {
                    ui.label(RichText::new(format!("{} {line}", if n == 0 { "$" } else { "#" })).color(if n == 0 { WHITE } else { DIM }));
                }
                None
            } else {
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut act = None;
                egui::ScrollArea::vertical().id_salt(match view { View::Queue => "queue", View::Library => "library", _ => "results" })
                    .auto_shrink([false; 2]).show_rows(ui, ROW, rows.len(), |ui, range| {
                        for k in range {
                            let t = rows[k];
                            let (label, playing, actions) = match view {
                                View::Queue => (if playing_now && k == 0 { ">>".to_string() } else { format!("{:02}", k + if playing_now { 0 } else { 1 }) }, playing_now && k == 0, if playing_now && k > 0 { 1 } else { 0 }),
                                _ => (if cur_id.as_deref() == Some(t.id.as_str()) { ">>".into() } else { format!("{:02}", k + 1) }, cur_id.as_deref() == Some(t.id.as_str()), 0),
                            };
                            let a = track_row(ui, t, &label, playing, actions, acc);
                            if a != RowAct::None { act = Some((k, a)); }
                        }
                    });
                act
            }
        };
        let Some((k, a)) = act else { return };
        match view {
            View::Results => match a {
                RowAct::Play => self.play_from_results(k),
                RowAct::Right => { if let Some(t) = self.results.get(k).cloned() { self.queue_next(t) } }
                _ => {}
            },
            View::Library => match a {
                RowAct::Play => { let l = self.library.clone(); self.play_list(l, k, None) }
                RowAct::Right => { if let Some(t) = self.library.get(k).cloned() { self.queue_next(t) } }
                _ => {}
            },
            _ => {
                let start = self.queue_rows().0;
                let qi = start + k;
                if playing_now && k == 0 { return; }
                match a {
                    RowAct::Play => self.play_index(qi),
                    RowAct::Remove => {
                        if qi < self.queue.len() { self.queue.remove(qi); }
                        if self.has_track && self.autoplay && self.index + 1 >= self.queue.len() { self.request_mix(false) } else { self.prefetch_next() }
                    }
                    RowAct::Up => {
                        if qi < self.queue.len() {
                            let t = self.queue.remove(qi);
                            self.queue.insert((self.index + 1).min(self.queue.len()), t);
                            self.prefetch_next();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn lyric_index(&self, l: &Lyrics) -> i64 {
        if l.state != LState::Ok || !l.synced { return -1; }
        l.lines.partition_point(|(t, _)| t.unwrap_or(0.0) <= self.pos + 0.3) as i64 - 1
    }

    fn draw_lyrics(&mut self, ui: &mut egui::Ui, acc: Color32) {
        let Some(l) = self.lyrics.clone() else {
            ui.label(RichText::new("$ play a song to see its lyrics").color(WHITE));
            return;
        };
        if l.state != LState::Ok {
            ui.label(RichText::new(format!("$ {}", match l.state {
                LState::Loading => "fetching lyrics...", LState::NotFound => "no lyrics found for this song",
                LState::Instrumental => "instrumental - no lyrics", _ => "could not reach the lyrics service",
            })).color(WHITE));
            return;
        }
        let idx = self.lyric_index(&l);
        let changed = idx != self.lyr_idx;
        self.lyr_idx = idx;
        let mut seek = None;
        egui::ScrollArea::vertical().id_salt("lyrics").auto_shrink([false; 2]).show(ui, |ui| {
            for (i, (t, text)) in l.lines.iter().enumerate() {
                let cur = l.synced && i as i64 == idx;
                let color = if !l.synced { Color32::from_rgb(196, 196, 196) } else if cur { WHITE } else if (i as i64) < idx { Color32::from_rgb(85, 85, 85) } else { Color32::from_rgb(154, 154, 154) };
                let mut rt = RichText::new(format!("{}{}", if cur { "> " } else { "  " }, if text.is_empty() { "..." } else { text })).size(16.0).color(if cur { acc } else { color });
                if cur { rt = rt.strong(); }
                let r = ui.add(egui::Label::new(rt).sense(Sense::click()));
                if cur && changed { r.scroll_to_me(Some(Align::Center)); }
                if r.clicked() && l.synced { seek = *t; }
            }
        });
        if let Some(when) = seek { self.seek_to(when); }
    }

    fn draw_playlists(&mut self, ui: &mut egui::Ui, acc: Color32) {
        egui::ScrollArea::vertical().id_salt("playlists").auto_shrink([false; 2]).show(ui, |ui| {
            let open = self.pl_open.clone().filter(|n| self.playlists.data.contains_key(n));
            self.pl_open = open.clone();
            match open {
                None => {
                    heading(ui, "$ playlists");
                    note(ui, "# type a name in the search box first, or leave it empty");
                    ui.horizontal(|ui| {
                        if tbtn(ui, "[new]", WHITE, acc).clicked() { self.pl_new(false); }
                        if tbtn(ui, "[from queue]", WHITE, acc).clicked() { self.pl_new(true); }
                    });
                    if !self.pl_note.is_empty() { note(ui, &format!("# {}", self.pl_note)); }
                    let names: Vec<String> = self.playlists.data.keys().cloned().collect();
                    if names.is_empty() { note(ui, "# no playlists yet"); }
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, n) in names.iter().enumerate() {
                        let tracks = &self.playlists.data[n];
                        let right = format!("{}   {}", plural(tracks.len(), "song"), fmt_span(tracks.iter().map(|t| t.duration as f64).sum()));
                        if line_row(ui, &format!("{:02}  {}", i + 1, ell(n, 50)), &right) {
                            self.pl_open = Some(n.clone());
                            self.pl_last = Some(n.clone());
                            self.pl_note.clear();
                        }
                    }
                }
                Some(name) => {
                    let tracks = self.playlists.data.get(&name).cloned().unwrap_or_default();
                    heading(ui, &format!("$ playlists / {}   ({}, {})", ell(&name, 40), plural(tracks.len(), "song"), fmt_span(tracks.iter().map(|t| t.duration as f64).sum())));
                    let sure = self.pl_confirm.as_ref().map(|(n, t)| *n == name && Instant::now() < *t).unwrap_or(false);
                    ui.horizontal_wrapped(|ui| {
                        if tbtn(ui, "[play]", WHITE, acc).clicked() { self.pl_play(&name, 0, false); }
                        if tbtn(ui, "[shuffle]", WHITE, acc).clicked() { self.pl_play(&name, 0, true); }
                        if tbtn(ui, "[export .m3u]", WHITE, acc).clicked() {
                            self.pl_note = match self.playlists.export_m3u(&name, &self.paths.export) {
                                Ok(p) => format!("exported to {}", p.display()),
                                Err(e) => format!("could not export: {e}"),
                            };
                        }
                        if tbtn(ui, if sure { "[sure? delete]" } else { "[delete]" }, if sure { acc } else { WHITE }, acc).clicked() {
                            if sure { self.playlists.delete(&name); self.pl_open = None; self.pl_confirm = None; }
                            else { self.pl_confirm = Some((name.clone(), Instant::now() + Duration::from_secs(3))); }
                        }
                        if tbtn(ui, "[<- back]", WHITE, acc).clicked() { self.pl_open = None; }
                    });
                    let cur = self.current_track().cloned();
                    ui.horizontal(|ui| {
                        if tbtn(ui, "[+ playing]", WHITE, acc).clicked() { self.pl_add(&name, cur.as_slice(), "This song"); }
                        if tbtn(ui, "[+ queue]", WHITE, acc).clicked() { let q = self.queue_rows().1.to_vec(); self.pl_add(&name, &q, "The queue"); }
                    });
                    if !self.pl_note.is_empty() { note(ui, &format!("# {}", ell(&self.pl_note, 100))); }
                    if tracks.is_empty() { note(ui, "# empty: use [+ playing] or [+ queue]"); }
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, t) in tracks.iter().enumerate() {
                        match track_row(ui, t, &format!("{:02}", i + 1), false, 2, acc) {
                            RowAct::Play => self.pl_play(&name, i, false),
                            RowAct::Remove => self.playlists.remove_at(&name, i),
                            _ => {}
                        }
                    }
                }
            }
        });
    }

    fn draw_taste(&mut self, ui: &mut egui::Ui, acc: Color32) {
        egui::ScrollArea::vertical().id_salt("taste").auto_shrink([false; 2]).show(ui, |ui| {
            heading(ui, "$ taste");
            note(ui, "# never play (click to allow again)");
            let banned = self.taste.banned();
            if banned.is_empty() { note(ui, "none yet: press [never] while a song you don't want plays"); }
            ui.spacing_mut().item_spacing.y = 0.0;
            for name in banned {
                if line_row(ui, &name, "[allow]") {
                    self.taste.toggle_ban(&name);
                    self.flash(format!("{name} is allowed again"), 3.0);
                }
            }
            ui.add_space(12.0);
            note(ui, "# liked songs (click to play, [x] removes the like)");
            let liked = self.taste.liked_tracks();
            if liked.is_empty() { note(ui, "none yet: press [like] while a song you love plays"); }
            for (i, t) in liked.iter().enumerate() {
                match track_row(ui, t, &format!("{:02}", i + 1), false, 2, acc) {
                    RowAct::Play => self.play_single(t.clone()),
                    RowAct::Remove => { self.taste.toggle_like(t); self.flash(format!("Like removed: {}", t.title), 3.0); }
                    _ => {}
                }
            }
            ui.add_space(12.0);
            note(ui, "# artists autoplay likes best");
            let artists = self.taste.top_artists(10);
            if artists.is_empty() { note(ui, "nothing yet: it learns as you listen, skip and save"); }
            let top = artists.first().map(|a| a.1).unwrap_or(1.0);
            for (n, v) in &artists { bar_row(ui, n, (v / top) as f32, &format!("{v:+.1}"), acc); }
        });
    }

    fn draw_stats(&mut self, ui: &mut egui::Ui, acc: Color32) {
        let st = self.taste.stats();
        egui::ScrollArea::vertical().id_salt("stats").auto_shrink([false; 2]).show(ui, |ui| {
            heading(ui, "$ stats");
            for (label, (secs, count)) in [("today", st.today), ("last 7 days", st.week), ("all time", st.all)] {
                ui.horizontal(|ui| {
                    ui.add_sized([150.0, 20.0], egui::Label::new(RichText::new(label).color(DIM)));
                    ui.add_sized([120.0, 20.0], egui::Label::new(fmt_span(secs as f64)));
                    ui.label(plural(count, "song"));
                });
            }
            ui.add_space(10.0);
            note(ui, "# top artists this week (by listening time)");
            if st.top_artists.is_empty() { note(ui, "no listening recorded yet"); }
            let top = st.top_artists.first().map(|a| a.1).unwrap_or(1).max(1) as f32;
            for (n, secs) in &st.top_artists { bar_row(ui, n, *secs as f32 / top, &fmt_span(*secs as f64), acc); }
            ui.add_space(10.0);
            note(ui, "# most skipped artists");
            if st.skipped.is_empty() { note(ui, "no skips yet"); }
            for (n, s) in &st.skipped { bar_row(ui, n, 0.0, &plural(*s as usize, "skip"), acc); }
            ui.add_space(10.0);
            note(ui, "# most played songs");
            if st.top_songs.is_empty() { note(ui, "nothing played to the end yet"); }
            for (plays, title, artist) in &st.top_songs { bar_row(ui, &format!("{title} - {artist}"), 0.0, &plural(*plays as usize, "play"), acc); }
        });
    }

    fn draw_audio(&mut self, ui: &mut egui::Ui, acc: Color32) {
        heading(ui, "$ equalizer");
        note(ui, "# drag a slider, or pick a preset");
        let labels = ["pre", "60", "170", "310", "600", "1k", "3k", "6k", "12k", "14k", "16k"];
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = 170.0;
            ui.add_space(20.0);
            for (i, label) in labels.iter().enumerate() {
                ui.vertical(|ui| {
                    let v = if i == 0 { &mut self.settings.eq.preamp } else { &mut self.settings.eq.bands[i - 1] };
                    changed |= ui.add(egui::Slider::new(v, -12.0..=12.0).vertical().step_by(0.5).show_value(true)).changed();
                    ui.label(RichText::new(*label).color(DIM).size(12.0));
                });
            }
        });
        if changed {
            let eq = &self.settings.eq;
            self.settings.eq.preset = EQ_PRESETS.iter().find(|(_, g)| g[..] == eq.bands[..] && eq.preamp == 0.0).map(|(n, _)| n.to_string()).unwrap_or_else(|| "custom".into());
            self.apply_eq();
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (i, (name, _)) in EQ_PRESETS.iter().enumerate() {
                if tbtn(ui, &format!("[{name}]"), if self.settings.eq.preset == *name { acc } else { WHITE }, acc).clicked() { self.set_preset(i); }
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if tbtn(ui, &format!("[{}] loudness matching", if self.settings.loudness { "x" } else { " " }), if self.settings.loudness { WHITE } else { DIM }, acc).clicked() { self.toggle_loudness(); }
            note(ui, "# evens out the volume between songs");
        });
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui, acc: Color32) {
        egui::ScrollArea::vertical().id_salt("settings").auto_shrink([false; 2]).show(ui, |ui| {
            heading(ui, "$ settings");
            ui.add_space(6.0);
            let themes: Vec<(String, Option<Color32>)> = THEMES.iter().map(|(n, c)| (n.to_string(), Some(rgb(*c)))).collect();
            let cur = THEMES.iter().position(|(n, _)| *n == self.settings.theme).unwrap_or(0);
            if let Some(i) = opt_row(ui, "theme", &themes, cur, acc, "") { self.set_theme(THEMES[i].0); }

            let covers: Vec<(String, Option<Color32>)> = COVER_MODES.iter().map(|m| (m.to_string(), None)).collect();
            let cur = COVER_MODES.iter().position(|m| *m == self.settings.cover).unwrap_or(0);
            if let Some(i) = opt_row(ui, "cover art", &covers, cur, acc, "") {
                self.settings.cover = COVER_MODES[i].into();
                self.settings.save();
                self.cover_for = None;
            }
            let fades: Vec<(String, Option<Color32>)> = CROSSFADES.iter().map(|c| (if *c == 0.0 { "off".to_string() } else { format!("{c}s") }, None)).collect();
            let cur = CROSSFADES.iter().position(|c| *c == self.settings.crossfade).unwrap_or(0);
            if let Some(i) = opt_row(ui, "crossfade", &fades, cur, acc, "# songs overlap") {
                self.settings.crossfade = CROSSFADES[i];
                self.settings.save();
            }
            let sleeps: Vec<(String, Option<Color32>)> = SLEEP_MINUTES.iter().map(|m| (if *m == 0 { "off".to_string() } else { format!("{m}m") }, None)).collect();
            let cur = if self.sleep_end.is_some() { SLEEP_MINUTES.iter().position(|m| *m == self.sleep_minutes).unwrap_or(0) } else { 0 };
            let left = self.sleep_end.map(|e| format!("ends in {}", fmt(e.saturating_duration_since(Instant::now()).as_secs_f64()))).unwrap_or_default();
            if let Some(i) = opt_row(ui, "sleep timer", &sleeps, cur, acc, &left) { self.set_sleep(SLEEP_MINUTES[i]); }
            let off = [("automatic".to_string(), None), ("saved songs only".to_string(), None)];
            let status = format!("# now: {}", if self.offline() { "offline" } else { "online" });
            if let Some(i) = opt_row(ui, "offline", &off, self.settings.offline as usize, acc, &status) {
                self.settings.offline = i == 1;
                self.settings.save();
                self.flash(if i == 1 { "Offline mode: only saved songs" } else { "Offline mode off (automatic)" }, 3.0);
            }
            ui.add_space(10.0);
            note(ui, "# keyboard (not while typing in the search box; esc leaves it)");
            let keys = [("space", "play / pause"), ("n  p", "next / previous"), ("left right", "seek 5 s (shift 30 s)"),
                        ("up down", "volume"), ("l", "like"), ("b", "never (ban artist)"), ("s", "save"), ("r", "random song"),
                        ("a", "autoplay"), ("z", "shuffle"), ("y", "repeat"), ("c", "next theme"), ("/", "search"), ("1-9", "open a tab")];
            egui::Grid::new("keys").num_columns(4).spacing([16.0, 2.0]).show(ui, |ui| {
                for (i, (k, what)) in keys.iter().enumerate() {
                    ui.label(RichText::new(*k).color(acc).size(12.0));
                    ui.label(RichText::new(*what).size(12.0));
                    if i % 2 == 1 { ui.end_row(); }
                }
            });
            ui.add_space(10.0);
            note(ui, &format!("# saved songs: {}", self.paths.save.display()));
            note(ui, &format!("# playlist files: {}", self.paths.export.display()));
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        self.media_keys();
        self.tick(ctx);
        self.sync_smtc();
        self.keys(ctx);
        if self.theme_applied != self.settings.theme {
            self.theme_applied = self.settings.theme.clone();
            apply_visuals(ctx, self.accent());
        }
        self.ui(ctx);
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.log_listen(false);
        if let Some(p) = self.player.as_mut() { p.stop(); }
        let _ = std::fs::remove_dir_all(&self.tmpdir);
    }
}

fn main() -> eframe::Result<()> {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}\n{}\n", std::backtrace::Backtrace::force_capture());
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log"))
            .and_then(|mut f| { use std::io::Write; f.write_all(msg.as_bytes()) });
        eprintln!("{msg}");
    }));
    // yt-dlp.exe, ffmpeg.exe and deno.exe placed next to music-player.exe are found first
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let mut paths = vec![dir];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        if let Ok(joined) = std::env::join_paths(paths) {
            std::env::set_var("PATH", joined);
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([940.0, 700.0]).with_min_inner_size([820.0, 600.0]).with_title("music"),
        ..Default::default()
    };
    eframe::run_native("music", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
