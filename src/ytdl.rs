//! YouTube video/playlist audio downloads via yt-dlp.
//! yt-dlp is fetched into %APPDATA%\SoundShelf\bin on first use and self-updated before each run.
//! Audio is saved as the original m4a stream, so ffmpeg isn't needed.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";

pub enum Event {
    Status(String),
    /// (item, of) within a playlist.
    Item(u32, u32),
    Percent(f32),
    Saved(String),
    Line(String),
    Finished(Result<(), String>),
}

pub struct Download {
    pub rx: Receiver<Event>,
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
}

impl Download {
    pub fn start(url: String, dest: PathBuf, ctx: eframe::egui::Context) -> Self {
        let (tx, rx) = channel();
        let child = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (c, k) = (child.clone(), cancelled.clone());
        std::thread::spawn(move || {
            let result = run(&url, &dest, &tx, &c, &k, &ctx);
            let result = if k.load(Ordering::Relaxed) { Err("Cancelled".into()) } else { result };
            let _ = tx.send(Event::Finished(result));
            ctx.request_repaint();
        });
        Self { rx, child, cancelled }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
    }
}

fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn bundled_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("SoundShelf").join("bin").join("yt-dlp.exe"))
}

/// Our own copy if present, else one on PATH, else download ours.
fn ensure_ytdlp(tx: &Sender<Event>) -> Result<PathBuf, String> {
    let ours = bundled_path().ok_or("APPDATA is not set")?;
    if ours.exists() {
        let _ = tx.send(Event::Status("Checking for yt-dlp updates…".into()));
        let _ = command(&ours).arg("-U").stdout(Stdio::null()).stderr(Stdio::null()).status();
        return Ok(ours);
    }
    if command(Path::new("yt-dlp")).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()) {
        return Ok(PathBuf::from("yt-dlp"));
    }
    let _ = tx.send(Event::Status("Downloading yt-dlp (first run only)…".into()));
    std::fs::create_dir_all(ours.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = ours.with_extension("part");
    let out = command(Path::new("curl.exe"))
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&tmp)
        .arg(YTDLP_URL)
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("couldn't download yt-dlp: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    std::fs::rename(&tmp, &ours).map_err(|e| e.to_string())?;
    Ok(ours)
}

fn run(
    url: &str,
    dest: &Path,
    tx: &Sender<Event>,
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancelled: &AtomicBool,
    ctx: &eframe::egui::Context,
) -> Result<(), String> {
    let ytdlp = ensure_ytdlp(tx)?;
    if cancelled.load(Ordering::Relaxed) {
        return Ok(());
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let _ = tx.send(Event::Status("Starting…".into()));

    let template = dest.join("%(playlist_title,uploader|YouTube)s").join("%(title)s.%(ext)s");
    let mut child = command(&ytdlp)
        .args(["-f", "bestaudio[ext=m4a]/bestaudio", "--ignore-errors", "--newline", "--no-part", "--color", "never"])
        // Skip anything already downloaded, so re-running a playlist only fetches new videos.
        .arg("--download-archive")
        .arg(dest.join(".soundshelf-archive.txt"))
        .args(["--progress-template", "download:SSPROG %(progress._percent_str)s"])
        .args(["--print", "after_move:SSSAVED %(filepath)s"])
        .arg("-o")
        .arg(template)
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start yt-dlp: {e}"))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    *child_slot.lock().unwrap() = Some(child);

    let err_tx = tx.clone();
    let err_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let err_keep = err_lines.clone();
    let err_thread = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.starts_with("ERROR") {
                err_keep.lock().unwrap().push(line.clone());
            }
            let _ = err_tx.send(Event::Line(line));
        }
    });

    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(p) = line.strip_prefix("SSPROG") {
            if let Ok(v) = p.trim().trim_end_matches('%').trim().parse::<f32>() {
                let _ = tx.send(Event::Percent(v));
            }
        } else if let Some(path) = line.strip_prefix("SSSAVED ") {
            let _ = tx.send(Event::Saved(path.trim().to_owned()));
        } else if let Some(rest) = line.strip_prefix("[download] Downloading item ") {
            // "[download] Downloading item 3 of 25"
            let mut nums = rest.split(" of ").filter_map(|n| n.trim().parse::<u32>().ok());
            if let (Some(a), Some(b)) = (nums.next(), nums.next()) {
                let _ = tx.send(Event::Item(a, b));
            }
        } else {
            let _ = tx.send(Event::Line(line));
        }
        ctx.request_repaint();
    }
    let _ = err_thread.join();

    let status = child_slot.lock().unwrap().take().map(|mut c| c.wait());
    match status {
        Some(Ok(s)) if s.success() => Ok(()),
        // With --ignore-errors yt-dlp exits 1 if some videos failed (private, removed…).
        Some(Ok(_)) => {
            let errs = err_lines.lock().unwrap();
            match errs.len() {
                0 => Err("yt-dlp reported an error".into()),
                n => Err(format!("{n} item(s) failed — last: {}", errs.last().unwrap())),
            }
        }
        Some(Err(e)) => Err(e.to_string()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network test: `cargo test -- --ignored ytdl` with SS_YT_TEST_DIR set to a scratch folder.
    #[test]
    #[ignore]
    fn downloads_audio() {
        let dir = PathBuf::from(std::env::var("SS_YT_TEST_DIR").expect("set SS_YT_TEST_DIR"));
        let dl = Download::start("https://www.youtube.com/watch?v=jNQXAC9IVRw".into(), dir, Default::default());
        let mut saved = Vec::new();
        loop {
            match dl.rx.recv().unwrap() {
                Event::Saved(p) => saved.push(p),
                Event::Finished(r) => {
                    r.unwrap();
                    break;
                }
                Event::Status(s) => eprintln!("status: {s}"),
                Event::Item(..) | Event::Percent(_) | Event::Line(_) => {}
            }
        }
        assert_eq!(saved.len(), 1, "{saved:?}");
        let path = PathBuf::from(&saved[0]);
        assert!(path.exists() && path.extension().is_some_and(|e| e == "m4a"), "{path:?}");
        assert!(crate::audio::probe_duration(&path).is_some_and(|d| d > 15.0));
        assert!(crate::analysis::analyze(&path).is_some_and(|a| a.lufs.is_finite()));
        eprintln!("saved {path:?}");
    }
}
