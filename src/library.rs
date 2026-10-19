use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use walkdir::WalkDir;

/// Video containers are listed too: lots of downloaded SFX are audio-only .mp4, and Premiere takes them all.
pub const AUDIO_EXTS: &[&str] =
    &["wav", "mp3", "flac", "ogg", "oga", "m4a", "aac", "aif", "aiff", "opus", "wma", "mp4", "mkv", "webm"];

pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: PathBuf,
    /// Folder relative to its library root, e.g. `Whooshes/Short`.
    pub folder: String,
    pub ext: String,
    pub size: u64,
    /// Modified time (secs since epoch), used to invalidate cached analysis.
    pub mtime: u64,
    pub duration: Option<f32>,
    /// Integrated LUFS and sample peak dBFS once analyzed; NaN lufs means analysis failed.
    pub loudness: Option<(f32, f32)>,
    /// Lowercased `name` + `folder`, for searching.
    pub haystack: String,
}

pub struct DirNode {
    pub name: String,
    pub path: PathBuf,
    pub children: Vec<usize>,
    /// Files in this folder and everything below it.
    pub count: usize,
}

pub enum Msg {
    Scanned { generation: u64, entries: Vec<Entry> },
    Durations { generation: u64, items: Vec<(usize, f32)> },
    ProbeDone { generation: u64 },
}

/// Owns the background scan + duration-probe threads.
pub struct Scanner {
    generation: Arc<AtomicU64>,
    pub found: Arc<AtomicUsize>,
    pub probed: Arc<AtomicUsize>,
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
}

impl Scanner {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            found: Arc::new(AtomicUsize::new(0)),
            probed: Arc::new(AtomicUsize::new(0)),
            tx,
            rx,
        }
    }

    /// Starts a fresh scan, cancelling any scan or probe still running.
    pub fn scan(&self, roots: Vec<PathBuf>, ctx: eframe::egui::Context) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let live = self.generation.clone();
        let found = self.found.clone();
        let probed = self.probed.clone();
        let tx = self.tx.clone();
        found.store(0, Ordering::Relaxed);
        probed.store(0, Ordering::Relaxed);
        std::thread::spawn(move || {
            let mut entries = Vec::new();
            for root in &roots {
                for item in WalkDir::new(root).follow_links(true).into_iter().filter_map(Result::ok) {
                    if live.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    if !item.file_type().is_file() {
                        continue;
                    }
                    let path = item.path();
                    let Some(ext) = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase) else {
                        continue;
                    };
                    if !AUDIO_EXTS.contains(&ext.as_str()) {
                        continue;
                    }
                    let meta = item.metadata().ok();
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let mtime = meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    entries.push(make_entry(root, path, ext, size, mtime));
                    if found.fetch_add(1, Ordering::Relaxed) % 500 == 0 {
                        ctx.request_repaint();
                    }
                }
            }
            let paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
            let _ = tx.send(Msg::Scanned { generation, entries });
            ctx.request_repaint();
            probe_all(paths, generation, live, probed, tx, ctx);
        });
        generation
    }
}

fn make_entry(root: &Path, path: &Path, ext: String, size: u64, mtime: u64) -> Entry {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let root_name = root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
    // Relative to the library folder, so the column shows "SFX/Whooshes" rather than repeating the root.
    let folder = match dir.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => root_name,
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => dir.display().to_string(),
    };
    let haystack = format!("{} {}", name, folder).to_lowercase();
    Entry { path: path.to_path_buf(), name, dir, folder, ext, size, mtime, duration: None, loudness: None, haystack }
}

fn probe_all(
    paths: Vec<PathBuf>,
    generation: u64,
    live: Arc<AtomicU64>,
    probed: Arc<AtomicUsize>,
    tx: Sender<Msg>,
    ctx: eframe::egui::Context,
) {
    // A few workers: probing is mostly I/O latency, so this helps a lot on SSDs and doesn't hurt HDDs much.
    let workers = std::thread::available_parallelism().map(|n| n.get().min(4)).unwrap_or(2);
    let next = Arc::new(AtomicUsize::new(0));
    let paths = Arc::new(paths);
    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let (next, paths, live, probed, tx, ctx) =
                (next.clone(), paths.clone(), live.clone(), probed.clone(), tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let mut batch = Vec::new();
                loop {
                    if live.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(i) else { break };
                    if let Some(d) = crate::audio::probe_duration(path) {
                        batch.push((i, d));
                    }
                    probed.fetch_add(1, Ordering::Relaxed);
                    if batch.len() >= 64 {
                        let _ = tx.send(Msg::Durations { generation, items: std::mem::take(&mut batch) });
                        ctx.request_repaint();
                    }
                }
                if !batch.is_empty() {
                    let _ = tx.send(Msg::Durations { generation, items: batch });
                }
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
    let _ = tx.send(Msg::ProbeDone { generation });
    ctx.request_repaint();
}

/// Builds the folder tree. Returns the arena and the indices of the root nodes.
pub fn build_tree(roots: &[PathBuf], entries: &[Entry]) -> (Vec<DirNode>, Vec<usize>) {
    let mut nodes: Vec<DirNode> = Vec::new();
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    let mut top = Vec::new();
    for root in roots {
        let name = root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
        index.insert(root.clone(), nodes.len());
        top.push(nodes.len());
        nodes.push(DirNode { name, path: root.clone(), children: Vec::new(), count: 0 });
    }

    for entry in entries {
        let Some(root) = roots.iter().find(|r| entry.dir.starts_with(r)) else { continue };
        // Walk from the file's folder up to the root, creating missing nodes and bumping counts.
        // A node is linked to its parent only when it's first created.
        let mut dir = entry.dir.clone();
        let mut new_child: Option<usize> = None;
        loop {
            let (id, created) = match index.get(&dir) {
                Some(&id) => (id, false),
                None => {
                    let id = nodes.len();
                    let name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    nodes.push(DirNode { name, path: dir.clone(), children: Vec::new(), count: 0 });
                    index.insert(dir.clone(), id);
                    (id, true)
                }
            };
            if let Some(c) = new_child.take() {
                nodes[id].children.push(c);
            }
            if created {
                new_child = Some(id);
            }
            nodes[id].count += 1;
            match dir.parent() {
                Some(p) if &dir != root => dir = p.to_path_buf(),
                _ => break,
            }
        }
    }

    let names: Vec<String> = nodes.iter().map(|n| n.name.to_lowercase()).collect();
    for node in &mut nodes {
        node.children.sort_by(|&a, &b| names[a].cmp(&names[b]));
    }
    (nodes, top)
}

pub fn fmt_duration(secs: f32) -> String {
    let s = secs.max(0.0);
    if s < 10.0 {
        format!("{:.1}s", s)
    } else {
        let t = s.round() as u64;
        format!("{}:{:02}", t / 60, t % 60)
    }
}

pub fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / 1024f64.powi(3))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024f64.powi(2))
    } else {
        format!("{:.0} KB", (b / 1024.0).max(1.0))
    }
}
