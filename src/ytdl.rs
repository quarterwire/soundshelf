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

/// How many yt-dlp processes download at once.
pub const PARALLEL: usize = 5;

pub enum Event {
    Status(String),
    /// Items finished (saved, already downloaded, or failed) out of the total.
    Progress(u32, u32),
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
    let log = Log::create();

    // 1. List the videos (fast: titles only, nothing downloaded yet).
    let _ = tx.send(Event::Status("Reading playlist…".into()));
    let entries = list_entries(&ytdlp, url, job, &log)?;
    if cancelled.load(Ordering::Relaxed) {
        return Ok(());
    }

    // 2. Split into batches: per output folder, round-robin across the workers.
    let total = entries.len() as u32;
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (video, folder) in entries {
        match groups.iter_mut().find(|(f, _)| *f == folder) {
            Some((_, urls)) => urls.push(video),
            None => groups.push((folder, vec![video])),
        }
    }
    let mut batches: Vec<(String, Vec<String>)> = Vec::new();
    for (folder, urls) in groups {
        let n = urls.len().min(PARALLEL);
        let mut split: Vec<Vec<String>> = vec![Vec::new(); n];
        for (i, u) in urls.into_iter().enumerate() {
            split[i % n].push(u);
        }
        batches.extend(split.into_iter().map(|b| (folder.clone(), b)));
    }
    let workers = batches.len().min(PARALLEL);
    let _ = tx.send(Event::Status(format!("Downloading · {workers} at a time")));
    let _ = tx.send(Event::Progress(0, total));
    ctx.request_repaint();

    // 3. Run the batches, each in its own yt-dlp process, at most PARALLEL at once.
    let queue = Mutex::new(batches.into_iter().enumerate().collect::<std::collections::VecDeque<_>>());
    let done = std::sync::atomic::AtomicU32::new(0);
    let failures = Mutex::new(Vec::<String>::new());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let Some((n, (folder, urls))) = queue.lock().unwrap().pop_front() else { break };
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    let tick = || {
                        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                        let _ = tx.send(Event::Progress(d.min(total), total));
                        ctx.request_repaint();
                    };
                    if let Err(e) = download_batch(&ytdlp, dest, &folder, &urls, n, job, tx, &log, &tick, &failures) {
                        failures.lock().unwrap().push(e);
                    }
                }
            });
        }
    });

    let failures = failures.into_inner().unwrap();
    match failures.len() {
        0 => Ok(()),
        n => Err(format!("{n} of {total} failed — last: {}", failures.last().unwrap())),
    }
}

/// The last run's full yt-dlp output, shared by all workers.
#[derive(Clone)]
struct Log(Arc<Mutex<Option<File>>>);

impl Log {
    fn create() -> Self {
        Log(Arc::new(Mutex::new(log_path().and_then(|p| File::create(p).ok()))))
    }

    fn line(&self, worker: &str, line: &str) {
        if let Some(f) = self.0.lock().unwrap().as_mut() {
            let _ = writeln!(f, "[{worker}] {line}");
        }
    }
}

/// Spawns yt-dlp with our output conventions, inside the job.
fn spawn(ytdlp: &Path, args: &[std::ffi::OsString], job: &Job) -> Result<Child, String> {
    let child = command(ytdlp)
        .args(["--ignore-errors", "--newline", "--color", "never"])
        // The frozen yt-dlp.exe ignores PYTHONUTF8 and would otherwise print in the ANSI code page.
        .args(["--encoding", "utf-8"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start yt-dlp: {e}"))?;
    job.adopt(&child);
    Ok(child)
}

/// Video URL + output folder name for every item behind `url` (one item for a plain video).
fn list_entries(ytdlp: &Path, url: &str, job: &Job, log: &Log) -> Result<Vec<(String, String)>, String> {
    let args: Vec<std::ffi::OsString> = vec![
        "--flat-playlist".into(),
        "--print".into(),
        "%(webpage_url,url)s\t%(playlist_title,uploader,channel|YouTube)s".into(),
        url.into(),
    ];
    let mut child = spawn(ytdlp, &args, job)?;
    let stderr = child.stderr.take().unwrap();
    let err_log = log.clone();
    let errs = std::thread::spawn(move || {
        lines_lossy(stderr)
            .inspect(|l| err_log.line("list", l))
            .filter(|l| l.starts_with("ERROR"))
            .collect::<Vec<_>>()
    });
    let entries: Vec<(String, String)> = lines_lossy(child.stdout.take().unwrap())
        .inspect(|l| log.line("list", l))
        .filter_map(|l| {
            let (video, folder) = l.split_once('\t')?;
            video.starts_with("http").then(|| (video.to_owned(), folder.to_owned()))
        })
        .collect();
    let _ = child.wait();
    let errs = errs.join().unwrap_or_default();
    if entries.is_empty() {
        return Err(errs.last().cloned().unwrap_or_else(|| "No videos found at that link".into()));
    }
    Ok(entries)
}

/// Makes a playlist/channel title safe as a Windows folder name inside an output template.
fn folder_name(title: &str) -> String {
    let mut s: String = title
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .take(100)
        .collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    if s.trim().is_empty() || s == "NA" {
        s = "YouTube".into();
    }
    // `%` is the template escape character.
    s.replace('%', "%%")
}

#[allow(clippy::too_many_arguments)]
fn download_batch(
    ytdlp: &Path,
    dest: &Path,
    folder: &str,
    urls: &[String],
    n: usize,
    job: &Job,
    tx: &Sender<Event>,
    log: &Log,
    tick: &(dyn Fn() + Sync),
    failures: &Mutex<Vec<String>>,
) -> Result<(), String> {
    let batch_file = std::env::temp_dir().join(format!("soundshelf-batch-{}-{n}.txt", std::process::id()));
    std::fs::write(&batch_file, urls.join("\n")).map_err(|e| e.to_string())?;
    let template = dest.join(folder_name(folder)).join("%(title)s.%(ext)s");
    let args: Vec<std::ffi::OsString> = vec![
        "-f".into(),
        "bestaudio[ext=m4a]/bestaudio".into(),
        // Skip anything already downloaded, so re-running a playlist only fetches new videos.
        // yt-dlp locks the archive file, so the parallel workers can share it.
        "--download-archive".into(),
        dest.join(".soundshelf-archive.txt").into(),
        "--print".into(),
        "after_move:SSSAVED %(filepath)s".into(),
        // --print implies --quiet; keep the normal output for the log and the "already downloaded" lines.
        "--no-quiet".into(),
        "-o".into(),
        template.into(),
        "-a".into(),
        batch_file.clone().into(),
    ];
    let mut child = spawn(ytdlp, &args, job)?;
    let worker = format!("w{n}");

    let stderr = child.stderr.take().unwrap();
    std::thread::scope(|s| {
        s.spawn(|| {
            for line in lines_lossy(stderr) {
                log.line(&worker, &line);
                // A failed video prints "ERROR: Unable to download video: X" and then "ERROR: X";
                // count each failure once.
                if line.starts_with("ERROR") && !line.starts_with("ERROR: Unable to download video") {
                    failures.lock().unwrap().push(line.clone());
                    tick();
                }
                if !line.starts_with("WARNING") {
                    let _ = tx.send(Event::Line(line));
                }
            }
        });
        for line in lines_lossy(child.stdout.take().unwrap()) {
            log.line(&worker, &line);
            if let Some(path) = line.strip_prefix("SSSAVED ") {
                let _ = tx.send(Event::Saved(path.trim().to_owned()));
                tick();
            } else if line.contains("has already been recorded in the archive") {
                tick();
            }
        }
    });
    let _ = child.wait();
    let _ = std::fs::remove_file(&batch_file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_windows_safe() {
        assert_eq!(folder_name("Undertale and Deltarune Battle Themes"), "Undertale and Deltarune Battle Themes");
        assert_eq!(folder_name("Best of: 2024 / 2025?"), "Best of_ 2024 _ 2025_");
        assert_eq!(folder_name("100% Hype..."), "100%% Hype");
        assert_eq!(folder_name("NA"), "YouTube");
        assert_eq!(folder_name("  "), "YouTube");
    }

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
                Event::Progress(..) | Event::Line(_) => {}
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
