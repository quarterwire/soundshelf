//! Loudness (ITU-R BS.1770 / EBU R128 integrated LUFS), sample peak and a small waveform per file.
//! Results are cached on disk keyed by path + size + mtime, so each file is decoded once.

use std::collections::{HashMap, VecDeque};
use std::f64::consts::PI;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use rodio::{Decoder, Source};

pub const WAVE_BUCKETS: usize = 600;
/// YouTube normalizes playback to -14 LUFS.
pub const YT_TARGET: f32 = -14.0;
/// A comfortable level for music/SFX sitting under a voice-over.
pub const BED_TARGET: f32 = -28.0;
/// Keep peaks at least this far below full scale.
pub const PEAK_CEILING: f32 = -1.0;

#[derive(Clone)]
pub struct Analysis {
    /// Integrated loudness in LUFS (`-inf` for silence).
    pub lufs: f32,
    /// Sample peak in dBFS.
    pub peak_db: f32,
    /// Peak amplitude per bucket, 0..=255 linear.
    pub wave: Vec<u8>,
}

/// Gain (dB) to reach `target` LUFS without pushing peaks over the ceiling.
/// The flag is true when the peak ceiling, not the loudness target, set the gain.
pub fn gain_to(lufs: f32, peak_db: f32, target: f32) -> Option<(f32, bool)> {
    if !lufs.is_finite() {
        return None;
    }
    let gain = target - lufs;
    let max = PEAK_CEILING - peak_db;
    Some(if gain > max { (max, true) } else { (gain, false) })
}

// ---------- measurement ----------

struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[1] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[2] * y;
        y
    }
}

/// The BS.1770 K-weighting pre-filter (high shelf + high pass), derived for any sample rate.
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let f0 = 1681.974450955533;
    let g = 3.999843853973347;
    let q = 0.7071752369554196;
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let f0 = 38.13547087602444;
    let q = 0.5003270373238773;
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, hp]
}

fn lufs(power: f64) -> f64 {
    -0.691 + 10.0 * power.log10()
}

pub fn analyze(path: &Path) -> Option<Analysis> {
    // Same track choice as playback, so a file with a silent first track is measured by its real sound.
    if let Some(d) = crate::multitrack::loudest_track(path) {
        return Some(measure(d.channels as usize, d.rate as f64, d.samples.into_iter()));
    }
    let decoder = Decoder::try_from(File::open(path).ok()?).ok()?;
    let channels = decoder.channels().get() as usize;
    let rate = decoder.sample_rate().get() as f64;
    Some(measure(channels, rate, decoder.map(|s| s as f32)))
}

fn measure(channels: usize, rate: f64, samples: impl Iterator<Item = f32>) -> Analysis {
    let channels = channels.max(1);
    let mut filters: Vec<[Biquad; 2]> = (0..channels).map(|_| k_weighting(rate)).collect();

    // Mean square per 100 ms segment (summed over channels); 400 ms blocks overlap by 75%.
    let seg_len = (rate / 10.0).round().max(1.0) as usize;
    let mut segments: Vec<f64> = Vec::new();
    let mut seg_sum = 0.0f64;
    let mut seg_frames = 0usize;

    // Waveform: peak per fixed chunk of frames, resampled to WAVE_BUCKETS at the end.
    let chunk_len = 256usize;
    let mut chunks: Vec<f32> = Vec::new();
    let mut chunk_peak = 0.0f32;
    let mut chunk_frames = 0usize;

    let mut peak = 0.0f32;
    let mut ch = 0usize;
    for s in samples {
        let a = s.abs();
        peak = peak.max(a);
        chunk_peak = chunk_peak.max(a);
        let [f1, f2] = &mut filters[ch];
        let y = f2.run(f1.run(s as f64));
        seg_sum += y * y;
        ch += 1;
        if ch == channels {
            ch = 0;
            seg_frames += 1;
            if seg_frames == seg_len {
                segments.push(seg_sum / seg_len as f64);
                seg_sum = 0.0;
                seg_frames = 0;
            }
            chunk_frames += 1;
            if chunk_frames == chunk_len {
                chunks.push(chunk_peak);
                chunk_peak = 0.0;
                chunk_frames = 0;
            }
        }
    }
    if chunk_frames > 0 {
        chunks.push(chunk_peak);
    }

    let lufs_value = if segments.len() >= 4 {
        let blocks: Vec<f64> = segments.windows(4).map(|w| w.iter().sum::<f64>() / 4.0).collect();
        let abs_gated: Vec<f64> = blocks.into_iter().filter(|&p| lufs(p) > -70.0).collect();
        if abs_gated.is_empty() {
            f64::NEG_INFINITY
        } else {
            let mean = abs_gated.iter().sum::<f64>() / abs_gated.len() as f64;
            let rel = lufs(mean) - 10.0;
            let gated: Vec<f64> = abs_gated.into_iter().filter(|&p| lufs(p) > rel).collect();
            lufs(gated.iter().sum::<f64>() / gated.len() as f64)
        }
    } else {
        // Shorter than one 400 ms block: measure the whole clip ungated.
        let frames = segments.len() * seg_len + seg_frames;
        let total = segments.iter().sum::<f64>() * seg_len as f64 + seg_sum;
        if frames == 0 || total <= 0.0 { f64::NEG_INFINITY } else { lufs(total / frames as f64) }
    };

    let wave = (0..WAVE_BUCKETS)
        .map(|b| {
            if chunks.is_empty() {
                return 0;
            }
            let lo = b * chunks.len() / WAVE_BUCKETS;
            let hi = ((b + 1) * chunks.len() / WAVE_BUCKETS).max(lo + 1).min(chunks.len());
            let p = chunks[lo..hi].iter().copied().fold(0.0f32, f32::max);
            (p.clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect();

    Analysis {
        lufs: lufs_value as f32,
        peak_db: if peak > 0.0 { 20.0 * peak.log10() } else { f32::NEG_INFINITY },
        wave,
    }
}

// ---------- disk cache ----------

pub struct Cache {
    map: HashMap<PathBuf, (u64, u64, Analysis)>,
    out: Option<BufWriter<File>>,
}

fn cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("XDG_CONFIG_HOME"))?;
    Some(PathBuf::from(base).join("SoundShelf").join("analysis.bin"))
}

/// Bumped when measurement changes (02: multi-track files measure their audible track).
const MAGIC: &[u8; 8] = b"SSHELF02";

impl Cache {
    pub fn load() -> Self {
        let mut map = HashMap::new();
        let mut records = 0usize;
        if let Some(mut r) = cache_path().and_then(|p| File::open(p).ok()).map(BufReader::new) {
            let mut magic = [0u8; 8];
            if r.read_exact(&mut magic).is_ok() && &magic == MAGIC {
                while let Some((path, size, mtime, a)) = read_record(&mut r) {
                    map.insert(path, (size, mtime, a));
                    records += 1;
                }
            } else {
                // Old format: start over (appending to it would keep the old header).
                drop(r);
                if let Some(p) = cache_path() {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        let mut cache = Self { map, out: None };
        // Rewrite when the append log has a lot of superseded records.
        if records > 0 && records > cache.map.len() * 2 {
            cache.rewrite();
        }
        cache
    }

    pub fn get(&self, path: &Path, size: u64, mtime: u64) -> Option<&Analysis> {
        self.map.get(path).filter(|(s, m, _)| *s == size && *m == mtime).map(|(_, _, a)| a)
    }

    pub fn insert(&mut self, path: PathBuf, size: u64, mtime: u64, a: Analysis) {
        if self.out.is_none() {
            self.out = open_append();
        }
        if let Some(out) = &mut self.out {
            let _ = write_record(out, &path, size, mtime, &a);
        }
        self.map.insert(path, (size, mtime, a));
    }

    pub fn flush(&mut self) {
        if let Some(out) = &mut self.out {
            let _ = out.flush();
        }
    }

    fn rewrite(&mut self) {
        let Some(path) = cache_path() else { return };
        let tmp = path.with_extension("tmp");
        let ok = (|| -> std::io::Result<()> {
            let mut w = BufWriter::new(File::create(&tmp)?);
            w.write_all(MAGIC)?;
            for (p, (size, mtime, a)) in &self.map {
                write_record(&mut w, p, *size, *mtime, a)?;
            }
            w.flush()
        })();
        if ok.is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn open_append() -> Option<BufWriter<File>> {
    let path = cache_path()?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    let fresh = !path.exists();
    let mut f = OpenOptions::new().create(true).append(true).open(path).ok()?;
    if fresh {
        f.write_all(MAGIC).ok()?;
    }
    Some(BufWriter::new(f))
}

fn write_record(w: &mut impl Write, path: &Path, size: u64, mtime: u64, a: &Analysis) -> std::io::Result<()> {
    let p = path.to_string_lossy();
    w.write_all(&(p.len() as u32).to_le_bytes())?;
    w.write_all(p.as_bytes())?;
    w.write_all(&size.to_le_bytes())?;
    w.write_all(&mtime.to_le_bytes())?;
    w.write_all(&a.lufs.to_le_bytes())?;
    w.write_all(&a.peak_db.to_le_bytes())?;
    w.write_all(&(a.wave.len() as u16).to_le_bytes())?;
    w.write_all(&a.wave)
}

fn read_record(r: &mut impl Read) -> Option<(PathBuf, u64, u64, Analysis)> {
    fn arr<const N: usize>(r: &mut impl Read) -> Option<[u8; N]> {
        let mut b = [0u8; N];
        r.read_exact(&mut b).ok()?;
        Some(b)
    }
    let len = u32::from_le_bytes(arr(r)?) as usize;
    if len > 32 * 1024 {
        return None;
    }
    let mut p = vec![0u8; len];
    r.read_exact(&mut p).ok()?;
    let size = u64::from_le_bytes(arr(r)?);
    let mtime = u64::from_le_bytes(arr(r)?);
    let lufs = f32::from_le_bytes(arr(r)?);
    let peak_db = f32::from_le_bytes(arr(r)?);
    let n = u16::from_le_bytes(arr(r)?) as usize;
    let mut wave = vec![0u8; n];
    r.read_exact(&mut wave).ok()?;
    Some((PathBuf::from(String::from_utf8(p).ok()?), size, mtime, Analysis { lufs, peak_db, wave }))
}

// ---------- background workers ----------

pub struct Job {
    pub generation: u64,
    pub idx: usize,
    pub path: PathBuf,
}

pub struct Done {
    pub generation: u64,
    pub idx: usize,
    pub analysis: Option<Analysis>,
}

#[derive(Default)]
struct Queue {
    urgent: Option<Job>,
    jobs: VecDeque<Job>,
}

/// A small pool that analyzes files in the background, at low OS priority.
pub struct Analyzer {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    pub rx: Receiver<Done>,
}

impl Analyzer {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let (tx, rx) = channel::<Done>();
        let workers = std::thread::available_parallelism().map(|n| (n.get() / 2).clamp(1, 3)).unwrap_or(1);
        for _ in 0..workers {
            let (queue, tx, ctx) = (queue.clone(), tx.clone(), ctx.clone());
            std::thread::spawn(move || worker(queue, tx, ctx));
        }
        Self { queue, rx }
    }

    /// Replaces the background queue.
    pub fn set_jobs(&self, jobs: Vec<Job>) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.jobs = jobs.into();
        cv.notify_all();
    }

    /// Analyzes this file next (e.g. the one the user just selected).
    pub fn prioritize(&self, job: Job) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.urgent = Some(job);
        cv.notify_one();
    }

    pub fn pending(&self) -> usize {
        let q = self.queue.0.lock().unwrap();
        q.jobs.len() + q.urgent.is_some() as usize
    }
}

fn worker(queue: Arc<(Mutex<Queue>, Condvar)>, tx: Sender<Done>, ctx: eframe::egui::Context) {
    lower_priority();
    let (lock, cv) = &*queue;
    loop {
        let job = {
            let mut q = lock.lock().unwrap();
            loop {
                if let Some(j) = q.urgent.take().or_else(|| q.jobs.pop_front()) {
                    break j;
                }
                q = cv.wait(q).unwrap();
            }
        };
        let analysis = analyze(&job.path);
        if tx.send(Done { generation: job.generation, idx: job.idx, analysis }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

#[cfg(windows)]
fn lower_priority() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
}

#[cfg(not(windows))]
fn lower_priority() {}

pub fn fmt_db(db: f32) -> String {
    if db >= 0.05 { format!("+{db:.1} dB") } else { format!("{db:.1} dB") }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a 16-bit PCM WAV of a sine wave.
    fn sine_wav(name: &str, channels: u16, rate: u32, secs: f32, dbfs: f32) -> PathBuf {
        let path = std::env::temp_dir().join(name);
        let frames = (rate as f32 * secs) as u32;
        let amp = 10f32.powf(dbfs / 20.0) * i16::MAX as f32;
        let data_len = frames * channels as u32 * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let s = (amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / rate as f32).sin()) as i16;
            for _ in 0..channels {
                b.extend_from_slice(&s.to_le_bytes());
            }
        }
        std::fs::write(&path, b).unwrap();
        path
    }

    #[test]
    fn ebu_reference_sine() {
        // EBU Tech 3341 case 1: 1 kHz stereo sine at -23 dBFS reads -23.0 LUFS.
        for rate in [44_100, 48_000] {
            let p = sine_wav(&format!("ss-test-{rate}.wav"), 2, rate, 20.0, -23.0);
            let a = analyze(&p).unwrap();
            assert!((a.lufs + 23.0).abs() < 0.1, "{rate} Hz: {}", a.lufs);
            assert!((a.peak_db + 23.0).abs() < 0.1, "peak {}", a.peak_db);
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn short_clip_and_gain() {
        let p = sine_wav("ss-test-short.wav", 2, 48_000, 0.2, -23.0);
        let a = analyze(&p).unwrap();
        assert!((a.lufs + 23.0).abs() < 0.3, "{}", a.lufs);
        let _ = std::fs::remove_file(p);

        assert_eq!(gain_to(-9.0, -0.5, YT_TARGET), Some((-5.0, false)));
        // Boost of 6 dB would clip; capped at the -1 dBFS ceiling.
        assert_eq!(gain_to(-20.0, -3.0, YT_TARGET), Some((2.0, true)));
    }
}
