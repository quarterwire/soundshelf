use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Margin, Modifiers, Rect, RichText, Sense, Stroke, TextWrapMode,
    Vec2, collapsing_header::CollapsingState, text::LayoutJob,
};

use crate::analysis::{self, Analyzer, BED_TARGET, Cache, Job, YT_TARGET, fmt_db};
use crate::audio::Audio;
use crate::config::Config;
use crate::dragout;
use crate::library::{self, DirNode, Entry, Msg, Scanner, fmt_duration, fmt_size};
use crate::ytdl::{self, Download};

#[derive(Default)]
struct DlState {
    open: bool,
    url: String,
    job: Option<Download>,
    status: String,
    /// Finished items and total, once the playlist has been read.
    item: Option<(u32, u32)>,
    saved: u32,
    last_line: String,
    result: Option<Result<(), String>>,
}

const ROW_H: f32 = 22.0;
/// Clips shorter than this count as SFX, longer as music.
const SFX_MAX_SECS: f32 = 30.0;

const BG: Color32 = Color32::from_rgb(22, 23, 26);
const PANEL: Color32 = Color32::from_rgb(30, 31, 35);
const ROW_ALT: Color32 = Color32::from_rgb(26, 27, 31);
const HOVER: Color32 = Color32::from_rgb(40, 42, 48);
const SELECTED: Color32 = Color32::from_rgb(38, 64, 108);
const ACCENT: Color32 = Color32::from_rgb(98, 160, 255);
const TEXT: Color32 = Color32::from_rgb(222, 224, 228);
const DIM: Color32 = Color32::from_rgb(140, 144, 152);
const WARN: Color32 = Color32::from_rgb(240, 180, 90);
const HOT: Color32 = Color32::from_rgb(240, 100, 90);
const GOOD: Color32 = Color32::from_rgb(120, 200, 140);

#[derive(Clone, Copy, PartialEq)]
enum SortKey {
    Name,
    Folder,
    Loudness,
    Length,
    Kind,
    Type,
    Size,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    All,
    Sfx,
    Music,
}

/// Whether an entry is SFX, and whether that came from a manual mark (file or folder)
/// rather than the length rule. `None` while the length is still unknown.
fn classify(marks: &HashMap<PathBuf, bool>, e: &Entry) -> Option<(bool, bool)> {
    if !marks.is_empty()
        && let Some(&is_sfx) = e.path.ancestors().find_map(|p| marks.get(p))
    {
        return Some((is_sfx, true));
    }
    e.duration.map(|d| (d < SFX_MAX_SECS, false))
}

enum Action {
    Click(usize, Modifiers),
    Play(usize),
    Drag(usize),
    Reveal(PathBuf),
    CopyFiles(usize),
    CopyPath(usize),
    SetFolder(Option<PathBuf>),
    /// Mark the selection containing this row: Some(true) SFX, Some(false) Music, None clear.
    MarkFiles(usize, Option<bool>),
    MarkFolder(PathBuf, Option<bool>),
    Sort(SortKey),
    AddRoot,
    RemoveRoot(PathBuf),
    Rescan,
    TogglePause,
    Stop,
    Seek(f32),
}

pub struct App {
    cfg: Config,
    entries: Vec<Entry>,
    tree: Vec<DirNode>,
    tree_top: Vec<usize>,
    scanner: Scanner,
    generation: u64,
    scanning: bool,
    probing: bool,

    folder: Option<PathBuf>,
    search: String,
    kind: Kind,
    sort: SortKey,
    sort_desc: bool,
    /// Entry indices currently shown, in display order.
    view: Vec<usize>,
    view_dirty: bool,

    selected: HashSet<usize>,
    cursor: Option<usize>,
    anchor: Option<usize>,
    scroll_to_cursor: bool,
    pending_offset: Option<f32>,
    focus_search: bool,
    search_id: egui::Id,

    audio: Audio,
    /// Entry index of the file loaded in the player.
    playing_idx: Option<usize>,
    seek_drag: Option<f32>,
    analyzer: Analyzer,
    cache: Cache,
    dl: DlState,
    /// Manual SFX (true) / Music (false) marks on files or folders.
    marks: HashMap<PathBuf, bool>,
    /// Set after an OS drag: the button-up went to the drop target, so egui never saw it.
    release_pointer: bool,
    message: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let cfg = Config::load();
        let audio = Audio::new(cfg.volume);
        let mut app = Self {
            cfg,
            entries: Vec::new(),
            tree: Vec::new(),
            tree_top: Vec::new(),
            scanner: Scanner::new(),
            generation: 0,
            scanning: false,
            probing: false,
            folder: None,
            search: String::new(),
            kind: Kind::All,
            sort: SortKey::Name,
            sort_desc: false,
            view: Vec::new(),
            view_dirty: true,
            selected: HashSet::new(),
            cursor: None,
            anchor: None,
            scroll_to_cursor: false,
            pending_offset: None,
            focus_search: false,
            search_id: egui::Id::unique("search"),
            audio,
            playing_idx: None,
            seek_drag: None,
            analyzer: Analyzer::new(cc.egui_ctx.clone()),
            cache: Cache::load(),
            dl: DlState::default(),
            marks: crate::config::load_marks(),
            release_pointer: false,
            message: None,
        };
        app.rescan(&cc.egui_ctx);
        app
    }

    fn rescan(&mut self, ctx: &egui::Context) {
        self.cfg.roots.retain(|r| r.is_dir());
        if self.cfg.roots.is_empty() {
            self.entries.clear();
            self.tree.clear();
            self.tree_top.clear();
            self.view_dirty = true;
            return;
        }
        self.generation = self.scanner.scan(self.cfg.roots.clone(), ctx.clone());
        self.scanning = true;
        self.probing = false;
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.scanner.rx.try_recv() {
            match msg {
                Msg::Scanned { generation, entries } if generation == self.generation => {
                    // Keep the selection/cursor across rescans by path.
                    let keep: HashSet<PathBuf> = self.selected.iter().map(|&i| self.entries[i].path.clone()).collect();
                    let cursor = self.cursor.map(|i| self.entries[i].path.clone());
                    let playing = self.playing_idx.map(|i| self.entries[i].path.clone());
                    self.entries = entries;
                    (self.tree, self.tree_top) = library::build_tree(&self.cfg.roots, &self.entries);
                    self.selected = (0..self.entries.len()).filter(|&i| keep.contains(&self.entries[i].path)).collect();
                    self.cursor = cursor.and_then(|p| self.entries.iter().position(|e| e.path == p));
                    self.playing_idx = playing.and_then(|p| self.entries.iter().position(|e| e.path == p));

                    // Fill loudness from the cache; queue everything else for background analysis.
                    let mut jobs = Vec::new();
                    for (idx, e) in self.entries.iter_mut().enumerate() {
                        match self.cache.get(&e.path, e.size, e.mtime) {
                            Some(a) => e.loudness = Some((a.lufs, a.peak_db)),
                            None => jobs.push(Job { generation, idx, path: e.path.clone() }),
                        }
                    }
                    self.analyzer.set_jobs(jobs);
                    self.anchor = self.cursor;
                    if let Some(f) = &self.folder
                        && !self.tree.iter().any(|n| &n.path == f)
                    {
                        self.folder = None;
                    }
                    self.scanning = false;
                    self.probing = true;
                    self.view_dirty = true;
                }
                Msg::Durations { generation, items } if generation == self.generation => {
                    for (i, d) in items {
                        if let Some(e) = self.entries.get_mut(i) {
                            e.duration = Some(d);
                        }
                    }
                    if self.kind != Kind::All {
                        self.view_dirty = true;
                    }
                }
                Msg::ProbeDone { generation } if generation == self.generation => {
                    self.probing = false;
                    if self.sort == SortKey::Length || self.kind != Kind::All {
                        self.view_dirty = true;
                    }
                }
                _ => {}
            }
        }

        let mut got = false;
        while let Ok(done) = self.analyzer.rx.try_recv() {
            if done.generation != self.generation {
                continue;
            }
            let Some(e) = self.entries.get_mut(done.idx) else { continue };
            got = true;
            match done.analysis {
                Some(a) => {
                    e.loudness = Some((a.lufs, a.peak_db));
                    self.cache.insert(e.path.clone(), e.size, e.mtime, a);
                }
                None => e.loudness = Some((f32::NAN, f32::NAN)),
            }
        }
        if got {
            self.cache.flush();
            if self.sort == SortKey::Loudness && self.analyzer.pending() == 0 {
                self.view_dirty = true;
            }
        }
    }

    /// Makes sure the file gets analyzed soon if it hasn't been yet.
    fn want_analysis(&self, idx: usize) {
        if let Some(e) = self.entries.get(idx)
            && e.loudness.is_none()
        {
            self.analyzer.prioritize(Job { generation: self.generation, idx, path: e.path.clone() });
        }
    }

    fn rebuild_view(&mut self) {
        self.view_dirty = false;
        let terms: Vec<String> = self.search.to_lowercase().split_whitespace().map(str::to_owned).collect();
        let entries = &self.entries;
        let marks = &self.marks;
        self.view = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                if let Some(f) = &self.folder
                    && !e.dir.starts_with(f)
                {
                    return false;
                }
                let kind_ok = match self.kind {
                    Kind::All => true,
                    Kind::Sfx => classify(marks, e).is_some_and(|(sfx, _)| sfx),
                    Kind::Music => classify(marks, e).is_some_and(|(sfx, _)| !sfx),
                };
                kind_ok && terms.iter().all(|t| e.haystack.contains(t.as_str()))
            })
            .collect();
        match self.sort {
            SortKey::Name => self.view.sort_by_cached_key(|&i| entries[i].name.to_lowercase()),
            SortKey::Folder => {
                self.view.sort_by_cached_key(|&i| (entries[i].folder.to_lowercase(), entries[i].name.to_lowercase()))
            }
            SortKey::Type => self.view.sort_by_cached_key(|&i| (entries[i].ext.clone(), entries[i].name.to_lowercase())),
            SortKey::Size => self.view.sort_by_key(|&i| entries[i].size),
            SortKey::Kind => self.view.sort_by_cached_key(|&i| {
                (classify(marks, &entries[i]).map(|(sfx, _)| !sfx), entries[i].name.to_lowercase())
            }),
            SortKey::Loudness => self.view.sort_by(|&a, &b| {
                let key = |i: usize| entries[i].loudness.map(|l| l.0).filter(|l| l.is_finite()).unwrap_or(f32::MIN);
                key(a).total_cmp(&key(b))
            }),
            SortKey::Length => self.view.sort_by(|&a, &b| {
                let (a, b) = (entries[a].duration.unwrap_or(f32::MAX), entries[b].duration.unwrap_or(f32::MAX));
                a.total_cmp(&b)
            }),
        }
        if self.sort_desc {
            self.view.reverse();
        }
    }

    fn cursor_pos(&self) -> Option<usize> {
        let c = self.cursor?;
        self.view.iter().position(|&i| i == c)
    }

    fn play(&mut self, idx: usize) {
        let Some(e) = self.entries.get(idx) else { return };
        self.playing_idx = Some(idx);
        self.want_analysis(idx);
        match self.audio.play(&e.path) {
            Ok(()) => self.message = None,
            Err(err) => self.message = Some(format!("Can't play {}: {err}", e.name)),
        }
    }

    /// Moves the cursor to a view position, selecting it (or extending with shift).
    fn move_cursor(&mut self, pos: usize, extend: bool) {
        let Some(&idx) = self.view.get(pos) else { return };
        if extend {
            self.select_range_to(pos);
        } else {
            self.selected.clear();
            self.selected.insert(idx);
            self.anchor = Some(idx);
        }
        self.cursor = Some(idx);
        self.scroll_to_cursor = true;
        self.want_analysis(idx);
        if self.cfg.autoplay && !extend {
            self.play(idx);
        }
    }

    fn select_range_to(&mut self, pos: usize) {
        let anchor = self.anchor.and_then(|a| self.view.iter().position(|&i| i == a)).unwrap_or(pos);
        let (lo, hi) = if anchor <= pos { (anchor, pos) } else { (pos, anchor) };
        self.selected = self.view[lo..=hi].iter().copied().collect();
    }

    /// Paths for an action on `idx`: the whole selection if `idx` is part of it.
    fn targets(&mut self, idx: usize) -> Vec<PathBuf> {
        if !self.selected.contains(&idx) {
            self.selected.clear();
            self.selected.insert(idx);
            self.cursor = Some(idx);
            self.anchor = Some(idx);
        }
        let mut idxs: Vec<usize> = self.view.iter().copied().filter(|i| self.selected.contains(i)).collect();
        if idxs.is_empty() {
            idxs.push(idx);
        }
        idxs.into_iter().map(|i| self.entries[i].path.clone()).collect()
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Click(idx, mods) => {
                self.want_analysis(idx);
                let pos = self.view.iter().position(|&i| i == idx);
                if mods.command {
                    if !self.selected.remove(&idx) {
                        self.selected.insert(idx);
                    }
                    self.anchor = Some(idx);
                    self.cursor = Some(idx);
                } else if mods.shift
                    && let Some(pos) = pos
                {
                    self.select_range_to(pos);
                    self.cursor = Some(idx);
                } else {
                    self.selected.clear();
                    self.selected.insert(idx);
                    self.anchor = Some(idx);
                    self.cursor = Some(idx);
                    if self.cfg.autoplay {
                        self.play(idx);
                    }
                }
            }
            Action::Play(idx) => {
                self.cursor = Some(idx);
                self.play(idx);
            }
            Action::Drag(idx) => {
                let paths = self.targets(idx);
                if let Err(e) = dragout::drag_files(&paths) {
                    self.message = Some(format!("Drag failed: {e}"));
                }
                self.release_pointer = true;
                ctx.request_repaint();
            }
            Action::Reveal(path) => reveal(&path),
            Action::CopyFiles(idx) => {
                let paths = self.targets(idx);
                self.message = Some(match dragout::copy_files(&paths) {
                    Ok(()) => format!("Copied {} file(s) — paste into Premiere's Project panel or Explorer", paths.len()),
                    Err(e) => format!("Copy failed: {e}"),
                });
            }
            Action::CopyPath(idx) => {
                let paths = self.targets(idx);
                let text = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                ctx.copy_text(text);
            }
            Action::SetFolder(f) => {
                self.folder = f;
                self.view_dirty = true;
            }
            Action::MarkFiles(idx, mark) => {
                for path in self.targets(idx) {
                    self.set_mark(path, mark);
                }
                crate::config::save_marks(&self.marks);
                self.view_dirty = true;
            }
            Action::MarkFolder(path, mark) => {
                self.set_mark(path, mark);
                crate::config::save_marks(&self.marks);
                self.view_dirty = true;
            }
            Action::Sort(key) => {
                if self.sort == key {
                    self.sort_desc = !self.sort_desc;
                } else {
                    self.sort = key;
                    self.sort_desc = matches!(key, SortKey::Size);
                }
                self.view_dirty = true;
                self.scroll_to_cursor = true;
            }
            Action::AddRoot => {
                if let Some(dirs) = rfd::FileDialog::new().set_title("Add folders to the library").pick_folders() {
                    self.add_roots(ctx, dirs);
                }
            }
            Action::RemoveRoot(path) => {
                self.cfg.roots.retain(|r| r != &path);
                self.cfg.save();
                if self.folder.as_ref().is_some_and(|f| f.starts_with(&path)) {
                    self.folder = None;
                }
                self.rescan(ctx);
            }
            Action::Rescan => self.rescan(ctx),
            Action::TogglePause => {
                if self.audio.current.is_some() {
                    self.audio.toggle_pause();
                } else if let Some(c) = self.cursor {
                    self.play(c);
                }
            }
            Action::Stop => self.audio.stop(),
            Action::Seek(frac) => {
                if let Some(d) = self.audio.duration {
                    self.audio.seek(d.mul_f32(frac.clamp(0.0, 1.0)));
                }
            }
        }
    }

    fn set_mark(&mut self, path: PathBuf, mark: Option<bool>) {
        match mark {
            Some(is_sfx) => {
                self.marks.insert(path, is_sfx);
            }
            None => {
                self.marks.remove(&path);
            }
        }
    }

    fn add_roots(&mut self, ctx: &egui::Context, dirs: Vec<PathBuf>) {
        for dir in dirs.into_iter().filter(|d| d.is_dir()) {
            // Skip folders already covered by an existing root; drop roots the new one covers.
            if self.cfg.roots.iter().any(|r| dir.starts_with(r)) {
                continue;
            }
            self.cfg.roots.retain(|r| !r.starts_with(&dir));
            self.cfg.roots.push(dir);
        }
        self.cfg.save();
        self.rescan(ctx);
    }

    fn handle_keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let search_focused = ctx.memory(|m| m.has_focus(self.search_id));
        // Another text field (e.g. the download URL box) owns the keyboard: leave all
        // keys, text, copy and paste events to it instead of treating them as shortcuts.
        if !search_focused && ctx.text_edit_focused() {
            return;
        }
        let (mut nav, mut extend) = (None::<isize>, false);
        let page = 20isize;
        ctx.input_mut(|i| {
            for (key, step) in [
                (Key::ArrowDown, 1),
                (Key::ArrowUp, -1),
                (Key::PageDown, page),
                (Key::PageUp, -page),
            ] {
                if i.consume_key(Modifiers::NONE, key) {
                    nav = Some(step);
                } else if i.consume_key(Modifiers::SHIFT, key) {
                    nav = Some(step);
                    extend = true;
                }
            }
            if i.consume_key(Modifiers::COMMAND, Key::F) {
                self.focus_search = true;
            }
            if i.consume_key(Modifiers::NONE, Key::F5) {
                actions.push(Action::Rescan);
            }
            if i.consume_key(Modifiers::NONE, Key::Enter)
                && let Some(c) = self.cursor
            {
                actions.push(Action::Play(c));
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                if search_focused && !self.search.is_empty() {
                    self.search.clear();
                    self.view_dirty = true;
                } else {
                    actions.push(Action::Stop);
                }
            }
            if !search_focused {
                if i.consume_key(Modifiers::NONE, Key::Space) {
                    actions.push(Action::TogglePause);
                }
                if i.consume_key(Modifiers::NONE, Key::Home) {
                    nav = Some(isize::MIN / 2);
                }
                if i.consume_key(Modifiers::NONE, Key::End) {
                    nav = Some(isize::MAX / 2);
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                    let pos = self.audio.position() + Duration::from_secs(5);
                    self.audio.seek(pos);
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                    let pos = self.audio.position().saturating_sub(Duration::from_secs(5));
                    self.audio.seek(pos);
                }
                if i.consume_key(Modifiers::COMMAND, Key::A) {
                    self.selected = self.view.iter().copied().collect();
                }
                // Ctrl+C arrives as a Copy event rather than a key press.
                let mut copy = false;
                i.events.retain(|e| match e {
                    egui::Event::Copy => {
                        copy = true;
                        false
                    }
                    _ => true,
                });
                if copy && let Some(c) = self.cursor {
                    actions.push(Action::CopyFiles(c));
                }
                // Typing anywhere starts a search.
                let typed: String = i
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) if !t.trim().is_empty() && !i.modifiers.command => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                if !typed.is_empty() {
                    self.search.push_str(&typed);
                    self.view_dirty = true;
                    self.focus_search = true;
                    i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                }
            }
        });

        if let Some(step) = nav
            && !self.view.is_empty()
        {
            let cur = self.cursor_pos().map(|p| p as isize).unwrap_or(-1);
            let next = if cur < 0 && step > 0 { 0 } else { (cur + step).clamp(0, self.view.len() as isize - 1) };
            self.move_cursor(next as usize, extend);
        }
    }

    fn ui_top(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .id(self.search_id)
                    .hint_text("🔍  Search names and folders…   (just start typing)")
                    .desired_width(380.0),
            );
            if self.focus_search {
                self.focus_search = false;
                resp.request_focus();
                // Put the caret at the end instead of selecting everything.
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), self.search_id) {
                    let end = egui::text::CCursor::new(self.search.chars().count());
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                    state.store(ui.ctx(), self.search_id);
                }
            }
            if resp.changed() {
                self.view_dirty = true;
            }
            if !self.search.is_empty() && ui.small_button("×").on_hover_text("Clear (Esc)").clicked() {
                self.search.clear();
                self.view_dirty = true;
            }
            ui.add_space(10.0);
            for (kind, label, tip) in [
                (Kind::All, "All", "Everything"),
                (Kind::Sfx, "SFX", "Clips shorter than 30 seconds"),
                (Kind::Music, "Music", "Clips 30 seconds or longer"),
            ] {
                if ui.selectable_label(self.kind == kind, label).on_hover_text(tip).clicked() {
                    self.kind = kind;
                    self.view_dirty = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⟳ Rescan").on_hover_text("F5").clicked() {
                    actions.push(Action::Rescan);
                }
                let label = if self.dl.job.is_some() { "YouTube (downloading…)" } else { "Download from YouTube" };
                if ui.button(label).clicked() {
                    self.dl.open = true;
                }
                if ui
                    .checkbox(&mut self.cfg.autoplay, "Play on select")
                    .on_hover_text("Preview a sound as soon as you click it or arrow onto it")
                    .changed()
                {
                    self.cfg.save();
                }
            });
        });
    }

    fn ui_sidebar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("LIBRARY").small().strong().color(DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+ Add folder").clicked() {
                    actions.push(Action::AddRoot);
                }
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let all = format!("All sounds   {}", self.entries.len());
            if ui.selectable_label(self.folder.is_none(), all).clicked() {
                actions.push(Action::SetFolder(None));
            }
            ui.add_space(2.0);
            for &id in &self.tree_top {
                tree_ui(ui, &self.tree, id, &self.folder, &self.marks, actions, true);
            }
        });
    }

    fn ui_list(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if self.cfg.roots.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(RichText::new("Your sound library is empty").size(20.0).color(TEXT));
                    ui.add_space(8.0);
                    ui.label(RichText::new("Add your music and SFX folders — every subfolder is included.").color(DIM));
                    ui.add_space(14.0);
                    if ui.button(RichText::new("  + Add folder  ").size(16.0)).clicked() {
                        actions.push(Action::AddRoot);
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new("or drop folders onto this window").small().color(DIM));
                });
            });
            return;
        }

        // Header
        let (header, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
        let cols = columns(header);
        let painter = ui.painter();
        painter.line_segment([header.left_bottom(), header.right_bottom()], Stroke::new(1.0, HOVER));
        for (i, (key, title, right)) in [
            (SortKey::Name, "Name", false),
            (SortKey::Folder, "Folder", false),
            (SortKey::Loudness, "LUFS", true),
            (SortKey::Length, "Length", true),
            (SortKey::Kind, "Kind", false),
            (SortKey::Type, "Type", false),
            (SortKey::Size, "Size", true),
        ]
        .into_iter()
        .enumerate()
        {
            let cell = cols[i];
            let resp = ui.interact(cell, ui.scope_id().with(("hdr", i)), Sense::click());
            let color = if resp.hovered() || self.sort == key { TEXT } else { DIM };
            let (pos, align) =
                if right { (cell.right_center(), Align2::RIGHT_CENTER) } else { (cell.left_center(), Align2::LEFT_CENTER) };
            let text_rect = ui.painter().text(pos, align, title, FontId::proportional(12.5), color);
            if self.sort == key {
                // Painted sort arrow: just left of right-aligned titles, just right of the others.
                let cx = if right { text_rect.left() - 8.0 } else { text_rect.right() + 8.0 };
                let c = egui::pos2(cx, text_rect.center().y);
                let (d, w) = if self.sort_desc { (3.0, 4.0) } else { (-3.0, 4.0) };
                let pts = vec![egui::pos2(c.x - w, c.y - d), egui::pos2(c.x + w, c.y - d), egui::pos2(c.x, c.y + d)];
                ui.painter().add(egui::Shape::convex_polygon(pts, ACCENT, Stroke::NONE));
            }
            if resp.clicked() {
                actions.push(Action::Sort(key));
            }
        }

        if self.view.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                let text = if self.scanning {
                    "Scanning…"
                } else if self.probing && self.kind != Kind::All {
                    "Reading clip lengths…"
                } else {
                    "No sounds match"
                };
                ui.label(RichText::new(text).color(DIM));
            });
            return;
        }

        let cursor_pos = self.cursor_pos();
        let playing = self.audio.current.clone();
        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if let Some(off) = self.pending_offset.take() {
            area = area.vertical_scroll_offset(off);
        }
        let mut scrolled = false;
        ui.spacing_mut().item_spacing.y = 0.0;
        let out = area.show_rows(ui, ROW_H, self.view.len(), |ui, range| {
            for pos in range {
                let idx = self.view[pos];
                let e = &self.entries[idx];
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click_and_drag());
                let is_sel = self.selected.contains(&idx);
                let is_playing = playing.as_ref() == Some(&e.path);
                let bg = if is_sel {
                    SELECTED
                } else if resp.hovered() {
                    HOVER
                } else if pos % 2 == 1 {
                    ROW_ALT
                } else {
                    Color32::TRANSPARENT
                };
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::ZERO, bg);
                if cursor_pos == Some(pos) {
                    painter.rect_stroke(rect.shrink(0.5), CornerRadius::ZERO, Stroke::new(1.0, ACCENT.gamma_multiply(0.5)), egui::StrokeKind::Inside);
                }

                let cols = columns(rect);
                let name_color = if is_playing { ACCENT } else { TEXT };
                let mut name_cell = cols[0];
                if is_playing {
                    painter.text(name_cell.left_center(), Align2::LEFT_CENTER, "▶", FontId::proportional(11.0), ACCENT);
                    name_cell.min.x += 16.0;
                }
                cell_text(painter, name_cell, &e.name, name_color, false);
                cell_text(painter, cols[1], &e.folder, DIM, false);
                if let Some((lufs, _)) = e.loudness {
                    let (text, color) = loudness_label(lufs);
                    cell_text(painter, cols[2], &text, color, true);
                }
                let len = e.duration.map(fmt_duration).unwrap_or_default();
                cell_text(painter, cols[3], &len, DIM, true);
                if let Some((sfx, manual)) = classify(&self.marks, e) {
                    // Manual marks are highlighted so you can tell them from the length rule.
                    cell_text(painter, cols[4], if sfx { "SFX" } else { "Music" }, if manual { ACCENT } else { DIM }, false);
                }
                cell_text(painter, cols[5], &e.ext.to_uppercase(), DIM, false);
                cell_text(painter, cols[6], &fmt_size(e.size), DIM, true);

                if resp.drag_started_by(egui::PointerButton::Primary) {
                    actions.push(Action::Drag(idx));
                } else if resp.double_clicked() {
                    actions.push(Action::Play(idx));
                } else if resp.clicked() {
                    actions.push(Action::Click(idx, ui.input(|i| i.modifiers)));
                }
                resp.on_hover_text_at_pointer(e.path.display().to_string()).context_menu(|ui| {
                    if ui.button("▶  Play").clicked() {
                        actions.push(Action::Play(idx));
                    }
                    if ui.button("📋  Copy file  (Ctrl+C)").clicked() {
                        actions.push(Action::CopyFiles(idx));
                    }
                    if ui.button("Copy path").clicked() {
                        actions.push(Action::CopyPath(idx));
                    }
                    if ui.button("📂  Show in Explorer").clicked() {
                        actions.push(Action::Reveal(e.path.clone()));
                    }
                    ui.separator();
                    mark_menu(ui, self.marks.get(&e.path).copied(), |m| actions.push(Action::MarkFiles(idx, m)));
                });

                if self.scroll_to_cursor && cursor_pos == Some(pos) {
                    ui.scroll_to_rect(rect, None);
                    scrolled = true;
                }
            }
        });
        if self.scroll_to_cursor {
            self.scroll_to_cursor = false;
            if !scrolled && let Some(pos) = cursor_pos {
                let view_h = out.inner_rect.height();
                self.pending_offset = Some((pos as f32 * ROW_H - view_h / 2.0).max(0.0));
                ui.ctx().request_repaint();
            }
        }
    }

    fn ui_player(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        // Show the playing file, or the selected one when nothing is playing.
        let live = self.audio.current.is_some();
        let shown = if live { self.playing_idx } else { self.cursor };
        let entry = shown.and_then(|i| self.entries.get(i));
        let wave = entry.and_then(|e| self.cache.get(&e.path, e.size, e.mtime)).map(|a| a.wave.clone());

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let title = entry.map(|e| e.name.clone()).unwrap_or_else(|| "Nothing selected".into());
            // Leave room for the loudness readout and gain advice on the right.
            let title_w = (ui.available_width() - 760.0).max(160.0);
            ui.scope(|ui| {
                ui.set_max_width(title_w);
                ui.add(
                    egui::Label::new(RichText::new(&title).color(if entry.is_some() { TEXT } else { DIM }).strong())
                        .truncate(),
                )
                .on_hover_text(&title);
            });
            let Some(e) = entry else { return };
            match e.loudness {
                None => {
                    ui.label(RichText::new("measuring loudness…").color(DIM));
                }
                Some((lufs, _)) if lufs.is_nan() => {
                    ui.label(RichText::new("couldn't measure this format").color(DIM));
                }
                Some((lufs, peak)) => {
                    let (text, color) = loudness_label(lufs);
                    ui.label(RichText::new(format!("{text} LUFS")).color(color).strong());
                    ui.label(RichText::new(format!("peak {:.1} dBFS", peak)).color(DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Right-to-left layout: this reads "YouTube … Under voice …" on screen.
                        let help = "Gain to apply in Premiere: select the clip(s) → Audio Gain (G) → Adjust Gain by.\n\
                                    YouTube plays everything at about -14 LUFS, so louder uploads get turned down.\n\
                                    Under voice ≈ -28 LUFS keeps music/SFX beds clearly behind dialogue.\n\
                                    Boosts are capped so peaks stay under -1 dBFS.";
                        for (label, target) in [("Under voice", BED_TARGET), ("YouTube", YT_TARGET)] {
                            if let Some((gain, limited)) = analysis::gain_to(lufs, peak, target) {
                                let suffix = if limited { " (peak-limited)" } else { "" };
                                let color = if gain < -0.5 { WARN } else if gain > 0.5 { GOOD } else { TEXT };
                                ui.label(RichText::new(format!("{}{suffix}", fmt_db(gain))).color(color).strong().monospace())
                                    .on_hover_text(help);
                                ui.label(RichText::new(format!("{label} ({target:.0} LUFS):")).color(DIM)).on_hover_text(help);
                                ui.add_space(10.0);
                            }
                        }
                    });
                }
            }
        });
        ui.add_space(2.0);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let icon = if self.audio.is_playing() { "⏸" } else { "▶" };
            if ui.add(egui::Button::new(RichText::new(icon).size(16.0)).min_size(Vec2::splat(30.0))).clicked() {
                actions.push(Action::TogglePause);
            }
            if ui.add(egui::Button::new(RichText::new("⏹").size(16.0)).min_size(Vec2::splat(30.0))).clicked() {
                actions.push(Action::Stop);
            }

            let right_w = 250.0;
            let bar_w = (ui.available_width() - right_w).max(80.0);
            let (bar, resp) = ui.allocate_exact_size(Vec2::new(bar_w, 48.0), Sense::click_and_drag());
            let pos = if live { self.audio.position().as_secs_f32() } else { 0.0 };
            let total = if live { self.audio.duration.map(|d| d.as_secs_f32()) } else { None };
            let mut frac = total.filter(|t| *t > 0.0).map(|t| (pos / t).clamp(0.0, 1.0)).unwrap_or(0.0);
            if let Some(p) = resp.interact_pointer_pos() {
                let f = ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
                self.seek_drag = Some(f);
                frac = f;
            }
            if (resp.drag_stopped() || resp.clicked())
                && let Some(f) = self.seek_drag.take()
            {
                // Clicking the waveform of a file that isn't playing starts it from there.
                if !live && let Some(idx) = shown {
                    actions.push(Action::Play(idx));
                }
                actions.push(Action::Seek(f));
            }
            draw_waveform(ui.painter(), bar, wave.as_deref(), frac, resp.hovered() || resp.dragged());

            let time = match total {
                Some(t) => format!("{} / {}", fmt_duration(pos.min(t)), fmt_duration(t)),
                None if self.audio.current.is_some() => fmt_duration(pos),
                None => String::new(),
            };
            ui.add_sized(Vec2::new(90.0, 30.0), egui::Label::new(RichText::new(time).color(DIM).monospace()));
            ui.label("🔊");
            let mut vol = self.audio.volume();
            let r = ui.add(egui::Slider::new(&mut vol, 0.0..=1.0).show_value(false));
            if r.changed() {
                self.audio.set_volume(vol);
                self.cfg.volume = vol;
            }
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.cfg.save();
            }
        });
    }

    fn download_dir(&self) -> PathBuf {
        if let Some(d) = &self.cfg.download_dir {
            return d.clone();
        }
        if let Some(root) = self.cfg.roots.first() {
            return root.join("YouTube");
        }
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
        home.join("Music").join("SoundShelf")
    }

    fn poll_download(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.dl.job else { return };
        let events: Vec<ytdl::Event> = job.rx.try_iter().collect();
        for ev in events {
            match ev {
                ytdl::Event::Status(s) => self.dl.status = s,
                ytdl::Event::Progress(done, total) => self.dl.item = Some((done, total)),
                ytdl::Event::Saved(path) => {
                    self.dl.saved += 1;
                    let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned());
                    self.dl.last_line = format!("Saved {}", name.unwrap_or(path));
                }
                ytdl::Event::Line(l) => {
                    if !l.trim().is_empty() {
                        self.dl.last_line = l;
                    }
                }
                ytdl::Event::Finished(r) => {
                    self.dl.job = None;
                    self.dl.result = Some(r);
                    if self.dl.saved > 0 {
                        let dir = self.download_dir();
                        if self.cfg.roots.iter().any(|r| dir.starts_with(r)) {
                            self.rescan(ctx);
                        } else {
                            self.add_roots(ctx, vec![dir]);
                        }
                    }
                }
            }
        }
    }

    fn ui_download(&mut self, ctx: &egui::Context) {
        if !self.dl.open {
            return;
        }
        let mut open = true;
        let dest = self.download_dir();
        let running = self.dl.job.is_some();
        egui::Window::new("Download from YouTube").open(&mut open).default_width(540.0).collapsible(false).show(ctx, |ui| {
            ui.label("Paste a YouTube video or playlist link. Audio is saved as .m4a, one folder per playlist.");
            ui.add_space(4.0);
            // The horizontal wrapper keeps the right-to-left row one line tall;
            // on its own it would claim all the remaining height of the window.
            let edit = ui
                .horizontal(|ui| ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!running, egui::Button::new("Paste")).on_hover_text("Paste link from clipboard").clicked()
                        && let Some(text) = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok())
                    {
                        self.dl.url = text.trim().to_owned();
                    }
                    ui.add_enabled(
                        !running,
                        egui::TextEdit::singleline(&mut self.dl.url)
                            .hint_text("https://www.youtube.com/playlist?list=…")
                            .desired_width(f32::INFINITY),
                    )
                }).inner)
                .inner;
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save to").color(DIM));
                if ui.add_enabled(!running, egui::Button::new("Change…").small()).clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_title("Save YouTube downloads to").pick_folder()
                {
                    self.cfg.download_dir = Some(dir);
                    self.cfg.save();
                }
                // Long paths are cut short instead of stretching the window.
                ui.add(egui::Label::new(RichText::new(dest.display().to_string()).monospace()).truncate())
                    .on_hover_text(dest.display().to_string());
            });
            ui.label(
                RichText::new("Videos already downloaded are skipped, so re-run a playlist to grab only the new ones.")
                    .small()
                    .color(DIM),
            );
            ui.label(RichText::new("Only download audio you have the rights to use.").small().color(DIM));
            ui.add_space(6.0);

            if running {
                ui.label(&self.dl.status);
                match self.dl.item {
                    Some((done, total)) => {
                        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(frac).text(format!("{done} of {total}")));
                    }
                    None => {
                        ui.add(egui::ProgressBar::new(0.0).animate(true));
                    }
                }
                if self.dl.saved > 0 {
                    ui.label(RichText::new(format!("{} saved so far", self.dl.saved)).small().color(DIM));
                }
                if !self.dl.last_line.is_empty() {
                    ui.add(egui::Label::new(RichText::new(&self.dl.last_line).small().color(DIM)).truncate());
                }
                if ui.button("Cancel").clicked()
                    && let Some(job) = &self.dl.job
                {
                    job.cancel();
                }
            } else {
                let can = !self.dl.url.trim().is_empty();
                let go = ui.add_enabled(can, egui::Button::new("Download")).clicked() || (enter && can);
                match &self.dl.result {
                    Some(Ok(())) if self.dl.saved == 0 => {
                        ui.label(RichText::new("Nothing new — everything here is already downloaded.").color(GOOD));
                    }
                    Some(Ok(())) => {
                        ui.label(RichText::new(format!("Done — {} file(s) added to your library.", self.dl.saved)).color(GOOD));
                    }
                    Some(Err(e)) => {
                        if self.dl.saved > 0 {
                            ui.label(RichText::new(format!("{} file(s) saved.", self.dl.saved)).color(GOOD));
                        }
                        ui.add(egui::Label::new(RichText::new(e).color(WARN)).wrap());
                    }
                    None => {}
                }
                if go {
                    self.dl = DlState {
                        open: true,
                        url: self.dl.url.trim().to_owned(),
                        status: "Preparing…".into(),
                        ..Default::default()
                    };
                    self.dl.job = Some(Download::start(self.dl.url.clone(), dest.clone(), ctx.clone()));
                }
            }
        });
        self.dl.open = open;
    }

    fn ui_status(&self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            let mut parts = vec![format!("{} sounds", self.entries.len())];
            if self.view.len() != self.entries.len() {
                parts.push(format!("{} shown", self.view.len()));
            }
            if self.selected.len() > 1 {
                parts.push(format!("{} selected", self.selected.len()));
            }
            if self.scanning {
                parts.push(format!("scanning… {} found", self.scanner.found.load(std::sync::atomic::Ordering::Relaxed)));
            } else if self.probing {
                parts.push(format!(
                    "reading lengths {}/{}",
                    self.scanner.probed.load(std::sync::atomic::Ordering::Relaxed),
                    self.entries.len()
                ));
            }
            let pending = self.analyzer.pending();
            if pending > 0 && !self.scanning {
                parts.push(format!("measuring loudness… {pending} left"));
            }
            ui.label(RichText::new(parts.join("  ·  ")).small().color(DIM));
            if let Some(m) = &self.message {
                ui.label(RichText::new(m).small().color(Color32::from_rgb(240, 180, 90)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("Drag rows into Premiere  ·  Space play/pause  ·  Up/Down browse  ·  Left/Right seek  ·  Ctrl+C copy file")
                        .small()
                        .color(DIM),
                );
            });
        });
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if std::mem::take(&mut self.release_pointer) {
            let pos = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
            raw_input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        self.poll_download(&ctx);

        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| p.is_dir()).collect());
        if !dropped.is_empty() {
            self.add_roots(&ctx, dropped);
        }

        let mut actions = Vec::new();
        self.handle_keys(&ctx, &mut actions);
        if self.view_dirty {
            self.rebuild_view();
        }

        if self.audio.finished() {
            self.audio.stop();
        }
        if self.audio.is_playing() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        if self.scanning || self.probing {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(10, 8)))
            .show(ui, |ui| self.ui_top(ui, &mut actions));
        egui::Panel::bottom("status")
            .exact_size(22.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| self.ui_status(ui));
        egui::Panel::bottom("player")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(10, 6)))
            .show(ui, |ui| self.ui_player(ui, &mut actions));
        egui::Panel::left("folders")
            .resizable(true)
            .default_size(250.0)
            .min_size(160.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(8, 8)))
            .show(ui, |ui| self.ui_sidebar(ui, &mut actions));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(6, 4)))
            .show(ui, |ui| self.ui_list(ui, &mut actions));

        self.ui_download(&ctx);

        for action in actions {
            self.apply(&ctx, action);
        }
        if self.view_dirty {
            ctx.request_repaint();
        }
    }
}

fn tree_ui(
    ui: &mut egui::Ui,
    nodes: &[DirNode],
    id: usize,
    selected: &Option<PathBuf>,
    marks: &HashMap<PathBuf, bool>,
    actions: &mut Vec<Action>,
    is_root: bool,
) {
    let node = &nodes[id];
    let is_sel = selected.as_ref() == Some(&node.path);
    let mut job = LayoutJob::default();
    let normal = egui::TextFormat { font_id: FontId::proportional(13.5), color: TEXT, ..Default::default() };
    let dim = egui::TextFormat { font_id: FontId::proportional(11.5), color: DIM, ..Default::default() };
    // Mark tag goes first so truncating a long folder name can't hide it.
    if let Some(&is_sfx) = marks.get(&node.path) {
        let tag = egui::TextFormat { font_id: FontId::proportional(11.0), color: ACCENT, ..Default::default() };
        job.append(if is_sfx { "SFX " } else { "Music " }, 0.0, tag);
    }
    job.append(&node.name, 0.0, normal);
    job.append(&node.count.to_string(), 8.0, dim);

    let mut label = |ui: &mut egui::Ui| {
        let resp = ui.add(egui::Button::selectable(is_sel, job.clone()).wrap_mode(TextWrapMode::Truncate));
        if resp.clicked() {
            actions.push(Action::SetFolder(Some(node.path.clone())));
        }
        resp.context_menu(|ui| {
            if ui.button("📂  Show in Explorer").clicked() {
                actions.push(Action::Reveal(node.path.clone()));
            }
            ui.separator();
            ui.label(RichText::new("Everything in this folder:").small().color(DIM));
            mark_menu(ui, marks.get(&node.path).copied(), |m| actions.push(Action::MarkFolder(node.path.clone(), m)));
            if is_root {
                ui.separator();
                if ui.button("Remove from library").clicked() {
                    actions.push(Action::RemoveRoot(node.path.clone()));
                }
            }
        });
    };

    if node.children.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().icon_width + ui.spacing().item_spacing.x);
            label(ui);
        });
    } else {
        let state = CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id(&node.path), is_root);
        state.show_header(ui, |ui| label(ui)).body(|ui| {
            for &c in &node.children {
                tree_ui(ui, nodes, c, selected, marks, actions, false);
            }
        });
    }
}

/// "Mark as SFX / Music / Reset" items; `current` is the existing mark on exactly this path.
fn mark_menu(ui: &mut egui::Ui, current: Option<bool>, mut set: impl FnMut(Option<bool>)) {
    if ui.add(egui::Button::selectable(current == Some(true), "Mark as SFX")).clicked() {
        set(Some(true));
    }
    if ui.add(egui::Button::selectable(current == Some(false), "Mark as Music")).clicked() {
        set(Some(false));
    }
    if current.is_some() && ui.button("Clear mark").clicked() {
        set(None);
    }
}

/// Column rects for a row: Name | Folder | LUFS | Length | Kind | Type | Size.
fn columns(row: Rect) -> [Rect; 7] {
    const PAD: f32 = 10.0;
    let fixed = [56.0, 64.0, 48.0, 52.0, 74.0];
    let x0 = row.left() + 6.0;
    let flex = (row.right() - 6.0 - x0 - fixed.iter().sum::<f32>() - PAD * 6.0).max(120.0);
    let widths = [flex * 0.55, flex * 0.45, fixed[0], fixed[1], fixed[2], fixed[3], fixed[4]];
    let mut x = x0;
    widths.map(|w| {
        let r = Rect::from_min_max(egui::pos2(x, row.top()), egui::pos2(x + w, row.bottom()));
        x += w + PAD;
        r
    })
}

/// Text + color for a LUFS value: hot files (louder than YouTube's target) stand out.
fn loudness_label(lufs: f32) -> (String, Color32) {
    if lufs.is_nan() {
        ("?".into(), DIM)
    } else if !lufs.is_finite() {
        ("silent".into(), DIM)
    } else if lufs > -9.0 {
        (format!("{lufs:.1}"), HOT)
    } else if lufs > YT_TARGET + 0.5 {
        (format!("{lufs:.1}"), WARN)
    } else {
        (format!("{lufs:.1}"), DIM)
    }
}

fn draw_waveform(painter: &egui::Painter, rect: Rect, wave: Option<&[u8]>, frac: f32, hot: bool) {
    painter.rect_filled(rect, CornerRadius::same(4), ROW_ALT);
    let mid = rect.center().y;
    let play_x = rect.left() + rect.width() * frac;
    match wave.filter(|w| !w.is_empty()) {
        Some(wave) => {
            let half = rect.height() / 2.0 - 3.0;
            let step = 3.0;
            let bars = (rect.width() / step).floor().max(1.0) as usize;
            for b in 0..bars {
                let x = rect.left() + b as f32 * step + 1.0;
                let lo = b * wave.len() / bars;
                let hi = ((b + 1) * wave.len() / bars).max(lo + 1).min(wave.len());
                let amp = wave[lo..hi].iter().copied().max().unwrap_or(0) as f32 / 255.0;
                let h = (amp * half).max(0.75);
                let color = if amp >= 0.99 {
                    HOT
                } else if x <= play_x {
                    ACCENT
                } else {
                    Color32::from_rgb(88, 94, 106)
                };
                painter.line_segment([egui::pos2(x, mid - h), egui::pos2(x, mid + h)], Stroke::new(2.0, color));
            }
        }
        None => {
            let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 4.0));
            painter.rect_filled(track, CornerRadius::same(2), HOVER);
            let mut done = track;
            done.set_width(track.width() * frac);
            painter.rect_filled(done, CornerRadius::same(2), ACCENT);
        }
    }
    if frac > 0.0 || hot {
        painter.line_segment(
            [egui::pos2(play_x, rect.top() + 2.0), egui::pos2(play_x, rect.bottom() - 2.0)],
            Stroke::new(1.5, TEXT),
        );
    }
}

fn cell_text(painter: &egui::Painter, cell: Rect, text: &str, color: Color32, right: bool) {
    if text.is_empty() || cell.width() <= 0.0 {
        return;
    }
    let mut job = LayoutJob::simple_singleline(text.to_owned(), FontId::proportional(13.5), color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(cell.width());
    let galley = painter.layout_job(job);
    let y = cell.center().y - galley.size().y / 2.0;
    let x = if right { cell.right() - galley.size().x } else { cell.left() };
    painter.galley(egui::pos2(x, y), galley, color);
}

fn reveal(path: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let arg = if path.is_dir() { format!("\"{}\"", path.display()) } else { format!("/select,\"{}\"", path.display()) };
        let _ = std::process::Command::new("explorer").raw_arg(arg).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

fn setup_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.selection.bg_fill = SELECTED;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|s| {
        s.spacing.button_padding = Vec2::new(8.0, 3.0);
        s.spacing.item_spacing = Vec2::new(6.0, 4.0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, secs: Option<f32>) -> Entry {
        let path = PathBuf::from(path);
        Entry {
            name: String::new(),
            dir: path.parent().unwrap().to_path_buf(),
            path,
            folder: String::new(),
            ext: "wav".into(),
            size: 0,
            mtime: 0,
            duration: secs,
            loudness: None,
            haystack: String::new(),
        }
    }

    #[test]
    fn marks_override_length_rule() {
        let mut marks = HashMap::new();
        let short = entry(r"C:\lib\Songs\Intro\sting.wav", Some(4.0));
        let long = entry(r"C:\lib\Songs\Intro\theme.wav", Some(95.0));
        assert_eq!(classify(&marks, &short), Some((true, false)));
        assert_eq!(classify(&marks, &long), Some((false, false)));
        assert_eq!(classify(&marks, &entry(r"C:\lib\x.wav", None)), None);

        // Folder mark applies to everything below it, even before the length is known.
        marks.insert(PathBuf::from(r"C:\lib\Songs"), false);
        assert_eq!(classify(&marks, &short), Some((false, true)));
        assert_eq!(classify(&marks, &entry(r"C:\lib\Songs\y.wav", None)), Some((false, true)));

        // A file mark beats the folder mark.
        marks.insert(short.path.clone(), true);
        assert_eq!(classify(&marks, &short), Some((true, true)));
        assert_eq!(classify(&marks, &long), Some((false, true)));
    }
}
