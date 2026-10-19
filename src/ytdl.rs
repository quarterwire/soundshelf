//! YouTube video/playlist audio downloads via yt-dlp.
//! yt-dlp is fetched into %APPDATA%\SoundShelf\bin on first use and self-updated before each run.
//! Audio is saved as the original m4a stream, so ffmpeg isn't needed.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// yt-dlp.exe re-launches itself as a child process, so killing the process we spawned isn't
/// enough. Every process we start goes into a job that dies with it (and with SoundShelf).
#[cfg(windows)]
struct Job(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for Job {}
#[cfg(windows)]
unsafe impl Sync for Job {}

#[cfg(windows)]
impl Job {
    fn new() -> Self {
        use windows::Win32::System::JobObjects::*;
        unsafe {
            let job = CreateJobObjectW(None, windows::core::PCWSTR::null()).unwrap_or_default();
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let _ = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of_val(&info) as u32,
            );
            Self(job)
        }
    }

    fn adopt(&self, child: &Child) {
        use std::os::windows::io::AsRawHandle;
        let process = windows::Win32::Foundation::HANDLE(child.as_raw_handle());
        unsafe {
            let _ = windows::Win32::System::JobObjects::AssignProcessToJobObject(self.0, process);
        }
    }

    fn kill(&self) {
        unsafe {
            let _ = windows::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(not(windows))]
struct Job;

#[cfg(not(windows))]
impl Job {
    fn new() -> Self {
        Job
    }
    fn adopt(&self, _child: &Child) {}
    fn kill(&self) {}
}

/// Reads lines until EOF, replacing invalid UTF-8 instead of stopping. Stopping early would close
/// the pipe and make yt-dlp die with "[Errno 22] Invalid argument" on its next write.
fn lines_lossy(r: impl std::io::Read) -> impl Iterator<Item = String> {
    let mut r = BufReader::new(r);
    std::iter::from_fn(move || {
        let mut buf = Vec::new();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_owned()),
        }
    })
}

pub fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("SoundShelf").join("youtube-download.log"))
}

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
    job: Arc<Job>,
    cancelled: Arc<AtomicBool>,
}

impl Download {
    pub fn start(url: String, dest: PathBuf, ctx: eframe::egui::Context) -> Self {
        let (tx, rx) = channel();
        let job = Arc::new(Job::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (j, k) = (job.clone(), cancelled.clone());
        std::thread::spawn(move || {
            let result = run(&url, &dest, &tx, &j, &k, &ctx);
            let result = if k.load(Ordering::Relaxed) { Err("Cancelled".into()) } else { result };
            let _ = tx.send(Event::Finished(result));
            ctx.request_repaint();
        });
        Self { rx, job, cancelled }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.job.kill();
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        self.cancel();
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
    job: &Job,
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
        // Downloads go to .part files until complete, so a cancelled one never leaves a broken .m4a
        // in the library (the scanner ignores .part).
        .args(["-f", "bestaudio[ext=m4a]/bestaudio", "--ignore-errors", "--newline", "--color", "never"])
        // The frozen yt-dlp.exe ignores PYTHONUTF8 and would otherwise print in the ANSI code page.
        .args(["--encoding", "utf-8"])
        // --print below implies --quiet, which would hide the progress and "item N of M" lines we parse.
        .args(["--no-quiet", "--progress"])
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

    job.adopt(&child);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Full yt-dlp output, for troubleshooting failed downloads.
    let log = Arc::new(Mutex::new(log_path().and_then(|p| File::create(p).ok())));
    let write_log = {
        let log = log.clone();
        move |line: &str| {
            if let Some(f) = log.lock().unwrap().as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    };

    let err_tx = tx.clone();
    let err_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let err_keep = err_lines.clone();
    let err_log = write_log.clone();
    let err_thread = std::thread::spawn(move || {
        for line in lines_lossy(stderr) {
            err_log(&line);
            if line.starts_with("ERROR") {
                err_keep.lock().unwrap().push(line.clone());
            }
            if !line.starts_with("WARNING") {
                let _ = err_tx.send(Event::Line(line));
            }
        }
    });

    for line in lines_lossy(stdout) {
        if !line.starts_with("SSPROG") {
            write_log(&line);
        }
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

    match child.wait() {
        Ok(s) if s.success() => Ok(()),
        // With --ignore-errors yt-dlp exits 1 if some videos failed (private, removed…).
        Ok(_) => {
            let errs = err_lines.lock().unwrap();
            match errs.len() {
                0 => Err("yt-dlp reported an error".into()),
                n => Err(format!("{n} item(s) failed — last: {}", errs.last().unwrap())),
            }
        }
        Err(e) => Err(e.to_string()),
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
