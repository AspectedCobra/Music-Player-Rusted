use rodio::source::SeekError;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::f32::consts::PI;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// VLC's 10 equalizer bands (Hz), the same ones the Python version showed.
pub const EQ_FREQS: [f32; 10] = [60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0];

// ------------------------------------------------------------------ equalizer
pub struct EqShared {
    version: AtomicU32,
    params: Mutex<(f32, [f32; 10])>, // preamp dB, band gains dB
}

impl EqShared {
    fn new() -> Arc<Self> {
        Arc::new(EqShared { version: AtomicU32::new(1), params: Mutex::new((0.0, [0.0; 10])) })
    }
    pub fn set(&self, preamp: f32, bands: &[f32]) {
        let mut p = self.params.lock().unwrap();
        p.0 = preamp;
        for (i, b) in bands.iter().take(10).enumerate() {
            p.1[i] = *b;
        }
        self.version.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Default)]
struct Biquad { b0: f32, b1: f32, b2: f32, a1: f32, a2: f32, z1: f32, z2: f32 }

impl Biquad {
    /// RBJ peaking filter
    fn peaking(sr: f32, f: f32, gain_db: f32, q: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * f / sr;
        let alpha = w0.sin() / (2.0 * q);
        let c = w0.cos();
        let a0 = 1.0 + alpha / a;
        Biquad {
            b0: (1.0 + alpha * a) / a0, b1: -2.0 * c / a0, b2: (1.0 - alpha * a) / a0,
            a1: -2.0 * c / a0, a2: (1.0 - alpha / a) / a0, z1: 0.0, z2: 0.0,
        }
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Wraps a source and runs it through preamp + 10 peaking filters. Settings can change while
/// the song plays (the shared version number is polled every 256 samples).
pub struct EqSource<S: Source<Item = f32>> {
    inner: S,
    shared: Arc<EqShared>,
    seen: u32,
    active: bool,
    pre: f32,
    filt: Vec<Vec<Biquad>>, // per channel
    ch: usize,
    idx: usize,
    n: u32,
}

impl<S: Source<Item = f32>> EqSource<S> {
    fn new(inner: S, shared: Arc<EqShared>) -> Self {
        let mut s = EqSource { inner, shared, seen: 0, active: false, pre: 1.0, filt: vec![], ch: 1, idx: 0, n: 0 };
        s.rebuild();
        s
    }
    fn rebuild(&mut self) {
        let (pre, bands) = *self.shared.params.lock().unwrap();
        self.seen = self.shared.version.load(Ordering::Relaxed);
        let sr = self.inner.sample_rate() as f32;
        self.ch = self.inner.channels().max(1) as usize;
        self.idx = 0;
        self.pre = 10f32.powf(pre / 20.0);
        let chain: Vec<Biquad> = bands.iter().enumerate()
            .filter(|(i, g)| g.abs() > 0.01 && EQ_FREQS[*i] < sr * 0.45)
            .map(|(i, g)| Biquad::peaking(sr, EQ_FREQS[i], *g, 1.2)).collect();
        self.active = pre.abs() > 0.01 || !chain.is_empty();
        self.filt = vec![chain; self.ch];
    }
}

impl<S: Source<Item = f32>> Iterator for EqSource<S> {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let s = self.inner.next()?;
        self.n = self.n.wrapping_add(1);
        if self.n & 255 == 0 && self.shared.version.load(Ordering::Relaxed) != self.seen {
            self.rebuild();
        }
        let ch = self.idx;
        self.idx = (self.idx + 1) % self.ch;
        if !self.active {
            return Some(s);
        }
        let mut x = s * self.pre;
        for f in self.filt[ch].iter_mut() {
            x = f.process(x);
        }
        Some(x.clamp(-1.0, 1.0))
    }
}

impl<S: Source<Item = f32>> Source for EqSource<S> {
    fn current_frame_len(&self) -> Option<usize> { self.inner.current_frame_len() }
    fn channels(&self) -> u16 { self.inner.channels() }
    fn sample_rate(&self) -> u32 { self.inner.sample_rate() }
    fn total_duration(&self) -> Option<Duration> { self.inner.total_duration() }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let r = self.inner.try_seek(pos);
        for c in self.filt.iter_mut() {
            for f in c.iter_mut() {
                f.z1 = 0.0;
                f.z2 = 0.0;
            }
        }
        r
    }
}

// --------------------------------------------------------------------- player
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PState { Idle, Playing, Paused, Ended }

struct Fade { old: Sink, start: Instant, secs: f32, old_vol: f32 }

pub struct Player {
    _stream: OutputStream,
    handle: OutputStreamHandle,
    sink: Option<Sink>,
    fade: Option<Fade>,
    eq: Arc<EqShared>,
    base: f32,    // the volume slider, 0-100
    gain_db: f32, // loudness matching for the current song
    sleep: f32,   // < 1 while the sleep timer fades the music out
}

impl Player {
    pub fn new() -> Result<Self, String> {
        let (stream, handle) = OutputStream::try_default().map_err(|e| format!("no audio output: {e}"))?;
        Ok(Player { _stream: stream, handle, sink: None, fade: None, eq: EqShared::new(), base: 100.0, gain_db: 0.0, sleep: 1.0 })
    }

    fn vol(&self, gain_db: f32) -> f32 {
        (self.base / 100.0 * 10f32.powf(gain_db / 20.0) * self.sleep).clamp(0.0, 2.0)
    }
    fn apply_volume(&self) {
        if let Some(s) = &self.sink {
            s.set_volume(self.vol(self.gain_db));
        }
    }

    pub fn set_eq(&self, preamp: f32, bands: &[f32]) { self.eq.set(preamp, bands); }
    pub fn set_volume(&mut self, pct: f32) { self.base = pct; self.apply_volume(); }
    pub fn set_gain(&mut self, db: f32) { self.gain_db = db; self.apply_volume(); }
    pub fn set_sleep_factor(&mut self, f: f32) { self.sleep = f.clamp(0.0, 1.0); self.apply_volume(); }

    /// Start a song. With fade > 0 and a song already playing, the new one fades in while the
    /// old one fades out (equal-power crossfade, driven by tick_fade()).
    pub fn play(&mut self, path: &Path, gain_db: f32, fade_secs: f32) -> Result<(), String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let decoder = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Decoder::new(std::io::BufReader::new(file))))
        .map_err(|_| "the audio decoder crashed on this file".to_string())?
        .map_err(|e| format!("cannot decode {}: {e}", path.display()))?;
        let source = EqSource::new(decoder.convert_samples::<f32>(), self.eq.clone());
        let sink = Sink::try_new(&self.handle).map_err(|e| e.to_string())?;
        let fading = fade_secs > 0.0 && self.status().0 == PState::Playing;
        if let Some(f) = self.fade.take() {
            f.old.stop(); // a fade that was still running
        }
        let old = self.sink.take();
        self.gain_db = gain_db;
        match old {
            Some(o) if fading => {
                let old_vol = o.volume();
                sink.set_volume(0.0);
                self.fade = Some(Fade { old: o, start: Instant::now(), secs: fade_secs, old_vol });
            }
            Some(o) => {
                o.stop();
                sink.set_volume(self.vol(gain_db));
            }
            None => sink.set_volume(self.vol(gain_db)),
        }
        sink.append(source);
        self.sink = Some(sink);
        Ok(())
    }

    /// Call often (every UI tick) to run a crossfade.
    pub fn tick_fade(&mut self) {
        let mut done = false;
        if let Some(f) = &self.fade {
            let t = (f.start.elapsed().as_secs_f32() / f.secs).min(1.0);
            if let Some(s) = &self.sink {
                s.set_volume((self.vol(self.gain_db) * (t * PI / 2.0).sin()).clamp(0.0, 2.0));
            }
            f.old.set_volume((f.old_vol * (t * PI / 2.0).cos()).max(0.0));
            done = t >= 1.0;
        }
        if done {
            if let Some(f) = self.fade.take() {
                f.old.stop();
            }
            self.apply_volume();
        }
    }

    pub fn pause(&self) {
        if let Some(s) = &self.sink { s.pause(); }
        if let Some(f) = &self.fade { f.old.pause(); }
    }
    pub fn resume(&self) {
        if let Some(s) = &self.sink { s.play(); }
        if let Some(f) = &self.fade { f.old.play(); }
    }
    pub fn stop(&mut self) {
        if let Some(f) = self.fade.take() { f.old.stop(); }
        if let Some(s) = self.sink.take() { s.stop(); }
    }
    pub fn seek(&self, secs: f64) {
        if let Some(s) = &self.sink {
            let _ = s.try_seek(Duration::from_secs_f64(secs.max(0.0)));
        }
    }
    pub fn status(&self) -> (PState, f64) {
        match &self.sink {
            None => (PState::Idle, 0.0),
            Some(s) => {
                let pos = s.get_pos().as_secs_f64();
                if s.empty() { (PState::Ended, pos) } else if s.is_paused() { (PState::Paused, pos) } else { (PState::Playing, pos) }
            }
        }
    }
}
