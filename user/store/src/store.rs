//
//       user/store/src/store.rs
//       Space 商店: window, navigation, views and transaction overlays
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//
// The store is a pure spk front end: every read goes through `spk ... --json`
// and every change goes through a queued `spk` transaction. Nothing here
// touches /var/lib/spk directly, and nothing inspects which theme skin is active.
//

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use spui_ui::app::{App, UiEvent};
use spui_ui::draw::{Canvas, Rect, Sprite};
use spui_ui::icons::{self, Icon};
use spui_ui::text::{Face, TextRenderer};
use spui_ui::theme::Theme;
use spui_ui::ui;
use spui_ui::x11::Key;

use crate::backend::{BackendEvent, StoreBackend};
use crate::curated::{
    build_hero_slides, build_sections, category_label, present_package, HeroSlide,
    PackagePresentation,
};
use crate::model::{
    format_bytes, format_timestamp, CatalogJson, FullSnapshot, InstalledFilter, LogEntry,
    LogLevel, NavSection, PackageFileEntry, RepoStatusEntry, SortMode, StoreCategory,
    StorePackage, TxJob, TxJobKind, TxJobStatus, TxPlanJson,
};
use crate::ui_cards::{self, FEATURED_CARD_H, SIDEBAR_W, TOPBAR_H};

const HERO_H: i32 = 212;
const QUEUE_H: i32 = 196;
const SCROLLBAR_GUTTER: i32 = 14;
const AUTO_ADVANCE: Duration = Duration::from_secs(7);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartView {
    Discover,
    Browse,
    Updates,
    Installed,
    Maintenance,
}

#[derive(Clone, Debug, Default)]
pub struct StoreOptions {
    pub view: Option<StartView>,
    pub package: Option<String>,
    pub category: Option<String>,
    pub search: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusedInput {
    None,
    Search,
    RepoName,
    RepoUrl,
    TrustPath,
    AuthPassword,
}

#[derive(Clone, Debug)]
enum Action {
    Noop,
    Nav(NavSection),
    Category(StoreCategory),
    Sort(SortMode),
    InstalledFilter(InstalledFilter),
    OpenDetail(String),
    Install(String),
    Update(String),
    UpdateAll,
    RefreshCatalog,
    HeroCta(usize),
    HeroDot(usize),
    HeroPrev,
    HeroNext,
    SelectDetailShot(usize),
    DetailShotPrev,
    DetailShotNext,
    Back,
    SetTab(crate::model::DetailTab),
    Reinstall(String),
    Hold(String),
    Unhold(String),
    Remove(String),
    TogglePkgMenu(String, Rect),
    ClosePkgMenu,
    OpenAppearance,
    ToggleQueue,
    QueueTab(crate::model::QueueTab),
    CancelJob(u64),
    MaintCheck,
    MaintVerify,
    MaintRepair,
    CacheClean,
    RepoRemove(String),
    RepoPriority(String, i32),
    RepoAddSubmit,
    TrustAdd,
    FocusInput(FocusedInput),
    ClearSearch,
    PreviewConfirm,
    PreviewCancel,
    ToggleRecursive,
    TogglePurge,
    ToggleAuthReveal,
    AuthSubmit,
    AuthCancel,
    DismissToast,
}

struct Hit {
    rect: Rect,
    raw_rect: Rect,
    action: Action,
}

struct Hits {
    list: Vec<Hit>,
}

impl Hits {
    fn new() -> Self {
        Self { list: Vec::new() }
    }

    fn push(&mut self, rect: Rect, action: Action) {
        if !rect.is_empty() {
            self.list.push(Hit {
                rect,
                raw_rect: rect,
                action,
            });
        }
    }

    fn push_clipped(&mut self, rect: Rect, clip: Rect, action: Action) {
        let visible = rect.intersect(clip);
        if !visible.is_empty() {
            self.list.push(Hit {
                rect: visible,
                raw_rect: rect,
                action,
            });
        }
    }

    fn at(&self, x: i32, y: i32) -> Option<&Action> {
        self.list
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(x, y))
            .map(|hit| &hit.action)
    }

    /// True when the topmost hit under the pointer was registered for `rect`.
    fn hover_rect(&self, x: i32, y: i32, rect: Rect) -> bool {
        if rect.is_empty() || !rect.contains(x, y) {
            return false;
        }
        self.list
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(x, y))
            .is_some_and(|hit| hit.raw_rect == rect)
    }
}

struct PreviewState {
    job: TxJobKind,
    title: String,
    plan: Result<TxPlanJson, String>,
    recursive: bool,
    purge: bool,
}

struct AuthPrompt {
    job_id: u64,
    title: String,
    command: String,
    error: Option<String>,
    input: String,
    reveal: bool,
}

impl Drop for AuthPrompt {
    fn drop(&mut self) {
        // SAFETY: wipe UTF-8 password buffer before releasing memory.
        unsafe {
            self.input.as_mut_vec().fill(0);
        }
        self.input.clear();
    }
}

#[derive(Clone)]
struct MaintenanceResult {
    title: String,
    ok: bool,
    summary: String,
    details: Vec<String>,
}

struct StoreState {
    catalog: CatalogJson,
    repos: Vec<RepoStatusEntry>,
    packages: Vec<StorePackage>,
    updates: Vec<String>,
    offline: bool,
    loading: bool,
    view: NavSection,
    category: StoreCategory,
    sort: SortMode,
    installed_filter: InstalledFilter,
    search: String,
    preedit: String,
    focused: FocusedInput,
    detail: Option<String>,
    detail_tab: crate::model::DetailTab,
    detail_shot_idx: usize,
    detail_files: Vec<PackageFileEntry>,
    discover_scroll: ui::SmoothScroll,
    detail_scroll: ui::SmoothScroll,
    grid_scroll: ui::SmoothScroll,
    maint_scroll: ui::SmoothScroll,
    queue_scroll: ui::SmoothScroll,
    hero_index: usize,
    hero_hover: bool,
    hero_last: Instant,
    jobs: Vec<TxJob>,
    logs: Vec<LogEntry>,
    queue_open: bool,
    queue_tab: crate::model::QueueTab,
    preview: Option<PreviewState>,
    auth: Option<AuthPrompt>,
    maint: Option<MaintenanceResult>,
    trust_keys: Vec<crate::model::TrustKeyEntry>,
    cache: crate::model::CacheInfoJson,
    repo_name: String,
    repo_url: String,
    trust_path: String,
    toast: Option<(String, Instant)>,
    open_menu: Option<(String, Rect)>,
    last_pointer: (i32, i32),
    banners: HashMap<String, Sprite>,
    banner_requested: HashSet<String>,
    banner_cache_dir: PathBuf,
}

fn default_banner_cache_dir() -> PathBuf {
    if let Some(xdg) = std::env::var("XDG_CACHE_HOME").ok().filter(|s| !s.is_empty()) {
        return PathBuf::from(xdg).join("space-store/previews");
    }
    if let Some(home) = std::env::var("HOME").ok().filter(|s| !s.is_empty()) {
        return PathBuf::from(home).join(".cache/space-store/previews");
    }
    std::env::temp_dir().join("space-store-previews")
}

fn sanitize_cache_name(rel_path: &str) -> String {
    rel_path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl StoreState {
    fn new() -> Self {
        Self {
            catalog: CatalogJson::default(),
            repos: Vec::new(),
            packages: Vec::new(),
            updates: Vec::new(),
            offline: false,
            loading: true,
            view: NavSection::Discover,
            category: StoreCategory::All,
            sort: SortMode::Featured,
            installed_filter: InstalledFilter::All,
            search: String::new(),
            preedit: String::new(),
            focused: FocusedInput::None,
            detail: None,
            detail_tab: crate::model::DetailTab::Overview,
            detail_shot_idx: 0,
            detail_files: Vec::new(),
            discover_scroll: ui::SmoothScroll::new("store_discover_scroll"),
            detail_scroll: ui::SmoothScroll::new("store_detail_scroll"),
            grid_scroll: ui::SmoothScroll::new("store_grid_scroll"),
            maint_scroll: ui::SmoothScroll::new("store_maint_scroll"),
            queue_scroll: ui::SmoothScroll::new("store_queue_scroll"),
            hero_index: 0,
            hero_hover: false,
            hero_last: Instant::now(),
            jobs: Vec::new(),
            logs: Vec::new(),
            queue_open: false,
            queue_tab: crate::model::QueueTab::Jobs,
            preview: None,
            auth: None,
            maint: None,
            trust_keys: Vec::new(),
            cache: crate::model::CacheInfoJson::default(),
            repo_name: String::new(),
            repo_url: String::new(),
            trust_path: "/etc/spk/trusted.d/".into(),
            toast: None,
            open_menu: None,
            last_pointer: (0, 0),
            banners: HashMap::new(),
            banner_requested: HashSet::new(),
            banner_cache_dir: default_banner_cache_dir(),
        }
    }

    fn apply_snapshot(&mut self, snapshot: FullSnapshot, backend: Option<&StoreBackend>) {
        self.catalog = snapshot.catalog;
        self.repos = self.catalog.repositories.clone();
        self.packages = self.catalog.packages.clone();
        self.updates = self.catalog.updates.clone();
        self.trust_keys = snapshot.trust_keys;
        self.cache = snapshot.cache;
        self.offline = snapshot.offline;
        self.loading = false;
        let len = self.hero_slides().len();
        if len > 0 && self.hero_index >= len {
            self.hero_index = 0;
        }
        if let Some(backend) = backend {
            self.request_missing_banners(backend);
            if let Some(detail_pkg) = self.detail.clone() {
                self.request_detail_screenshots(&detail_pkg, backend);
            }
        }
    }

    fn request_detail_screenshots(&mut self, name: &str, backend: &StoreBackend) {
        let mut rels: Vec<String> = Vec::new();
        if let Some(meta) = self.meta(name) {
            if let Some(b) = &meta.banner {
                rels.push(b.clone());
            }
            for s in &meta.screenshots {
                rels.push(s.clone());
            }
        }
        for rel in rels {
            if self.banners.contains_key(&rel) || !self.banner_requested.insert(rel.clone()) {
                continue;
            }
            let dest = self.banner_cache_dir.join(sanitize_cache_name(&rel));
            backend.fetch_banner(rel, dest, self.offline);
        }
    }

    fn request_missing_banners(&mut self, backend: &StoreBackend) {
        let mut rels: Vec<String> = Vec::new();
        for slide in self.hero_slides() {
            if let Some(banner) = slide.banner {
                rels.push(banner);
            }
        }
        for pkg in &self.packages {
            if let Some(meta) = self.meta(&pkg.name) {
                if let Some(banner) = &meta.banner {
                    rels.push(banner.clone());
                }
                for shot in &meta.screenshots {
                    rels.push(shot.clone());
                }
            }
        }
        for rel in rels {
            if self.banners.contains_key(&rel) || !self.banner_requested.insert(rel.clone()) {
                continue;
            }
            let dest = self.banner_cache_dir.join(sanitize_cache_name(&rel));
            backend.fetch_banner(rel, dest, self.offline);
        }
    }

    fn package(&self, name: &str) -> Option<&StorePackage> {
        self.packages.iter().find(|p| p.name == name)
    }

    fn meta(&self, name: &str) -> Option<&crate::model::StoreMeta> {
        self.catalog.meta_for(name)
    }

    fn presentation(&self, pkg: &StorePackage) -> PackagePresentation {
        present_package(pkg, self.meta(&pkg.name))
    }

    fn hero_slides(&self) -> Vec<HeroSlide> {
        build_hero_slides(&self.catalog)
    }

    fn banner_sprite(&self, rel: Option<&str>) -> Option<&Sprite> {
        rel.and_then(|r| self.banners.get(r))
    }

    fn installed_count(&self) -> usize {
        self.packages.iter().filter(|p| p.installed).count()
    }

    fn explicit_count(&self) -> usize {
        self.packages
            .iter()
            .filter(|p| p.installed && p.install_reason.as_deref() != Some("dependency"))
            .count()
    }

    fn dependency_count(&self) -> usize {
        self.packages
            .iter()
            .filter(|p| p.installed && p.install_reason.as_deref() == Some("dependency"))
            .count()
    }

    fn held_count(&self) -> usize {
        self.packages.iter().filter(|p| p.held).count()
    }

    fn job_for(&self, package: &str) -> Option<&TxJob> {
        self.jobs.iter().rev().find(|job| {
            job.status.is_active() && job.kind.primary_package() == Some(package)
        })
    }

    fn filtered_packages(&self) -> Vec<&StorePackage> {
        let lowered = self.search.trim().to_ascii_lowercase();
        let mut list: Vec<&StorePackage> = self
            .packages
            .iter()
            .filter(|pkg| {
                if self.view == NavSection::Installed && !pkg.installed {
                    return false;
                }
                if self.view == NavSection::Updates && !pkg.upgrade_available {
                    return false;
                }
                if self.view == NavSection::Browse && self.category != StoreCategory::All {
                    let pres = self.presentation(pkg);
                    if pres.category != self.category {
                        return false;
                    }
                }
                if self.view == NavSection::Installed {
                    match self.installed_filter {
                        InstalledFilter::All => {}
                        InstalledFilter::Explicit => {
                            if pkg.install_reason.as_deref() == Some("dependency") {
                                return false;
                            }
                        }
                        InstalledFilter::Dependency => {
                            if pkg.install_reason.as_deref() != Some("dependency") {
                                return false;
                            }
                        }
                        InstalledFilter::Held => {
                            if !pkg.held {
                                return false;
                            }
                        }
                    }
                }
                if lowered.is_empty() {
                    return true;
                }
                pkg.name.to_ascii_lowercase().contains(&lowered)
                    || pkg.summary.to_ascii_lowercase().contains(&lowered)
                    || self
                        .meta(&pkg.name)
                        .and_then(|m| m.display_name.clone())
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .contains(&lowered)
            })
            .collect();
        match self.sort {
            SortMode::Featured => list.sort_by_key(|pkg| {
                std::cmp::Reverse(self.presentation(pkg).featured_rank)
            }),
            SortMode::Name => list.sort_by(|a, b| {
                self.presentation(a)
                    .display_name
                    .cmp(&self.presentation(b).display_name)
            }),
            SortMode::SizeDesc => list.sort_by_key(|pkg| std::cmp::Reverse(pkg.size)),
            SortMode::InstalledFirst => list.sort_by_key(|pkg| std::cmp::Reverse(pkg.installed)),
        }
        list
    }

    fn active_focus(&self) -> FocusedInput {
        if self.auth.is_some() {
            FocusedInput::AuthPassword
        } else if self.preview.is_some() {
            FocusedInput::None
        } else {
            self.focused
        }
    }

    fn active_input_mut(&mut self) -> Option<&mut String> {
        match self.active_focus() {
            FocusedInput::None => None,
            FocusedInput::Search => Some(&mut self.search),
            FocusedInput::RepoName => Some(&mut self.repo_name),
            FocusedInput::RepoUrl => Some(&mut self.repo_url),
            FocusedInput::TrustPath => Some(&mut self.trust_path),
            FocusedInput::AuthPassword => self.auth.as_mut().map(|a| &mut a.input),
        }
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }
}

pub fn run_store(options: StoreOptions) -> Result<(), String> {
    let mut app = App::new("Space 商店", Icon::Store, 1060, 648).map_err(|e| e.to_string())?;
    app.min_width = 880;
    app.min_height = 560;

    let backend = StoreBackend::spawn();
    let mut state = StoreState::new();
    if let Some(view) = options.view {
        state.view = match view {
            StartView::Discover => NavSection::Discover,
            StartView::Browse => NavSection::Browse,
            StartView::Updates => NavSection::Updates,
            StartView::Installed => NavSection::Installed,
            StartView::Maintenance => NavSection::Maintenance,
        };
    }
    if let Some(category) = options.category.as_deref() {
        state.category = StoreCategory::from_slug(category);
        if state.view == NavSection::Discover {
            state.view = NavSection::Browse;
        }
    }
    if let Some(search) = options.search {
        state.search = search;
        state.focused = FocusedInput::Search;
        state.view = NavSection::Browse;
    }
    if let Some(package) = options.package.clone() {
        state.detail = Some(package);
    }

    backend.refresh_catalog(false);
    let mut prev_hits = Hits::new();

    loop {
        if app.close {
            break;
        }

        let content = app.content();
        if state
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(4))
        {
            state.toast = None;
            app.needs_redraw = true;
        }

        let ime_active = !matches!(
            state.active_focus(),
            FocusedInput::None | FocusedInput::AuthPassword
        );
        app.set_ime_focus(ime_active);

        if app.needs_redraw {
            let mut hits = Hits::new();
            let ime = {
                let App {
                    canvas, text, theme, ..
                } = &mut app;
                draw_store(canvas, text, theme, &mut state, content, &prev_hits, &mut hits)
            };
            if let Some(spot) = ime {
                app.set_ime_spot(spot);
            }
            app.draw_chrome();
            app.present().map_err(|e| e.to_string())?;
            state.hero_hover = hero_rect(content, state.queue_open)
                .contains(state.last_pointer.0, state.last_pointer.1);
            prev_hits = hits;
        }

        let timeout = if ui::has_active_animations() { 16 } else { 120 };
        let (events, _changed) = app
            .pump_with_fd(Some(backend.wakeup_fd()), timeout)
            .map_err(|e| e.to_string())?;

        for event in events {
            handle_event(&mut app, &mut state, &backend, &prev_hits, event);
        }
        let backend_events = backend.poll_events();
        if !backend_events.is_empty() {
            app.needs_redraw = true;
        }
        for event in backend_events {
            apply_backend_event(&mut state, &backend, event);
        }

        if state.hero_slides().len() > 1
            && !state.hero_hover
            && state.view == NavSection::Discover
            && state.detail.is_none()
            && state.preview.is_none()
            && state.auth.is_none()
            && state.hero_last.elapsed() >= AUTO_ADVANCE
        {
            state.hero_index = (state.hero_index + 1) % state.hero_slides().len();
            state.hero_last = Instant::now();
            app.needs_redraw = true;
        }
    }
    Ok(())
}

fn hero_rect(content: Rect, queue_open: bool) -> Rect {
    let main_h = content.h - TOPBAR_H - if queue_open { QUEUE_H } else { 0 };
    let main = Rect::new(
        content.x + SIDEBAR_W,
        content.y + TOPBAR_H,
        content.w - SIDEBAR_W,
        main_h.max(1),
    );
    let area = Rect::new(main.x + 22, main.y + 14, main.w - 44, main.h - 28);
    Rect::new(area.x, area.y, area.w - SCROLLBAR_GUTTER, HERO_H)
}

fn handle_event(
    app: &mut App,
    state: &mut StoreState,
    backend: &StoreBackend,
    hits: &Hits,
    event: UiEvent,
) {
    match event {
        UiEvent::Close => app.request_close(),
        UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
            app.needs_redraw = true;
        }
        UiEvent::Motion { x, y, state: mask } => {
            state.last_pointer = (x, y);
            ui::notify_pointer_motion(x, y, mask & 0x100 != 0);
            app.needs_redraw = true;
        }
        UiEvent::Wheel { x, y, delta } => {
            if handle_wheel(state, x, y, delta, app.content()) {
                app.needs_redraw = true;
            }
        }
        UiEvent::Button {
            x,
            y,
            button,
            down,
            ..
        } => {
            state.last_pointer = (x, y);
            ui::notify_pointer_button(x, y, button, down);
            if button == 1 && !down {
                if let Some(action) = hits.at(x, y).cloned() {
                    perform_action(app, state, backend, action);
                } else {
                    state.focused = FocusedInput::None;
                }
            }
            app.needs_redraw = true;
        }
        UiEvent::CommitText(text) => {
            state.preedit.clear();
            if let Some(target) = state.active_input_mut() {
                target.push_str(&text);
                app.needs_redraw = true;
            }
        }
        UiEvent::Preedit(text) => {
            state.preedit = text;
            app.needs_redraw = true;
        }
        UiEvent::Key(key) => {
            if let Some(action) = handle_key_state(state, key) {
                perform_action(app, state, backend, action);
            }
            app.needs_redraw = true;
        }
    }
}

fn handle_key_state(state: &mut StoreState, key: Key) -> Option<Action> {
    if key.is_escape() {
        if state.auth.is_some() {
            return Some(Action::AuthCancel);
        } else if state.preview.is_some() {
            return Some(Action::PreviewCancel);
        } else if state.open_menu.is_some() {
            state.open_menu = None;
        } else if state.focused != FocusedInput::None {
            state.focused = FocusedInput::None;
            state.preedit.clear();
        } else if state.detail.is_some() {
            state.detail = None;
        } else if state.queue_open {
            state.queue_open = false;
        }
        return None;
    }
    if key.is_enter() {
        if state.auth.is_some() {
            return Some(Action::AuthSubmit);
        }
        if state.preview.is_some() {
            return Some(Action::PreviewConfirm);
        }
        match state.active_focus() {
            FocusedInput::RepoName | FocusedInput::RepoUrl => return Some(Action::RepoAddSubmit),
            FocusedInput::TrustPath => return Some(Action::TrustAdd),
            FocusedInput::Search => {
                if state.view == NavSection::Discover || state.detail.is_some() {
                    state.detail = None;
                    state.view = NavSection::Browse;
                    state.grid_scroll.snap_to(0.0);
                }
            }
            FocusedInput::None | FocusedInput::AuthPassword => {}
        }
        return None;
    }
    if key.control() && key.text() == Some('f') {
        state.detail = None;
        state.view = NavSection::Browse;
        state.focused = FocusedInput::Search;
        state.grid_scroll.snap_to(0.0);
        return None;
    }
    if key.control() && key.text() == Some('u') {
        if let Some(buf) = state.active_input_mut() {
            buf.clear();
        }
        return None;
    }
    if key.sym == 0xff09 && state.view == NavSection::Maintenance && state.preview.is_none() && state.auth.is_none() {
        state.focused = match state.focused {
            FocusedInput::RepoName => FocusedInput::RepoUrl,
            FocusedInput::RepoUrl => FocusedInput::TrustPath,
            _ => FocusedInput::RepoName,
        };
        return None;
    }
    if key.is_backspace() || key.sym == 0xff08 {
        if let Some(buf) = state.active_input_mut() {
            buf.pop();
        }
        return None;
    }
    if !key.control() && !key.alt() {
        if let Some(ch) = key.text() {
            if !ch.is_control() {
                if let Some(buf) = state.active_input_mut() {
                    buf.push_str(&ch.to_string());
                }
            }
        }
    }
    None
}

fn handle_wheel(state: &mut StoreState, _x: i32, y: i32, delta: i32, content: Rect) -> bool {
    if state.auth.is_some() || state.preview.is_some() {
        return false;
    }
    state.open_menu = None;
    let step = (delta as f32) * 36.0;
    if state.queue_open && y > content.y + content.h - QUEUE_H {
        state.queue_scroll.wheel(step)
    } else if state.detail.is_some() {
        state.detail_scroll.wheel(step)
    } else {
        match state.view {
            NavSection::Discover => state.discover_scroll.wheel(step),
            NavSection::Maintenance => state.maint_scroll.wheel(step),
            NavSection::Browse | NavSection::Updates | NavSection::Installed => {
                state.grid_scroll.wheel(step)
            }
        }
    }
}

fn perform_action(app: &mut App, state: &mut StoreState, backend: &StoreBackend, action: Action) {
    if !matches!(action, Action::TogglePkgMenu(_, _) | Action::Noop) {
        state.open_menu = None;
    }
    match action {
        Action::Noop => {}
        Action::TogglePkgMenu(name, anchor) => {
            if state
                .open_menu
                .as_ref()
                .is_some_and(|(cur, _)| cur == &name)
            {
                state.open_menu = None;
            } else {
                state.open_menu = Some((name, anchor));
            }
            app.needs_redraw = true;
        }
        Action::ClosePkgMenu => {
            state.open_menu = None;
            app.needs_redraw = true;
        }
        Action::Nav(view) => {
            state.view = view;
            state.detail = None;
            if state.focused != FocusedInput::Search {
                state.focused = FocusedInput::None;
            }
            state.grid_scroll.snap_to(0.0);
            state.discover_scroll.snap_to(0.0);
            state.maint_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::Category(category) => {
            state.category = category;
            state.grid_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::Sort(sort) => {
            state.sort = sort;
            state.grid_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::InstalledFilter(filter) => {
            state.installed_filter = filter;
            state.grid_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::OpenDetail(name) => {
            state.detail = Some(name.clone());
            state.detail_tab = crate::model::DetailTab::Overview;
            state.detail_shot_idx = 0;
            state.detail_files.clear();
            state.detail_scroll.snap_to(0.0);
            state.request_detail_screenshots(&name, backend);
            backend.load_package_details(name);
            app.needs_redraw = true;
        }
        Action::Back => {
            state.detail = None;
            app.needs_redraw = true;
        }
        Action::SetTab(tab) => {
            state.detail_tab = tab;
            state.detail_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::Install(name) => {
            enqueue_or_preview(app, state, backend, TxJobKind::Install(vec![name]));
        }
        Action::Update(name) => {
            enqueue_or_preview(app, state, backend, TxJobKind::UpgradeOne(name));
        }
        Action::UpdateAll => {
            enqueue_or_preview(app, state, backend, TxJobKind::UpgradeAll);
        }
        Action::RefreshCatalog => {
            state.loading = true;
            backend.refresh_catalog(state.offline);
            app.needs_redraw = true;
        }
        Action::HeroCta(index) => {
            if let Some(slide) = state.hero_slides().get(index).cloned() {
                if slide.go_updates {
                    state.view = NavSection::Updates;
                    state.detail = None;
                } else if let Some(package) = slide.target_package {
                    perform_action(app, state, backend, Action::OpenDetail(package));
                } else {
                    perform_action(app, state, backend, Action::RefreshCatalog);
                }
                app.needs_redraw = true;
            }
        }
        Action::HeroDot(index) => {
            state.hero_index = index;
            state.hero_last = Instant::now();
            app.needs_redraw = true;
        }
        Action::HeroPrev => {
            let len = state.hero_slides().len().max(1);
            state.hero_index = (state.hero_index + len - 1) % len;
            state.hero_last = Instant::now();
            app.needs_redraw = true;
        }
        Action::HeroNext => {
            let len = state.hero_slides().len().max(1);
            state.hero_index = (state.hero_index + 1) % len;
            state.hero_last = Instant::now();
            app.needs_redraw = true;
        }
        Action::SelectDetailShot(index) => {
            state.detail_shot_idx = index;
            app.needs_redraw = true;
        }
        Action::DetailShotPrev => {
            if let Some(pkg) = state.detail.clone().and_then(|n| state.package(&n).cloned()) {
                let len = state.presentation(&pkg).screenshots.len().max(1);
                state.detail_shot_idx = (state.detail_shot_idx + len - 1) % len;
                app.needs_redraw = true;
            }
        }
        Action::DetailShotNext => {
            if let Some(pkg) = state.detail.clone().and_then(|n| state.package(&n).cloned()) {
                let len = state.presentation(&pkg).screenshots.len().max(1);
                state.detail_shot_idx = (state.detail_shot_idx + 1) % len;
                app.needs_redraw = true;
            }
        }
        Action::Reinstall(name) => {
            enqueue_or_preview(app, state, backend, TxJobKind::Reinstall(name));
        }
        Action::Hold(name) => {
            backend.enqueue_job(TxJobKind::Hold(name), "锁定软件包");
        }
        Action::Unhold(name) => {
            backend.enqueue_job(TxJobKind::Unhold(name), "解除锁定");
        }
        Action::Remove(name) => {
            enqueue_or_preview(
                app,
                state,
                backend,
                TxJobKind::Remove {
                    name,
                    recursive: false,
                    purge: false,
                },
            );
        }
        Action::OpenAppearance => {
            spawn_sibling("spui", &["settings", "appearance"]);
            state.toast("正在打开系统设置的外观页…");
            app.needs_redraw = true;
        }
        Action::ToggleQueue => {
            state.queue_open = !state.queue_open;
            app.needs_redraw = true;
        }
        Action::QueueTab(tab) => {
            state.queue_tab = tab;
            state.queue_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::CancelJob(id) => {
            backend.cancel_queued_job(id);
            if let Some(job) = state.jobs.iter_mut().find(|j| j.id == id) {
                if job.status == TxJobStatus::Queued || job.status == TxJobStatus::WaitingAuth {
                    job.status = TxJobStatus::Cancelled;
                }
            }
            app.needs_redraw = true;
        }
        Action::MaintCheck => backend.run_check(),
        Action::MaintVerify => backend.run_verify(),
        Action::MaintRepair => {
            backend.enqueue_job(TxJobKind::Repair, "恢复未完成事务");
        }
        Action::CacheClean => {
            backend.enqueue_job(TxJobKind::CacheClean, "清理软件包缓存");
        }
        Action::RepoRemove(name) => {
            enqueue_or_preview(app, state, backend, TxJobKind::RepoRemove(name));
        }
        Action::RepoPriority(name, delta) => {
            let current = state
                .repos
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.priority)
                .unwrap_or(0);
            backend.enqueue_job(
                TxJobKind::RepoPriority {
                    name,
                    priority: current + delta,
                },
                "调整软件源优先级",
            );
        }
        Action::RepoAddSubmit => {
            let name = state.repo_name.trim().to_string();
            let url = state.repo_url.trim().to_string();
            if name.is_empty() || url.is_empty() {
                state.toast("请先点击输入框填写软件源名称与地址");
            } else {
                backend.enqueue_job(
                    TxJobKind::RepoAdd {
                        name,
                        url,
                        priority: None,
                    },
                    "添加软件源",
                );
                state.repo_name.clear();
                state.repo_url.clear();
            }
            app.needs_redraw = true;
        }
        Action::TrustAdd => {
            let path = state.trust_path.trim().to_string();
            if path.is_empty() {
                state.toast("请输入要导入的公钥文件路径");
            } else {
                backend.enqueue_job(TxJobKind::TrustAdd(path), "导入信任密钥");
            }
            app.needs_redraw = true;
        }
        Action::FocusInput(target) => {
            state.focused = target;
            state.preedit.clear();
            if target == FocusedInput::Search && state.view == NavSection::Discover && state.detail.is_none() {
                state.view = NavSection::Browse;
            }
            app.needs_redraw = true;
        }
        Action::ClearSearch => {
            state.search.clear();
            state.preedit.clear();
            state.grid_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::PreviewConfirm => {
            if let Some(preview) = state.preview.take() {
                let mut kind = preview.job.clone();
                if let TxJobKind::Remove { name, .. } = &kind {
                    kind = TxJobKind::Remove {
                        name: name.clone(),
                        recursive: preview.recursive,
                        purge: preview.purge,
                    };
                }
                let title = preview.title.clone();
                backend.enqueue_job(kind, title);
            }
            app.needs_redraw = true;
        }
        Action::PreviewCancel => {
            state.preview = None;
            app.needs_redraw = true;
        }
        Action::ToggleRecursive => {
            if let Some(preview) = state.preview.as_mut() {
                preview.recursive = !preview.recursive;
            }
            app.needs_redraw = true;
        }
        Action::TogglePurge => {
            if let Some(preview) = state.preview.as_mut() {
                preview.purge = !preview.purge;
            }
            app.needs_redraw = true;
        }
        Action::ToggleAuthReveal => {
            if let Some(auth) = state.auth.as_mut() {
                auth.reveal = !auth.reveal;
            }
            app.needs_redraw = true;
        }
        Action::AuthSubmit => {
            let secret = state.auth.as_mut().map(|auth| {
                let s = auth.input.clone();
                // SAFETY: overwrite input bytes immediately after handing secret to backend.
                unsafe {
                    auth.input.as_mut_vec().fill(0);
                }
                auth.input.clear();
                s
            });
            if let Some(s) = secret {
                backend.submit_auth(s);
            }
            app.needs_redraw = true;
        }
        Action::AuthCancel => {
            backend.cancel_auth();
            state.auth = None;
            app.needs_redraw = true;
        }
        Action::DismissToast => {
            state.toast = None;
            app.needs_redraw = true;
        }
    }
}

fn enqueue_or_preview(
    app: &mut App,
    state: &mut StoreState,
    backend: &StoreBackend,
    kind: TxJobKind,
) {
    if kind.needs_preview() && !state.offline {
        let title = kind.command_summary();
        backend.preview_tx(kind, title);
    } else {
        let title = kind.command_summary();
        backend.enqueue_job(kind, title);
    }
    app.needs_redraw = true;
}

fn apply_backend_event(state: &mut StoreState, backend: &StoreBackend, event: BackendEvent) {
    match event {
        BackendEvent::CatalogLoaded(Ok(snapshot)) => {
            state.apply_snapshot(snapshot, Some(backend));
        }
        BackendEvent::CatalogLoaded(Err(error)) => {
            state.loading = false;
            state.toast(error);
        }
        BackendEvent::PackageDetailsLoaded { name, files, .. } => {
            if state.detail.as_deref() == Some(name.as_str()) {
                state.detail_files = files;
            }
        }
        BackendEvent::PreviewReady { kind, title, plan } => {
            state.preview = Some(PreviewState {
                job: kind,
                title,
                plan,
                recursive: false,
                purge: false,
            });
        }
        BackendEvent::BannerReady { rel_path, path } => {
            if let Some(sprite) = icons::decode_theme_asset(&path, None) {
                state.banners.insert(rel_path, sprite);
            }
        }
        BackendEvent::MaintenanceCheckResult {
            title,
            result,
            summary,
            details,
        } => {
            state.maint = Some(MaintenanceResult {
                title,
                ok: result.ok,
                summary,
                details,
            });
        }
        BackendEvent::JobAdded(job) => {
            state.jobs.push(job);
        }
        BackendEvent::JobUpdated(job) => {
            if !matches!(job.status, TxJobStatus::WaitingAuth)
                && state.auth.as_ref().is_some_and(|a| a.job_id == job.id)
            {
                state.auth = None;
            }
            if let TxJobStatus::Failed(ref err) = job.status {
                state.toast(format!("任务 #{} 失败：{err}", job.id));
            } else if matches!(job.status, TxJobStatus::Succeeded) {
                state.toast(format!("{} 已完成", job.title));
            }
            match state.jobs.iter_mut().find(|j| j.id == job.id) {
                Some(existing) => *existing = job,
                None => state.jobs.push(job),
            }
        }
        BackendEvent::JobLog { entry, .. } => {
            state.logs.push(entry);
            if state.logs.len() > 600 {
                state.logs.drain(..200);
            }
        }
        BackendEvent::AuthRequired {
            job_id,
            title,
            command_preview,
            error_msg,
        } => {
            state.auth = Some(AuthPrompt {
                job_id,
                title,
                command: command_preview,
                error: error_msg,
                input: String::new(),
                reveal: false,
            });
        }
    }
}

fn spawn_sibling(binary: &str, args: &[&str]) {
    for dir in ["/usr/bin", "/bin", "/usr/local/bin"] {
        let path = format!("{dir}/{binary}");
        if std::path::Path::new(&path).is_file() {
            let _ = std::process::Command::new(path).args(args).spawn();
            return;
        }
    }
}

include!("store_draw.rs");

/// Parse the store's command line into launch options. Kept in the library so
/// it can be unit tested without an X server.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<StoreOptions, String> {
    let mut options = StoreOptions::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--updates" => options.view = Some(StartView::Updates),
            "--maintenance" => options.view = Some(StartView::Maintenance),
            "--installed" => options.view = Some(StartView::Installed),
            "--browse" => options.view = Some(StartView::Browse),
            "--package" => {
                options.package = Some(next_value(&mut iter, "--package")?);
                options.view.get_or_insert(StartView::Discover);
            }
            "--category" => {
                options.category = Some(next_value(&mut iter, "--category")?);
                options.view.get_or_insert(StartView::Browse);
            }
            "--search" => {
                options.search = Some(next_value(&mut iter, "--search")?);
                options.view.get_or_insert(StartView::Browse);
            }
            "-h" | "--help" => return Err("help".into()),
            "-V" | "--version" => return Err("version".into()),
            other => return Err(format!("未知参数：{other}")),
        }
    }
    Ok(options)
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{flag} 需要一个参数"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<StoreOptions, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_launch_flags() {
        assert_eq!(parse(&["--updates"]).unwrap().view, Some(StartView::Updates));
        assert_eq!(
            parse(&["--category", "themes"]).unwrap().category.as_deref(),
            Some("themes")
        );
        assert_eq!(
            parse(&["--category", "themes"]).unwrap().view,
            Some(StartView::Browse)
        );
        let search = parse(&["--search", "主题"]).unwrap();
        assert_eq!(search.search.as_deref(), Some("主题"));
        let package = parse(&["--package", "theme-aqua"]).unwrap();
        assert_eq!(package.package.as_deref(), Some("theme-aqua"));
    }

    #[test]
    fn rejects_unknown_and_incomplete_flags() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--package"]).is_err());
        assert_eq!(parse(&["--help"]).unwrap_err(), "help");
    }

    #[test]
    fn hit_testing_prefers_the_topmost_region() {
        let mut hits = Hits::new();
        let outer = Rect::new(0, 0, 100, 100);
        let inner = Rect::new(20, 20, 30, 30);
        hits.push(outer, Action::Back);
        hits.push(inner, Action::ToggleQueue);
        assert!(matches!(hits.at(25, 25), Some(Action::ToggleQueue)));
        assert!(matches!(hits.at(5, 5), Some(Action::Back)));
        assert!(hits.at(200, 200).is_none());
        assert!(hits.hover_rect(25, 25, inner));
        assert!(!hits.hover_rect(25, 25, outer));
    }

    #[test]
    fn wheel_scroll_down_increases_offset() {
        let mut state = StoreState::new();
        state
            .discover_scroll
            .set_bounds(400.0, 1200.0, Rect::new(0, 0, 8, 400));
        // At offset 0, wheel UP (delta < 0) cannot scroll further up; wheel DOWN (delta > 0) scrolls down.
        assert!(!handle_wheel(&mut state, 400, 300, -3, Rect::new(0, 0, 1000, 700)));
        assert!(handle_wheel(&mut state, 400, 300, 3, Rect::new(0, 0, 1000, 700)));

        state.view = NavSection::Browse;
        state
            .grid_scroll
            .set_bounds(400.0, 1200.0, Rect::new(0, 0, 8, 400));
        state.grid_scroll.snap_to(200.0);
        assert!(handle_wheel(&mut state, 400, 300, -3, Rect::new(0, 0, 1000, 700)));
    }

    #[test]
    fn handle_key_updates_focused_inputs() {
        let mut state = StoreState::new();
        state.focused = FocusedInput::RepoName;
        for ch in ['m', 'y', '-', 'r'] {
            handle_key_state(
                &mut state,
                Key {
                    code: 0,
                    state: 0,
                    sym: ch as u32,
                },
            );
        }
        assert_eq!(state.repo_name, "my-r");
        handle_key_state(
            &mut state,
            Key {
                code: 0,
                state: 0,
                sym: 0xff08,
            },
        );
        assert_eq!(state.repo_name, "my-");
        state.view = NavSection::Maintenance;
        handle_key_state(
            &mut state,
            Key {
                code: 0,
                state: 0,
                sym: 0xff09,
            },
        );
        assert_eq!(state.focused, FocusedInput::RepoUrl);

        state.auth = Some(AuthPrompt {
            job_id: 1,
            title: "t".into(),
            command: "c".into(),
            error: None,
            input: String::new(),
            reveal: false,
        });
        handle_key_state(
            &mut state,
            Key {
                code: 0,
                state: 0,
                sym: 'p' as u32,
            },
        );
        assert_eq!(state.auth.as_ref().unwrap().input, "p");
    }

    #[test]
    fn modal_backdrop_blocks_underlying_hits() {
        let mut hits = Hits::new();
        let screen = Rect::new(0, 0, 1060, 700);
        let underlying_btn = Rect::new(40, 40, 120, 30);
        let cancel_btn = Rect::new(500, 420, 92, 30);
        hits.push(underlying_btn, Action::RefreshCatalog);
        hits.push(screen, Action::Noop);
        hits.push(cancel_btn, Action::PreviewCancel);

        assert!(matches!(hits.at(50, 50), Some(Action::Noop)));
        assert!(matches!(hits.at(520, 430), Some(Action::PreviewCancel)));
    }

    #[test]
    fn overflow_menu_closes_on_escape_and_wheel() {
        let mut state = StoreState::new();
        state.open_menu = Some(("space-editor".into(), Rect::new(800, 120, 34, 28)));
        handle_key_state(
            &mut state,
            Key {
                code: 0,
                state: 0,
                sym: 0xff1b,
            },
        );
        assert!(state.open_menu.is_none());

        state.open_menu = Some(("space-store".into(), Rect::new(800, 180, 34, 28)));
        state
            .discover_scroll
            .set_bounds(400.0, 1200.0, Rect::new(0, 0, 8, 400));
        let _ = handle_wheel(&mut state, 400, 300, 3, Rect::new(0, 0, 1000, 700));
        assert!(state.open_menu.is_none());
    }
}
