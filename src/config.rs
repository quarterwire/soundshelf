use std::collections::HashMap;
use std::path::PathBuf;

/// Plain `key=value` lines in %APPDATA%\SoundShelf\config.txt — no serde needed.
pub struct Config {
    pub roots: Vec<PathBuf>,
    pub volume: f32,
    pub autoplay: bool,
    pub download_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self { roots: Vec::new(), volume: 0.8, autoplay: true, download_dir: None }
    }
}

fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("XDG_CONFIG_HOME"))?;
    Some(PathBuf::from(base).join("SoundShelf"))
}

fn path() -> Option<PathBuf> {
    Some(dir()?.join("config.txt"))
}

/// Manual SFX/Music marks, one `sfx<TAB>path` or `music<TAB>path` per line. Paths may be files or folders.
pub fn load_marks() -> HashMap<PathBuf, bool> {
    let Some(text) = dir().and_then(|d| std::fs::read_to_string(d.join("marks.txt")).ok()) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|l| {
            let (kind, path) = l.split_once('\t')?;
            Some((PathBuf::from(path), kind == "sfx"))
        })
        .collect()
}

/// `is_sfx` per path: true = SFX, false = Music.
pub fn save_marks(marks: &HashMap<PathBuf, bool>) {
    let Some(dir) = dir() else { return };
    let mut out = String::new();
    for (path, is_sfx) in marks {
        out.push_str(if *is_sfx { "sfx\t" } else { "music\t" });
        out.push_str(&path.display().to_string());
        out.push('\n');
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("marks.txt"), out);
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return cfg };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "root" => cfg.roots.push(PathBuf::from(value.trim())),
                "volume" => cfg.volume = value.trim().parse().unwrap_or(cfg.volume),
                "autoplay" => cfg.autoplay = value.trim() == "1",
                "download_dir" => cfg.download_dir = Some(PathBuf::from(value.trim())),
                _ => {}
            }
        }
        cfg
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        let mut out = format!("volume={}\nautoplay={}\n", self.volume, if self.autoplay { 1 } else { 0 });
        if let Some(dir) = &self.download_dir {
            out.push_str(&format!("download_dir={}\n", dir.display()));
        }
        for root in &self.roots {
            out.push_str(&format!("root={}\n", root.display()));
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, out);
    }
}
