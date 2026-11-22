//
//       user/store/src/store.rs
//       Space 商店: window, navigation, views and transaction overlays
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//
// The store is a pure spk front end: every read goes through `spk ... --json`
// and every change goes through a queued `spk` transaction, authorised with an
// in-app sudo prompt. Nothing here touches /var/lib/spk directly, and nothing
// inspects which theme skin is active.
//

use std::time::{Duration, Instant};

use spui_ui::app::{App, UiEvent};
use spui_ui::icons;
use spui_ui::x11::Key;
use spui_ui::draw::{Canvas, Rect};
use spui_ui::icons::Icon;
use spui_ui::text::{Face, TextRenderer};
use spui_ui::theme::Theme;
use spui_ui::ui;

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
use crate::ui_cards::{self, SIDEBAR_W, TOPBAR_H};

const HERO_H: i32 = 172;
const QUEUE_H: i32 = 190;
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

#[derive(Clone, Debug)]
enum Action {
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
    Back,
    SetTab(crate::model::DetailTab),
    Reinstall(String),
    Hold(String),
    Unhold(String),
    Remove(String),
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
    FocusSearch,
    PreviewConfirm,
    PreviewCancel,
    ToggleRecursive,
    TogglePurge,
    AuthSubmit,
    AuthCancel,
    DismissToast,
}

struct Hit {
    rect: Rect,
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
            self.list.push(Hit { rect, action });
        }
    }

    fn at(&self, x: i32, y: i32) -> Option<&Action> {
        self.list
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(x, y))
            .map(|hit| &hit.action)
    }

    /// True when the topmost hit under the pointer is exactly `rect`.
    fn hover_rect(&self, x: i32, y: i32, rect: Rect) -> bool {
        self.list
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(x, y))
            .is_some_and(|hit| hit.rect == rect)
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
    search_focus: bool,
    detail: Option<String>,
    detail_tab: crate::model::DetailTab,
    detail_files: Vec<PackageFileEntry>,
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
    toast: Option<(String, Instant)>,
    last_pointer: (i32, i32),
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
            search_focus: false,
            detail: None,
            detail_tab: crate::model::DetailTab::Overview,
            detail_files: Vec::new(),
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
            toast: None,
            last_pointer: (0, 0),
        }
    }

    fn apply_snapshot(&mut self, snapshot: FullSnapshot) {
        self.catalog = snapshot.catalog;
        self.repos = self.catalog.repositories.clone();
        self.packages = self.catalog.packages.clone();
        self.updates = self.catalog.updates.clone();
        self.trust_keys = snapshot.trust_keys;
        self.cache = snapshot.cache;
        self.offline = snapshot.offline;
        self.loading = false;
        if self.hero_index >= self.hero_slides().len() {
            self.hero_index = 0;
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

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }
}

pub fn run_store(options: StoreOptions) -> Result<(), String> {
    let mut app = App::new("Space 商店", Icon::Store, 1060, 700).map_err(|e| e.to_string())?;
    app.min_width = 880;
    app.min_height = 600;

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
        state.search_focus = true;
        state.view = NavSection::Browse;
    }
    if let Some(package) = options.package.clone() {
        state.detail = Some(package);
    }

    backend.refresh_catalog(false);
    let mut hits = Hits::new();

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

        if app.needs_redraw {
            hits = Hits::new();
            let ime = {
                let App {
                    canvas, text, theme, ..
                } = &mut app;
                draw_store(canvas, text, theme, &mut state, content, &mut hits)
            };
            if let Some(spot) = ime {
                app.set_ime_spot(spot);
            }
            app.draw_chrome();
            app.present().map_err(|e| e.to_string())?;
            state.hero_hover = hero_rect(content, state.queue_open)
                .contains(state.last_pointer.0, state.last_pointer.1);
        }

        let timeout = if ui::has_active_animations() { 16 } else { 120 };
        let (events, _changed) = app
            .pump_with_fd(Some(backend.wakeup_fd()), timeout)
            .map_err(|e| e.to_string())?;

        for event in events {
            handle_event(&mut app, &mut state, &backend, &hits, event);
        }
        let backend_events = backend.poll_events();
        if !backend_events.is_empty() {
            app.needs_redraw = true;
        }
        for event in backend_events {
            apply_backend_event(&mut state, event);
        }

        if state.hero_slides().len() > 1
            && !state.hero_hover
            && state.view == NavSection::Discover
            && state.detail.is_none()
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
    let area = Rect::new(main.x + 20, main.y + 12, main.w - 40, main.h - 24);
    Rect::new(area.x, area.y, area.w, HERO_H)
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
            handle_wheel(state, x, y, delta, app.content());
            app.needs_redraw = true;
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
                }
            }
            app.needs_redraw = true;
        }
        UiEvent::CommitText(text) => {
            if state.search_focus {
                state.search.push_str(&text);
                app.needs_redraw = true;
            } else if let Some(auth) = state.auth.as_mut() {
                auth.input.push_str(&text);
                app.needs_redraw = true;
            }
        }
        UiEvent::Preedit(text) => {
            state.preedit = text;
            app.needs_redraw = true;
        }
        UiEvent::Key(key) => handle_key(app, state, backend, key),
    }
}

fn handle_key(app: &mut App, state: &mut StoreState, backend: &StoreBackend, key: Key) {
    if key.is_escape() {
        if state.auth.is_some() {
            perform_action(app, state, backend, Action::AuthCancel);
        } else if state.preview.is_some() {
            perform_action(app, state, backend, Action::PreviewCancel);
        } else if state.search_focus {
            state.search_focus = false;
        } else if state.detail.is_some() {
            state.detail = None;
        } else if state.queue_open {
            state.queue_open = false;
        }
        app.needs_redraw = true;
        return;
    }
    if key.is_enter() {
        if state.auth.is_some() {
            perform_action(app, state, backend, Action::AuthSubmit);
        } else if state.preview.is_some() {
            perform_action(app, state, backend, Action::PreviewConfirm);
        }
        app.needs_redraw = true;
        return;
    }
    if key.control() && key.text() == Some('f') {
        state.search_focus = true;
        state.view = NavSection::Browse;
        app.needs_redraw = true;
        return;
    }
    if key.sym == 0xff08 {
        if let Some(auth) = state.auth.as_mut() {
            auth.input.pop();
        } else if state.search_focus {
            state.search.pop();
        }
        app.needs_redraw = true;
    }
}

fn handle_wheel(state: &mut StoreState, x: i32, y: i32, delta: i32, content: Rect) {
    let _ = content;
    let step = -(delta as f32) * 48.0;
    if state.queue_open && y > content.y + content.h - QUEUE_H {
        state.queue_scroll.wheel(step);
    } else if state.view == NavSection::Maintenance {
        state.maint_scroll.wheel(step);
    } else if state.detail.is_some() {
        state.detail_scroll.wheel(step);
    } else {
        state.grid_scroll.wheel(step);
    }
    let _ = x;
}

fn perform_action(app: &mut App, state: &mut StoreState, backend: &StoreBackend, action: Action) {
    match action {
        Action::Nav(view) => {
            state.view = view;
            state.detail = None;
            state.grid_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::Category(category) => {
            state.category = category;
            state.grid_scroll.snap_to(0.0);
            app.needs_redraw = true;
        }
        Action::Sort(sort) => {
            state.sort = sort;
            app.needs_redraw = true;
        }
        Action::InstalledFilter(filter) => {
            state.installed_filter = filter;
            app.needs_redraw = true;
        }
        Action::OpenDetail(name) => {
            state.detail = Some(name.clone());
            state.detail_tab = crate::model::DetailTab::Overview;
            state.detail_files.clear();
            state.detail_scroll.snap_to(0.0);
            backend.load_package_details(name);
            app.needs_redraw = true;
        }
        Action::Back => {
            state.detail = None;
            app.needs_redraw = true;
        }
        Action::SetTab(tab) => {
            state.detail_tab = tab;
            app.needs_redraw = true;
        }
        Action::Install(name) => {
            enqueue_or_preview(app, state, backend, TxJobKind::Install(vec![name]), false);
        }
        Action::Update(name) => {
            enqueue_or_preview(app, state, backend, TxJobKind::UpgradeOne(name), false);
        }
        Action::UpdateAll => {
            enqueue_or_preview(app, state, backend, TxJobKind::UpgradeAll, false);
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
        Action::Reinstall(name) => {
            enqueue_or_preview(app, state, backend, TxJobKind::Reinstall(name), false);
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
                false,
            );
        }
        Action::OpenAppearance => {
            spawn_sibling("spui-settings", &["--page", "appearance"]);
            state.toast("正在打开系统设置的外观页…");
            app.needs_redraw = true;
        }
        Action::ToggleQueue => {
            state.queue_open = !state.queue_open;
            app.needs_redraw = true;
        }
        Action::QueueTab(tab) => {
            state.queue_tab = tab;
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
            enqueue_or_preview(app, state, backend, TxJobKind::RepoRemove(name), false);
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
                state.toast("请填写软件源名称与地址");
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
            let path = "/etc/spk/trusted.d".to_string();
            backend.enqueue_job(TxJobKind::TrustAdd(path), "导入信任密钥");
        }
        Action::FocusSearch => {
            state.search_focus = true;
            app.needs_redraw = true;
        }
        Action::PreviewConfirm => {
            if let Some(preview) = state.preview.take() {
                let mut kind = preview.job.clone();
                if let TxJobKind::Remove {
                    name,
                    ..
                } = &kind
                {
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
        Action::AuthSubmit => {
            if let Some(auth) = state.auth.as_ref() {
                backend.submit_auth(auth.input.clone());
            }
            if let Some(auth) = state.auth.as_mut() {
                auth.input.clear();
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
    _direct: bool,
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

fn apply_backend_event(state: &mut StoreState, event: BackendEvent) {
    match event {
        BackendEvent::CatalogLoaded(Ok(snapshot)) => {
            state.apply_snapshot(snapshot);
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
}
