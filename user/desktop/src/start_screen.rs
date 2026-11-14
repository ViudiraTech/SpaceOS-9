/*
 *
 *       user/desktop/src/start_screen.rs
 *       SPUI 4.0 Full-Screen Start Screen (XJ380 Control Center + Win8 Live Tiles)
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Full-screen Windows 8 / 8.1 inspired Start Screen styled after XJ380's
//! `ctrlmenu` 3-tab control center (`主页` / `应用陈列柜` / `设置中心`),
//! complete with live system tiles, tile size/pin customization persisted via
//! `uxconfd`, global instant search, and an embedded 1:1 shared Settings Center.

use crate::apps::settings::{
    search_settings, Page as SettingsPage, SettingsHost, SettingsRenderStyle, SettingsState,
};
use crate::ddm::wire::PowerAction;
use crate::draw::{mix, Canvas, Rect};
use crate::icons::{self, Icon};
use crate::netmgr::NetStatus;
use crate::shell::{now_local, session_identity, AppCategory, APPS};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, RADIUS_CONTROL, RADIUS_POPUP};
use crate::ui;
use crate::x11::{CursorKind, Key, X};
use std::fs;
use std::time::Instant;
use uxconf::client::UxConfClient;
use uxconf::schema::DEFAULT_START_TILES;
use uxconf::{
    encode_start_tiles, parse_start_tiles, BackdropStyle, DesktopConfig, StartTileSize,
    StartTileSpec, ThemeMode, Value,
};
use x11rb::protocol::xproto::ConnectionExt;

/// Top navigation tabs matching XJ380 `ctrlmenu` 3-tab layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartTab {
    Home,
    Apps,
    Settings,
}

impl StartTab {
    const ALL: [(Self, &'static str); 3] = [
        (Self::Home, "主页"),
        (Self::Apps, "应用陈列柜"),
        (Self::Settings, "设置中心"),
    ];
}

/// Filter categories on the Application Cabinet tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppCabinetFilter {
    All,
    Pinned,
    System,
    Productivity,
    Media,
    Games,
}

impl AppCabinetFilter {
    const ALL: [(Self, &'static str, Icon); 6] = [
        (Self::All, "全部应用", Icon::Applications),
        (Self::Pinned, "已固定磁贴", Icon::Appearance),
        (Self::System, "系统与控制", Icon::Settings),
        (Self::Productivity, "办公与工具", Icon::Editor),
        (Self::Media, "影音与图像", Icon::Music),
        (Self::Games, "益智与游戏", Icon::Minesweeper),
    ];

    fn matches(self, mode: &str, cat: AppCategory, pinned: &[StartTileSpec]) -> bool {
        match self {
            Self::All => true,
            Self::Pinned => pinned.iter().any(|t| t.id == app_tile_id(mode)),
            Self::System => cat == AppCategory::System,
            Self::Productivity => cat == AppCategory::Productivity,
            Self::Media => cat == AppCategory::Media,
            Self::Games => cat == AppCategory::Games,
        }
    }
}

/// Actions propagated from [`StartScreenState`] back to `Shell`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartAction {
    None,
    Redraw,
    Close,
    Launch(&'static str),
    Logout,
    Power(PowerAction),
    ToggleMute,
}

/// Host bridge implementing [`SettingsHost`] inside `Shell`.
pub struct ShellConfigHost<'a> {
    pub x: &'a X,
    pub config: &'a mut DesktopConfig,
    pub ux_client: &'a mut Option<UxConfClient>,
    pub prev_snapshot: Option<DesktopConfig>,
}

impl<'a> ShellConfigHost<'a> {
    pub fn new(
        x: &'a X,
        config: &'a mut DesktopConfig,
        ux_client: &'a mut Option<UxConfClient>,
    ) -> Self {
        Self {
            x,
            config,
            ux_client,
            prev_snapshot: None,
        }
    }

    pub fn take_changed(&mut self) -> Option<DesktopConfig> {
        self.prev_snapshot.take()
    }
}

impl SettingsHost for ShellConfigHost<'_> {
    fn config(&self) -> &DesktopConfig {
        self.config
    }

    fn root_resolution(&self) -> (u16, u16) {
        self.x
            .conn
            .get_geometry(self.x.root)
            .ok()
            .and_then(|c| c.reply().ok())
            .map_or((self.x.width, self.x.height), |g| (g.width, g.height))
    }

    fn set_config_property(&mut self, channel: &str, property: &str, value: Value) {
        if self.ux_client.is_none() {
            *self.ux_client = UxConfClient::connect().ok();
        }
        if let Some(client) = self.ux_client.as_mut() {
            let _ = client.set(channel, property, value.clone());
        }
        let snapshot = self.config.clone();
        if self.config.apply_property(channel, property, &value) && self.prev_snapshot.is_none() {
            self.prev_snapshot = Some(snapshot);
        }
    }

    fn stage_config_property(&mut self, channel: &str, property: &str, value: Value) {
        let snapshot = self.config.clone();
        if self.config.apply_property(channel, property, &value) && self.prev_snapshot.is_none() {
            self.prev_snapshot = Some(snapshot);
        }
    }

    fn reset_config_channel(&mut self, channel: &str) {
        if self.ux_client.is_none() {
            *self.ux_client = UxConfClient::connect().ok();
        }
        let snapshot = self.config.clone();
        let mut applied_from_daemon = false;
        if let Some(client) = self.ux_client.as_mut() {
            let _ = client.reset(channel, "/", true);
            if let Ok(entries) = client.get_all(channel, "/") {
                for (prop, val) in entries {
                    self.config.apply_property(channel, &prop, &val);
                }
                applied_from_daemon = true;
            }
        }
        if !applied_from_daemon {
            for (prop, val) in uxconf::schema::channel_defaults(channel) {
                self.config.apply_property(channel, prop, &val);
            }
        }
        if self.config != &snapshot && self.prev_snapshot.is_none() {
            self.prev_snapshot = Some(snapshot);
        }
    }

    fn factory_reset(&mut self) -> Option<std::result::Result<(), uxconf::ConfError>> {
        if self.ux_client.is_none() {
            *self.ux_client = UxConfClient::connect().ok();
        }
        self.ux_client.as_mut().map(|c| c.factory_reset())
    }
}

/// Interactive targets registered during Start Screen rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum StartHit {
    Tab(StartTab),
    SearchBox,
    ClearSearch,
    Logout,
    Reboot,
    PowerOff,
    ConfirmPowerOk,
    ConfirmPowerCancel,
    CloseDesktop,
    OpenSettings(Option<SettingsPage>),
    AudioTileMute,
    Tile(usize),
    ResetTiles,
    AppFilter(AppCabinetFilter),
    AppCard(usize),
    SearchApp(usize),
    SearchSetting(usize),
    SearchPower(PowerAction),
    CtxResize(StartTileSize),
    CtxTogglePin,
    CtxOpenStandaloneSettings,
    CtxResetTiles,
}

struct StartHitBox {
    rect: Rect,
    target: StartHit,
    ctx_tile_id: Option<String>,
    ctx_app_mode: Option<&'static str>,
}

/// Right-click context menu overlay for any tile or app card.
#[derive(Clone, Debug)]
struct TileCtxMenu {
    x: i32,
    y: i32,
    tile_id: String,
    app_mode: Option<&'static str>,
}

impl TileCtxMenu {
    fn rect(&self, screen_w: i32, screen_h: i32) -> Rect {
        let h = self.item_count() as i32 * 32 + 12;
        let w = 204;
        Rect::new(
            self.x.clamp(8, (screen_w - w - 8).max(8)),
            self.y.clamp(8, (screen_h - h - 8).max(8)),
            w,
            h,
        )
    }

    fn item_count(&self) -> usize {
        if self.app_mode == Some("settings") {
            7
        } else {
            6
        }
    }
}

/// Placed tile rectangle generated by [`pack_tile_group`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedTile {
    pub id: String,
    pub size: StartTileSize,
    pub rect: Rect,
}

/// Non-blocking `/proc/stat` CPU time counter snapshot.
#[derive(Clone, Copy, Default)]
struct CpuTicks {
    total: u64,
    idle: u64,
}

/// Non-blocking `/proc/net/dev` byte counter snapshot.
#[derive(Clone, Copy, Default)]
struct NetByteSample {
    rx_bytes: u64,
    tx_bytes: u64,
    at: Option<Instant>,
}

#[derive(Clone, PartialEq, Eq)]
struct BackdropSig {
    w: u16,
    h: u16,
    wallpaper: String,
    style: BackdropStyle,
    solid: u32,
    mode: ThemeMode,
}

/// Responsive top-header layout coordinates guaranteed overlap-free at 800..1920+ px.
#[derive(Clone, Copy, Debug)]
pub struct HeaderLayout {
    pub height: i32,
    pub tabs: [Rect; 3],
    pub search: Rect,
    pub clock: Rect,
    pub logout: Rect,
    pub reboot: Rect,
    pub power_off: Rect,
    pub close_desktop: Rect,
    pub compact: bool,
}

impl HeaderLayout {
    pub fn compute(w: i32, h: i32) -> Self {
        let compact = w < 1140;
        let height = if h < 660 { 82 } else { 94 };
        let brand_end = if compact { 104 } else { 188 };
        let tab_w = if compact { 88 } else { 120 };
        let tab_h = if compact { 36 } else { 40 };
        let tab_y = (height - 3 - tab_h) / 2;
        let tab_gap = 6;
        let tabs = [
            Rect::new(brand_end, tab_y, tab_w, tab_h),
            Rect::new(brand_end + tab_w + tab_gap, tab_y, tab_w, tab_h),
            Rect::new(brand_end + (tab_w + tab_gap) * 2, tab_y, tab_w, tab_h),
        ];
        let tabs_right = tabs[2].right();

        let btn_h = 34;
        let btn_y = (height - 3 - btn_h) / 2;
        let gap = 6;
        let close_w = if compact { 32 } else { 82 };
        let power_w = if compact { 32 } else { 66 };
        let reboot_w = if compact { 32 } else { 66 };
        let logout_w = if compact { 32 } else { 66 };
        let clock_w = if compact { 78 } else { 108 };
        let margin_r = if compact { 16 } else { 20 };

        let close_desktop = Rect::new(w - margin_r - close_w, btn_y, close_w, btn_h);
        let power_off = Rect::new(close_desktop.x - gap - power_w, btn_y, power_w, btn_h);
        let reboot = Rect::new(power_off.x - gap - reboot_w, btn_y, reboot_w, btn_h);
        let logout = Rect::new(reboot.x - gap - logout_w, btn_y, logout_w, btn_h);
        let clock = Rect::new(logout.x - 10 - clock_w, (height - 44) / 2, clock_w, 42);

        let search_x = tabs_right + if compact { 10 } else { 18 };
        let max_search_right = clock.x - 10;
        let search_w = (max_search_right - search_x).clamp(120, 260);
        let search_h = 34;
        let search = Rect::new(
            (max_search_right - search_w).max(search_x),
            (height - 3 - search_h) / 2,
            search_w.min((max_search_right - search_x).max(96)),
            search_h,
        );

        Self {
            height,
            tabs,
            search,
            clock,
            logout,
            reboot,
            power_off,
            close_desktop,
            compact,
        }
    }
}

/// Complete runtime state for the Full-Screen Start Screen window.
pub struct StartScreenState {
    pub tab: StartTab,
    pub query: String,
    pub app_filter: AppCabinetFilter,
    pub settings: Option<SettingsState>,
    home_scroll: ui::SmoothScroll,
    apps_scroll: ui::SmoothScroll,
    hover: Option<StartHit>,
    hits: Vec<StartHitBox>,
    ctx: Option<TileCtxMenu>,
    confirm_power: Option<PowerAction>,
    backdrop: Vec<u32>,
    backdrop_sig: Option<BackdropSig>,
    prev_cpu: Option<CpuTicks>,
    cpu_pct: u8,
    mem_used_mb: u64,
    mem_total_mb: u64,
    mem_pct: u8,
    uptime_str: String,
    net_bytes: NetByteSample,
    rx_kbps: u64,
    tx_kbps: u64,
    last_sample: Instant,
    music_track: String,
    kernel_summary: String,
    identity: String,
    last_clock: (i32, i32, i32),
}

impl StartScreenState {
    pub fn new() -> Self {
        Self {
            tab: StartTab::Home,
            query: String::new(),
            app_filter: AppCabinetFilter::All,
            settings: None,
            home_scroll: ui::SmoothScroll::new("start_home_scroll"),
            apps_scroll: ui::SmoothScroll::new("start_apps_scroll"),
            hover: None,
            hits: Vec::with_capacity(96),
            ctx: None,
            confirm_power: None,
            backdrop: Vec::new(),
            backdrop_sig: None,
            prev_cpu: read_proc_stat_ticks(),
            cpu_pct: 0,
            mem_used_mb: 0,
            mem_total_mb: 0,
            mem_pct: 0,
            uptime_str: String::from("--"),
            net_bytes: NetByteSample::default(),
            rx_kbps: 0,
            tx_kbps: 0,
            last_sample: Instant::now() - std::time::Duration::from_secs(5),
            music_track: detect_default_track_title(),
            kernel_summary: read_kernel_summary(),
            identity: session_identity(),
            last_clock: (-1, -1, -1),
        }
    }

    pub fn invalidate_backdrop(&mut self) {
        self.backdrop_sig = None;
    }

    pub fn on_open(&mut self) {
        self.tab = StartTab::Home;
        self.query.clear();
        self.ctx = None;
        self.confirm_power = None;
        self.hover = None;
        self.home_scroll.snap_to(0.0);
        self.apps_scroll.snap_to(0.0);
        if let Some(s) = self.settings.as_mut() {
            s.clear_text_focus();
        }
        self.sample_metrics(true);
    }

    pub fn ensure_settings(&mut self, host: &impl SettingsHost) -> &mut SettingsState {
        if self.settings.is_none() {
            self.settings = Some(SettingsState::new(host, None));
        }
        let s = self.settings.as_mut().unwrap();
        s.set_search_query(&self.query);
        s
    }

    pub fn on_config_changed(&mut self, host: &impl SettingsHost) {
        if let Some(settings) = self.settings.as_mut() {
            settings.on_config_changed(host);
        }
        self.backdrop_sig = None;
    }

    pub fn on_net_event(&mut self) -> bool {
        self.settings
            .as_mut()
            .is_some_and(|s| s.refresh_net_now() && self.tab == StartTab::Settings)
    }

    pub fn tick(&mut self, cfg: &DesktopConfig) -> bool {
        let mut dirty = false;
        let (_, _, _, hour, minute, sec, _) = now_local();
        let stamp = (hour, minute, sec);
        if stamp != self.last_clock {
            self.last_clock = stamp;
            dirty = true;
        }
        if self.last_sample.elapsed().as_millis() >= 1000 {
            self.sample_metrics(false);
            dirty = true;
        }
        if self.tab == StartTab::Settings {
            if let Some(settings) = self.settings.as_mut() {
                if settings.tick(cfg) {
                    dirty = true;
                }
            }
        }
        dirty
    }

    fn sample_metrics(&mut self, _force: bool) {
        self.last_sample = Instant::now();
        if let Some(curr) = read_proc_stat_ticks() {
            if let Some(prev) = self.prev_cpu {
                let d_total = curr.total.saturating_sub(prev.total);
                let d_idle = curr.idle.saturating_sub(prev.idle);
                if d_total > 0 {
                    let busy = d_total.saturating_sub(d_idle);
                    self.cpu_pct = ((busy * 100) / d_total).min(100) as u8;
                }
            }
            self.prev_cpu = Some(curr);
        }
        if let Some((used_mb, total_mb)) = read_mem_info_mb() {
            self.mem_used_mb = used_mb;
            self.mem_total_mb = total_mb;
            self.mem_pct = if total_mb > 0 {
                ((used_mb * 100) / total_mb).min(100) as u8
            } else {
                0
            };
        }
        self.uptime_str = read_uptime_label();
        if let Some((rx, tx)) = read_net_bytes() {
            let now = Instant::now();
            if let Some(prev_at) = self.net_bytes.at {
                let dt_ms = now.duration_since(prev_at).as_millis().max(1) as u64;
                let drx = rx.saturating_sub(self.net_bytes.rx_bytes);
                let dtx = tx.saturating_sub(self.net_bytes.tx_bytes);
                self.rx_kbps = (drx * 1000) / (dt_ms * 1024);
                self.tx_kbps = (dtx * 1000) / (dt_ms * 1024);
            }
            self.net_bytes = NetByteSample {
                rx_bytes: rx,
                tx_bytes: tx,
                at: Some(now),
            };
        }
    }

    fn open_settings_tab(&mut self, host: &impl SettingsHost, page: Option<SettingsPage>) {
        let q = self.query.clone();
        let st = self.ensure_settings(host);
        if let Some(p) = page {
            st.set_page(host, p);
        }
        st.set_search_query(&q);
        self.tab = StartTab::Settings;
        self.ctx = None;
    }

    pub fn on_wheel(&mut self, win_w: i32, win_h: i32, x: i32, y: i32, delta: i32) -> bool {
        if self.ctx.is_some() {
            return false;
        }
        let hdr = HeaderLayout::compute(win_w, win_h);
        if y < hdr.height {
            return false;
        }
        match self.tab {
            StartTab::Home => self.home_scroll.wheel(delta as f32 * 28.0),
            StartTab::Apps => self.apps_scroll.wheel(delta as f32 * 28.0),
            StartTab::Settings => self
                .settings
                .as_mut()
                .is_some_and(|s| s.on_wheel(x, y, delta)),
        }
    }

    pub fn on_motion(
        &mut self,
        host: &mut ShellConfigHost<'_>,
        x: i32,
        y: i32,
    ) -> (bool, CursorKind) {
        if self.tab == StartTab::Settings && self.ctx.is_none() {
            let hdr_h = HeaderLayout::compute(host.x.width as i32, host.x.height as i32).height;
            if y >= hdr_h {
                if let Some(settings) = self.settings.as_mut() {
                    let res = settings.on_motion(host, x, y);
                    if self.hover.is_some() {
                        self.hover = None;
                        return (true, res.1);
                    }
                    return res;
                }
            }
        }

        let mut redraw = match self.tab {
            StartTab::Home => self.home_scroll.handle_motion(x, y),
            StartTab::Apps => self.apps_scroll.handle_motion(x, y),
            StartTab::Settings => false,
        };
        let next = self.find_hit(x, y);
        let cursor = if next.is_some() {
            if matches!(next, Some(StartHit::SearchBox)) {
                CursorKind::Text
            } else {
                CursorKind::Pointer
            }
        } else {
            CursorKind::Default
        };
        if next != self.hover {
            self.hover = next;
            redraw = true;
        }
        (redraw, cursor)
    }

    pub fn on_leave(&mut self) -> bool {
        if self.hover.take().is_some() {
            return true;
        }
        false
    }

    pub fn on_button(
        &mut self,
        host: &mut ShellConfigHost<'_>,
        x: i32,
        y: i32,
        button: u8,
        down: bool,
    ) -> StartAction {
        let win_w = i32::from(host.x.width);
        let win_h = i32::from(host.x.height);
        let hdr = HeaderLayout::compute(win_w, win_h);

        // Power confirmation modal blocks underlying controls until confirmed or dismissed.
        if let Some(action) = self.confirm_power {
            if down && button == 1 {
                if let Some(hit) = self.find_hit(x, y) {
                    return match hit {
                        StartHit::ConfirmPowerOk => {
                            crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                            self.confirm_power = None;
                            StartAction::Power(action)
                        }
                        StartHit::ConfirmPowerCancel => {
                            crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                            self.confirm_power = None;
                            StartAction::Redraw
                        }
                        _ => StartAction::None,
                    };
                }
                if !confirm_power_card_rect(win_w, win_h).contains(x, y) {
                    self.confirm_power = None;
                    return StartAction::Redraw;
                }
            }
            return StartAction::None;
        }

        // Right-click menu modal takes priority when open.
        if let Some(ctx) = self.ctx.clone() {
            let menu_r = ctx.rect(win_w, win_h);
            if down && !menu_r.contains(x, y) {
                self.ctx = None;
                return StartAction::Redraw;
            }
            if down && button == 1 {
                if let Some(hit) = self.find_hit(x, y) {
                    return self.handle_hit(host, hit, x, y);
                }
            }
            return StartAction::None;
        }

        if button == 3 && down && y >= hdr.height {
            if let Some(hb) = self
                .hits
                .iter()
                .rev()
                .find(|hb| hb.rect.contains(x, y) && hb.ctx_tile_id.is_some())
            {
                if let Some(tile_id) = hb.ctx_tile_id.clone() {
                    self.ctx = Some(TileCtxMenu {
                        x,
                        y,
                        tile_id,
                        app_mode: hb.ctx_app_mode,
                    });
                    crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                    return StartAction::Redraw;
                }
            }
        }

        // Scrollbar drag/click on Home or Apps.
        let scrolled = match self.tab {
            StartTab::Home => self.home_scroll.handle_button(x, y, button, down),
            StartTab::Apps => self.apps_scroll.handle_button(x, y, button, down),
            StartTab::Settings => false,
        };
        if scrolled {
            return StartAction::Redraw;
        }

        if self.tab == StartTab::Settings && y >= hdr.height {
            if let Some(settings) = self.settings.as_mut() {
                let act = settings.on_button(host, x, y, button, down);
                if act.back_to_home {
                    crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                    self.tab = StartTab::Home;
                    self.hover = None;
                    return StartAction::Redraw;
                }
                if act.redraw {
                    return StartAction::Redraw;
                }
            }
        }

        if down && button == 1 {
            if let Some(hit) = self.find_hit(x, y) {
                return self.handle_hit(host, hit, x, y);
            }
        }
        StartAction::None
    }

    pub fn on_key(&mut self, host: &mut ShellConfigHost<'_>, key: Key) -> StartAction {
        if let Some(action) = self.confirm_power {
            if key.is_enter() {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                self.confirm_power = None;
                return StartAction::Power(action);
            }
            if key.is_escape() {
                self.confirm_power = None;
                return StartAction::Redraw;
            }
            return StartAction::None;
        }

        if self.ctx.is_some() {
            if key.is_escape() {
                self.ctx = None;
                return StartAction::Redraw;
            }
            return StartAction::None;
        }

        if self.tab == StartTab::Settings {
            if let Some(settings) = self.settings.as_mut() {
                if settings.is_text_input_focused() {
                    if settings.on_key(host, key) {
                        return StartAction::Redraw;
                    }
                    return StartAction::None;
                }
            }
        }

        if key.is_escape() {
            if !self.query.is_empty() {
                self.query.clear();
                if let Some(s) = self.settings.as_mut() {
                    s.set_search_query("");
                }
                return StartAction::Redraw;
            }
            if self.tab != StartTab::Home {
                self.tab = StartTab::Home;
                return StartAction::Redraw;
            }
            return StartAction::Close;
        }

        if key.control() {
            if let Some(ch) = key.text() {
                match ch {
                    '1' => {
                        self.tab = StartTab::Home;
                        return StartAction::Redraw;
                    }
                    '2' => {
                        self.tab = StartTab::Apps;
                        return StartAction::Redraw;
                    }
                    '3' => {
                        self.open_settings_tab(host, None);
                        return StartAction::Redraw;
                    }
                    _ => {}
                }
            }
        }

        if key.is_backspace() {
            if self.query.pop().is_some() {
                let q = self.query.clone();
                if let Some(s) = self.settings.as_mut() {
                    s.set_search_query(&q);
                }
                return StartAction::Redraw;
            }
            return StartAction::None;
        }

        if key.is_enter() && !self.query.trim().is_empty() {
            let matching_apps = filtered_apps_for(&self.query, None, &[]);
            if let Some(&app_idx) = matching_apps.first() {
                let mode = APPS[app_idx].mode;
                if mode == "settings" {
                    self.open_settings_tab(host, None);
                    return StartAction::Redraw;
                }
                return StartAction::Launch(mode);
            }
            let matching_settings = search_settings(&self.query);
            if let Some(first_set) = matching_settings.first() {
                let page = first_set.page;
                self.open_settings_tab(host, Some(page));
                return StartAction::Redraw;
            }
            if let Some(action) = matching_power_actions(&self.query).first().copied() {
                self.confirm_power = Some(action);
                return StartAction::Redraw;
            }
        }

        if !key.control() && !key.alt() {
            if let Some(ch) = key.text().filter(|c| !c.is_control()) {
                if self.query.chars().count() < 28 {
                    self.query.push(ch);
                    let q = self.query.clone();
                    if let Some(s) = self.settings.as_mut() {
                        s.set_search_query(&q);
                    }
                    return StartAction::Redraw;
                }
            }
        }

        StartAction::None
    }

    fn handle_hit(
        &mut self,
        host: &mut ShellConfigHost<'_>,
        hit: StartHit,
        _x: i32,
        _y: i32,
    ) -> StartAction {
        match hit {
            StartHit::Tab(tab) => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                self.ctx = None;
                self.hover = None;
                if let Some(s) = self.settings.as_mut() {
                    s.clear_text_focus();
                }
                if tab == StartTab::Settings {
                    self.open_settings_tab(host, None);
                } else {
                    self.tab = tab;
                }
                StartAction::Redraw
            }
            StartHit::SearchBox => {
                if let Some(s) = self.settings.as_mut() {
                    s.clear_text_focus();
                }
                StartAction::Redraw
            }
            StartHit::ClearSearch => {
                self.query.clear();
                if let Some(s) = self.settings.as_mut() {
                    s.clear_text_focus();
                    s.set_search_query("");
                }
                StartAction::Redraw
            }
            StartHit::Logout => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                StartAction::Logout
            }
            StartHit::Reboot => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                self.confirm_power = Some(PowerAction::Reboot);
                StartAction::Redraw
            }
            StartHit::PowerOff => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                self.confirm_power = Some(PowerAction::PowerOff);
                StartAction::Redraw
            }
            StartHit::ConfirmPowerOk => {
                if let Some(action) = self.confirm_power.take() {
                    crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                    StartAction::Power(action)
                } else {
                    StartAction::None
                }
            }
            StartHit::ConfirmPowerCancel => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                self.confirm_power = None;
                StartAction::Redraw
            }
            StartHit::SearchPower(action) => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                self.confirm_power = Some(action);
                StartAction::Redraw
            }
            StartHit::CloseDesktop => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                StartAction::Close
            }
            StartHit::OpenSettings(page) => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                self.open_settings_tab(host, page);
                StartAction::Redraw
            }
            StartHit::AudioTileMute => StartAction::ToggleMute,
            StartHit::Tile(idx) => {
                let specs = parse_start_tiles(&host.config().panel_start_tiles);
                if let Some(spec) = specs.get(idx) {
                    crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                    return self.activate_tile(host, &spec.id);
                }
                StartAction::None
            }
            StartHit::ResetTiles => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                host.set_config_property(
                    "panel",
                    "/start-tiles",
                    Value::String(DEFAULT_START_TILES.to_string()),
                );
                StartAction::Redraw
            }
            StartHit::AppFilter(filter) => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                self.app_filter = filter;
                self.apps_scroll.snap_to(0.0);
                StartAction::Redraw
            }
            StartHit::AppCard(app_idx) | StartHit::SearchApp(app_idx) => {
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                let mode = APPS[app_idx].mode;
                if mode == "settings" {
                    self.open_settings_tab(host, None);
                    StartAction::Redraw
                } else {
                    StartAction::Launch(mode)
                }
            }
            StartHit::SearchSetting(idx) => {
                let matches = search_settings(&self.query);
                if let Some(item) = matches.get(idx) {
                    let page = item.page;
                    crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                    self.open_settings_tab(host, Some(page));
                    return StartAction::Redraw;
                }
                StartAction::None
            }
            StartHit::CtxResize(new_size) => {
                if let Some(ctx) = self.ctx.take() {
                    crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                    let mut specs = parse_start_tiles(&host.config().panel_start_tiles);
                    if let Some(existing) = specs.iter_mut().find(|t| t.id == ctx.tile_id) {
                        existing.size = new_size;
                    } else {
                        specs.push(StartTileSpec {
                            id: ctx.tile_id,
                            size: new_size,
                        });
                    }
                    host.set_config_property(
                        "panel",
                        "/start-tiles",
                        Value::String(encode_start_tiles(&specs)),
                    );
                }
                StartAction::Redraw
            }
            StartHit::CtxTogglePin => {
                if let Some(ctx) = self.ctx.take() {
                    crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                    let mut specs = parse_start_tiles(&host.config().panel_start_tiles);
                    if let Some(pos) = specs.iter().position(|t| t.id == ctx.tile_id) {
                        specs.remove(pos);
                    } else {
                        specs.push(StartTileSpec {
                            id: ctx.tile_id,
                            size: StartTileSize::Medium,
                        });
                    }
                    host.set_config_property(
                        "panel",
                        "/start-tiles",
                        Value::String(encode_start_tiles(&specs)),
                    );
                }
                StartAction::Redraw
            }
            StartHit::CtxOpenStandaloneSettings => {
                self.ctx = None;
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                StartAction::Launch("settings")
            }
            StartHit::CtxResetTiles => {
                self.ctx = None;
                crate::sound::play(host.config(), crate::sound::SystemSound::Click);
                host.set_config_property(
                    "panel",
                    "/start-tiles",
                    Value::String(DEFAULT_START_TILES.to_string()),
                );
                StartAction::Redraw
            }
        }
    }

    fn activate_tile(&mut self, host: &impl SettingsHost, tile_id: &str) -> StartAction {
        match tile_id {
            "status-clock" => {
                self.open_settings_tab(host, Some(SettingsPage::DateTime));
                StartAction::Redraw
            }
            "status-net" => {
                self.open_settings_tab(host, Some(SettingsPage::Network));
                StartAction::Redraw
            }
            "status-audio" => {
                self.open_settings_tab(host, Some(SettingsPage::Sound));
                StartAction::Redraw
            }
            "status-sys" => StartAction::Launch("monitor"),
            "app-settings" => {
                self.open_settings_tab(host, None);
                StartAction::Redraw
            }
            other => {
                if let Some(mode) = other.strip_prefix("app-") {
                    if let Some(entry) = APPS.iter().find(|a| a.mode == mode) {
                        return StartAction::Launch(entry.mode);
                    }
                }
                StartAction::None
            }
        }
    }

    fn find_hit(&self, x: i32, y: i32) -> Option<StartHit> {
        self.hits
            .iter()
            .rev()
            .find_map(|hb| hb.rect.contains(x, y).then_some(hb.target))
    }

    fn push_hit(&mut self, rect: Rect, target: StartHit) {
        self.hits.push(StartHitBox {
            rect,
            target,
            ctx_tile_id: None,
            ctx_app_mode: None,
        });
    }

    fn push_tile_hit(
        &mut self,
        rect: Rect,
        viewport: Rect,
        target: StartHit,
        tile_id: String,
        app_mode: Option<&'static str>,
    ) {
        let clipped = rect.intersect(viewport);
        if !clipped.is_empty() {
            self.hits.push(StartHitBox {
                rect: clipped,
                target,
                ctx_tile_id: Some(tile_id),
                ctx_app_mode: app_mode,
            });
        }
    }

    fn ensure_backdrop(
        &mut self,
        w: u16,
        h: u16,
        config: &DesktopConfig,
        wallpaper_scaled: &[u32],
    ) {
        let sig = BackdropSig {
            w,
            h,
            wallpaper: config.wallpaper.clone(),
            style: config.backdrop_style,
            solid: config.solid_color,
            mode: config.mode,
        };
        if self.backdrop_sig.as_ref() == Some(&sig)
            && self.backdrop.len() == usize::from(w) * usize::from(h)
        {
            return;
        }
        self.backdrop = build_start_backdrop(w, h, config, wallpaper_scaled);
        self.backdrop_sig = Some(sig);
    }

    /// Render the complete full-screen Start Screen frame onto `canvas`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        host: &ShellConfigHost<'_>,
        net_status: &NetStatus,
        wallpaper_scaled: &[u32],
        open_windows: usize,
    ) {
        let cfg = host.config();
        let w = canvas.width as i32;
        let h = canvas.height as i32;
        self.ensure_backdrop(w as u16, h as u16, cfg, wallpaper_scaled);
        self.hits.clear();

        if self.backdrop.len() == (w * h) as usize {
            canvas.copy_background(&self.backdrop);
        } else {
            canvas.clear(theme.bg_top);
        }

        let hdr = HeaderLayout::compute(w, h);
        self.draw_header(canvas, text, theme, cfg, hdr);

        let body_rect = Rect::new(0, hdr.height, w, (h - hdr.height).max(1));
        match self.tab {
            StartTab::Home => {
                self.draw_home(
                    canvas,
                    text,
                    theme,
                    cfg,
                    net_status,
                    body_rect,
                    open_windows,
                );
            }
            StartTab::Apps => {
                self.draw_apps(canvas, text, theme, cfg, body_rect);
            }
            StartTab::Settings => {
                let st = self.ensure_settings(host);
                st.draw(
                    canvas,
                    text,
                    theme,
                    cfg,
                    body_rect,
                    SettingsRenderStyle::Xj380,
                );
            }
        }

        if self.ctx.is_some() {
            self.draw_ctx_menu(canvas, text, theme, cfg, w, h);
        }
        if let Some(action) = self.confirm_power {
            self.draw_power_confirm(canvas, text, theme, action, w, h);
        }
    }

    fn draw_header(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        hdr: HeaderLayout,
    ) {
        let w = canvas.width as i32;
        let hdr_bg = if theme.mode == ThemeMode::Dark {
            0x0a1019
        } else {
            0xeaf0f6
        };
        canvas.fill_alpha(Rect::new(0, 0, w, hdr.height - 3), hdr_bg, 0.92);

        // XJ380 branding on the left.
        let logo_sz = if hdr.compact { 26 } else { 32 };
        let logo_x = if hdr.compact { 16 } else { 24 };
        let logo_y = (hdr.height - 3 - logo_sz) / 2;
        icons::draw(
            canvas,
            Icon::SpaceLogo,
            logo_x,
            logo_y,
            logo_sz as u32,
            None,
        );
        if hdr.compact {
            text.draw(
                canvas,
                "开始",
                (logo_x + logo_sz + 8) as f32,
                (hdr.height as f32 * 0.5) + 4.0,
                16.0,
                theme.text,
                Face::Bold,
            );
        } else {
            text.draw(
                canvas,
                "控制中心",
                (logo_x + logo_sz + 12) as f32,
                (logo_y + 16) as f32,
                17.0,
                theme.text,
                Face::Bold,
            );
            text.draw(
                canvas,
                "SPACEOS 9 · SPUI",
                (logo_x + logo_sz + 12) as f32,
                (logo_y + 31) as f32,
                10.0,
                theme.accent,
                Face::Bold,
            );
        }

        // Animated active tab indicator under XJ380 Metro tab buttons.
        let active_idx = StartTab::ALL
            .iter()
            .position(|(t, _)| *t == self.tab)
            .unwrap_or(0);
        let target_tab_x = hdr.tabs[active_idx].x as f32;
        let anim_x = ui::animate_float("start_tab_x", target_tab_x, ui::MOTION_INDICATOR);
        let tab_r = hdr.tabs[active_idx];
        canvas.rounded_rect(
            Rect::new(anim_x.round() as i32, tab_r.y, tab_r.w, tab_r.h),
            RADIUS_CONTROL,
            theme.accent,
            0.22,
        );

        for (i, (tab, label)) in StartTab::ALL.iter().enumerate() {
            let r = hdr.tabs[i];
            let active = *tab == self.tab;
            let hov = self.hover == Some(StartHit::Tab(*tab));
            let fill = if active {
                theme.accent
            } else if hov {
                theme.panel_hover
            } else if theme.mode == ThemeMode::Dark {
                0x192334
            } else {
                0xdde5ef
            };
            let fill_alpha = if active { 0.88 } else { 0.80 };
            canvas.rounded_rect(r, RADIUS_CONTROL, fill, fill_alpha);
            canvas.rounded_border(
                r,
                RADIUS_CONTROL,
                if active || hov {
                    theme.accent
                } else {
                    theme.panel_border
                },
                if active { 0.9 } else { 0.6 },
                1,
            );
            let txt_col = if active { 0xffffff } else { theme.text };
            text.draw_centered(
                canvas,
                label,
                r.x as f32 + r.w as f32 * 0.5,
                (r.y + r.h / 2 + 5) as f32,
                if hdr.compact { 12.0 } else { 13.0 },
                txt_col,
                if active { Face::Bold } else { Face::Ui },
            );
            self.push_hit(r, StartHit::Tab(*tab));
        }

        // Global Search Input.
        let placeholder = match self.tab {
            StartTab::Settings => "搜索设置项（跨分类跳转）...",
            _ => "键入搜索应用或系统设置...",
        };
        ui::text_input(
            canvas,
            text,
            theme,
            hdr.search,
            &self.query,
            placeholder,
            self.tab != StartTab::Settings
                || self
                    .settings
                    .as_ref()
                    .is_none_or(|s| !s.is_text_input_focused()),
            Some(Icon::Search),
        );
        self.push_hit(hdr.search, StartHit::SearchBox);
        if !self.query.is_empty() {
            let clr_r = Rect::new(hdr.search.right() - 26, hdr.search.y + 5, 22, 24);
            text.draw_centered(
                canvas,
                "×",
                clr_r.x as f32 + 11.0,
                (clr_r.y + 17) as f32,
                14.0,
                theme.text_muted,
                Face::Bold,
            );
            self.push_hit(clr_r, StartHit::ClearSearch);
        }

        // Top-right Live Date/Time.
        let (year, month, day, hour, minute, sec, wday) = now_local();
        let _ = year;
        let time_str = if cfg.clock_24h {
            if cfg.clock_seconds {
                format!("{hour:02}:{minute:02}:{sec:02}")
            } else {
                format!("{hour:02}:{minute:02}")
            }
        } else {
            let period = if hour < 12 { "上午" } else { "下午" };
            let h12 = match hour % 12 {
                0 => 12,
                v => v,
            };
            format!("{period} {h12:02}:{minute:02}")
        };
        let wday_zh = match wday {
            0 => "周日",
            1 => "周一",
            2 => "周二",
            3 => "周三",
            4 => "周四",
            5 => "周五",
            _ => "周六",
        };
        let date_str = format!("{month:02}/{day:02} {wday_zh}");
        text.draw_right(
            canvas,
            &time_str,
            hdr.clock.right() as f32,
            (hdr.clock.y + 20) as f32,
            if hdr.compact { 15.0 } else { 17.5 },
            theme.text,
            Face::Bold,
        );
        text.draw_right(
            canvas,
            &date_str,
            hdr.clock.right() as f32,
            (hdr.clock.y + 37) as f32,
            11.0,
            theme.text_dim,
            Face::Ui,
        );

        // Logout, Reboot, PowerOff and Close/Desktop buttons.
        if hdr.compact {
            ui::button(
                canvas,
                text,
                theme,
                hdr.logout,
                "",
                Some(Icon::Logout),
                false,
                self.hover == Some(StartHit::Logout),
            );
            ui::button(
                canvas,
                text,
                theme,
                hdr.reboot,
                "",
                Some(Icon::Refresh),
                false,
                self.hover == Some(StartHit::Reboot),
            );
            ui::button(
                canvas,
                text,
                theme,
                hdr.power_off,
                "",
                Some(Icon::Shutdown),
                false,
                self.hover == Some(StartHit::PowerOff),
            );
            ui::button(
                canvas,
                text,
                theme,
                hdr.close_desktop,
                "×",
                None,
                true,
                self.hover == Some(StartHit::CloseDesktop),
            );
        } else {
            ui::button(
                canvas,
                text,
                theme,
                hdr.logout,
                "注销",
                Some(Icon::Logout),
                false,
                self.hover == Some(StartHit::Logout),
            );
            ui::button(
                canvas,
                text,
                theme,
                hdr.reboot,
                "重启",
                Some(Icon::Refresh),
                false,
                self.hover == Some(StartHit::Reboot),
            );
            ui::button(
                canvas,
                text,
                theme,
                hdr.power_off,
                "关机",
                Some(Icon::Shutdown),
                false,
                self.hover == Some(StartHit::PowerOff),
            );
            ui::button(
                canvas,
                text,
                theme,
                hdr.close_desktop,
                "桌面 →",
                None,
                true,
                self.hover == Some(StartHit::CloseDesktop),
            );
        }
        self.push_hit(hdr.logout, StartHit::Logout);
        self.push_hit(hdr.reboot, StartHit::Reboot);
        self.push_hit(hdr.power_off, StartHit::PowerOff);
        self.push_hit(hdr.close_desktop, StartHit::CloseDesktop);

        // Signature XJ380 horizontal accent line beneath top bar.
        canvas.fill(Rect::new(0, hdr.height - 3, w, 3), theme.accent);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_home(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        net_status: &NetStatus,
        body: Rect,
        open_windows: usize,
    ) {
        let footer_h = 46;
        let margin_x = if body.w >= 1100 { 42 } else { 24 };
        let content_w = (body.w - margin_x * 2).clamp(320, 1280);
        let content_x = body.x + (body.w - content_w) / 2;
        let viewport = Rect::new(body.x, body.y, body.w, (body.h - footer_h).max(80));

        let scroll_y = self.home_scroll.offset_i32().max(0);
        let start_y = viewport.y + 18 - scroll_y;
        let mut end_y = start_y;

        canvas.with_clip(viewport, |canvas| {
            let mut y = start_y;

            // Global Search Results View on Home if query is non-empty.
            if !self.query.trim().is_empty() {
                y = self.draw_home_search_results(
                    canvas, text, theme, viewport, content_x, content_w, y,
                );
            }

            // XJ380 Top Summary Row: Account Card + OS & System Card.
            let card_h = 88;
            let card_gap = 16;
            let acct_w = ((content_w - card_gap) * 42) / 100;
            let sys_w = content_w - card_gap - acct_w;
            let acct_rect = Rect::new(content_x, y, acct_w, card_h);
            let sys_rect = Rect::new(content_x + acct_w + card_gap, y, sys_w, card_h);

            draw_xj380_card(canvas, theme, acct_rect, theme.accent, false, 0.88);
            icons::draw(
                canvas,
                Icon::SpaceLogo,
                acct_rect.x + 16,
                acct_rect.y + 20,
                48,
                None,
            );
            let id_shown = text.truncate(
                &self.identity,
                (acct_w - 190).max(80) as f32,
                15.0,
                Face::Bold,
            );
            text.draw(
                canvas,
                &id_shown,
                (acct_rect.x + 78) as f32,
                (acct_rect.y + 38) as f32,
                15.0,
                theme.text,
                Face::Bold,
            );
            text.draw(
                canvas,
                "本地桌面会话 · 已认证",
                (acct_rect.x + 78) as f32,
                (acct_rect.y + 60) as f32,
                11.5,
                theme.text_muted,
                Face::Ui,
            );
            let set_btn = Rect::new(acct_rect.right() - 102, acct_rect.y + 27, 88, 34);
            ui::button(
                canvas,
                text,
                theme,
                set_btn,
                "设置中心",
                None,
                true,
                self.hover == Some(StartHit::OpenSettings(None)),
            );
            if let Some(clipped) = non_empty_clip(set_btn, viewport) {
                self.push_hit(clipped, StartHit::OpenSettings(None));
            }

            draw_xj380_card(canvas, theme, sys_rect, 0x10b981, false, 0.88);
            text.draw(
                canvas,
                "SpaceOS 9 桌面环境 · SPUI 4.0",
                (sys_rect.x + 18) as f32,
                (sys_rect.y + 36) as f32,
                15.0,
                theme.text,
                Face::Bold,
            );
            let sys_sub = format!(
                "内核: {}   ·   运行时间: {}   ·   内存: {}%",
                self.kernel_summary, self.uptime_str, self.mem_pct
            );
            let sys_shown = text.truncate(&sys_sub, (sys_w - 36).max(100) as f32, 11.5, Face::Ui);
            text.draw(
                canvas,
                &sys_shown,
                (sys_rect.x + 18) as f32,
                (sys_rect.y + 60) as f32,
                11.5,
                theme.text_dim,
                Face::Ui,
            );

            y += card_h + 22;

            // Win8 / 8.1 Dynamic Tiles + Pinned Apps Section.
            let specs = parse_start_tiles(&cfg.panel_start_tiles);
            if specs.is_empty() {
                let empty_r = Rect::new(content_x, y, content_w, 120);
                draw_xj380_card(canvas, theme, empty_r, theme.accent, false, 0.88);
                text.draw_centered(
                    canvas,
                    "当前没有固定的开始屏幕磁贴，请前往「应用陈列柜」右键固定应用",
                    empty_r.x as f32 + empty_r.w as f32 * 0.5,
                    (empty_r.y + 48) as f32,
                    13.0,
                    theme.text_dim,
                    Face::Ui,
                );
                let rst_r = Rect::new(empty_r.x + empty_r.w / 2 - 74, empty_r.y + 68, 148, 32);
                ui::button(
                    canvas,
                    text,
                    theme,
                    rst_r,
                    "恢复默认磁贴布局",
                    None,
                    true,
                    self.hover == Some(StartHit::ResetTiles),
                );
                if let Some(clipped) = non_empty_clip(rst_r, viewport) {
                    self.push_hit(clipped, StartHit::ResetTiles);
                }
                y = empty_r.bottom() + 18;
            } else {
                let indexed: Vec<(usize, &StartTileSpec)> = specs.iter().enumerate().collect();
                let status_group: Vec<(usize, &StartTileSpec)> = indexed
                    .iter()
                    .copied()
                    .filter(|(_, t)| t.id.starts_with("status-"))
                    .collect();
                let app_group: Vec<(usize, &StartTileSpec)> = indexed
                    .iter()
                    .copied()
                    .filter(|(_, t)| !t.id.starts_with("status-"))
                    .collect();

                let unit = if content_w >= 1120 {
                    68
                } else if content_w >= 880 {
                    62
                } else {
                    54
                };
                let gap = 10;

                if content_w >= 1060 && !status_group.is_empty() && !app_group.is_empty() {
                    // Side-by-side two Metro tile groups matching Win8/8.1 Start Screen.
                    let g1_cols = 8;
                    let g1_w = g1_cols * unit + (g1_cols - 1) * gap;
                    let col_gap = 36;
                    let rem_w = (content_w - g1_w - col_gap).max(unit * 4 + gap * 3);
                    let g2_cols = ((rem_w + gap) / (unit + gap)).max(4);
                    let g2_x = content_x + g1_w + col_gap;

                    text.draw(
                        canvas,
                        "动态磁贴与实时状态（左键设置 / 右键调整大小）",
                        content_x as f32,
                        (y + 14) as f32,
                        12.0,
                        theme.accent,
                        Face::Bold,
                    );
                    text.draw(
                        canvas,
                        "固定应用与快捷磁贴（右键可缩放或取消固定）",
                        g2_x as f32,
                        (y + 14) as f32,
                        12.0,
                        theme.text_dim,
                        Face::Bold,
                    );
                    let grid_y = y + 26;
                    let (placed1, h1) =
                        pack_tile_group(&status_group, g1_cols, content_x, grid_y, unit, gap);
                    let (placed2, h2) =
                        pack_tile_group(&app_group, g2_cols, g2_x, grid_y, unit, gap);
                    for (orig_idx, tile) in placed1.into_iter().chain(placed2.into_iter()) {
                        self.draw_single_tile(
                            canvas, text, theme, cfg, net_status, viewport, orig_idx, &tile,
                        );
                    }
                    y = grid_y + h1.max(h2) + 20;
                } else {
                    let cols = ((content_w + gap) / (unit + gap)).max(4);
                    if !status_group.is_empty() {
                        text.draw(
                            canvas,
                            "动态磁贴与实时状态（右键调整大小或取消固定）",
                            content_x as f32,
                            (y + 14) as f32,
                            12.0,
                            theme.accent,
                            Face::Bold,
                        );
                        let (placed1, h1) =
                            pack_tile_group(&status_group, cols, content_x, y + 24, unit, gap);
                        for (orig_idx, tile) in placed1 {
                            self.draw_single_tile(
                                canvas, text, theme, cfg, net_status, viewport, orig_idx, &tile,
                            );
                        }
                        y += 24 + h1 + 20;
                    }
                    if !app_group.is_empty() {
                        text.draw(
                            canvas,
                            "固定应用与快捷磁贴",
                            content_x as f32,
                            (y + 14) as f32,
                            12.0,
                            theme.text_dim,
                            Face::Bold,
                        );
                        let (placed2, h2) =
                            pack_tile_group(&app_group, cols, content_x, y + 24, unit, gap);
                        for (orig_idx, tile) in placed2 {
                            self.draw_single_tile(
                                canvas, text, theme, cfg, net_status, viewport, orig_idx, &tile,
                            );
                        }
                        y += 24 + h2 + 20;
                    }
                }
            }
            end_y = y;
        });

        let total_h = (end_y - start_y + 12).max(viewport.h);
        self.home_scroll.draw_v(
            canvas,
            theme,
            Rect::new(
                body.right() - ui::SCROLLBAR_HIT_W - 2,
                viewport.y + 6,
                ui::SCROLLBAR_HIT_W,
                (viewport.h - 12).max(8),
            ),
            viewport.h as f32,
            total_h as f32,
        );

        // XJ380 Bottom Status & Return-to-Desktop Footer.
        let footer = Rect::new(0, body.bottom() - footer_h, body.w, footer_h);
        let foot_bg = if theme.mode == ThemeMode::Dark {
            0x0a1019
        } else {
            0xeaf0f6
        };
        canvas.fill_alpha(footer, foot_bg, 0.90);
        canvas.fill(Rect::new(0, footer.y, body.w, 1), theme.panel_border);
        let status_summary = format!(
            "运行窗口: {}  ·  CPU: {}%  ·  内存: {}/{} MiB  ·  右键磁贴自定义布局  ·  按 Esc 返回",
            open_windows, self.cpu_pct, self.mem_used_mb, self.mem_total_mb
        );
        let st_shown = text.truncate(
            &status_summary,
            (footer.w - 168).max(120) as f32,
            11.5,
            Face::Ui,
        );
        text.draw(
            canvas,
            &st_shown,
            content_x as f32,
            (footer.y + 28) as f32,
            11.5,
            theme.text_dim,
            Face::Ui,
        );
        let back_r = Rect::new(footer.right() - 126, footer.y + 8, 106, 30);
        ui::button(
            canvas,
            text,
            theme,
            back_r,
            "→ 返回桌面",
            None,
            true,
            self.hover == Some(StartHit::CloseDesktop),
        );
        self.push_hit(back_r, StartHit::CloseDesktop);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_home_search_results(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        x: i32,
        w: i32,
        mut y: i32,
    ) -> i32 {
        let app_matches = filtered_apps_for(&self.query, None, &[]);
        let setting_matches = search_settings(&self.query);
        let power_matches = matching_power_actions(&self.query);

        let rows = app_matches.len().max(setting_matches.len()).clamp(1, 5) as i32;
        let power_strip_h = if power_matches.is_empty() { 0 } else { 42 };
        let box_h = 44 + rows * 38 + power_strip_h;
        let panel_r = Rect::new(x, y, w, box_h);
        draw_xj380_card(canvas, theme, panel_r, theme.accent, false, 0.94);

        let col_w = (w - 40) / 2;
        let left_x = x + 16;
        let right_x = x + 24 + col_w;
        text.draw(
            canvas,
            "匹配应用程序（Enter 启动首个结果）",
            left_x as f32,
            (y + 26) as f32,
            12.0,
            theme.accent,
            Face::Bold,
        );
        text.draw(
            canvas,
            "匹配系统设置项（点击直达设置分类）",
            right_x as f32,
            (y + 26) as f32,
            12.0,
            theme.accent,
            Face::Bold,
        );

        if app_matches.is_empty() {
            text.draw(
                canvas,
                "未找到匹配应用",
                left_x as f32,
                (y + 58) as f32,
                11.5,
                theme.text_muted,
                Face::Ui,
            );
        } else {
            for (slot, &app_idx) in app_matches.iter().take(5).enumerate() {
                let ry = y + 38 + slot as i32 * 38;
                let r = Rect::new(left_x, ry, col_w, 34);
                let target = StartHit::SearchApp(app_idx);
                if self.hover == Some(target) || slot == 0 {
                    canvas.rounded_rect(r, RADIUS_CONTROL, theme.accent_soft, 1.0);
                }
                let app = &APPS[app_idx];
                icons::draw(canvas, app.icon, r.x + 8, r.y + 6, 22, None);
                let label = format!("{}  ·  {}", app.name, app.subtitle);
                let shown = text.truncate(&label, (col_w - 44).max(60) as f32, 12.0, Face::Ui);
                text.draw(
                    canvas,
                    &shown,
                    (r.x + 36) as f32,
                    (r.y + 22) as f32,
                    12.0,
                    theme.text,
                    Face::Ui,
                );
                if let Some(clipped) = non_empty_clip(r, viewport) {
                    self.push_hit(clipped, target);
                }
            }
        }

        if setting_matches.is_empty() {
            text.draw(
                canvas,
                "未找到匹配设置项",
                right_x as f32,
                (y + 58) as f32,
                11.5,
                theme.text_muted,
                Face::Ui,
            );
        } else {
            for (slot, item) in setting_matches.iter().take(5).enumerate() {
                let ry = y + 38 + slot as i32 * 38;
                let r = Rect::new(right_x, ry, col_w, 34);
                let target = StartHit::SearchSetting(slot);
                if self.hover == Some(target) {
                    canvas.rounded_rect(r, RADIUS_CONTROL, theme.panel_hover, 1.0);
                }
                let badge = Rect::new(r.x + 6, r.y + 7, 72, 20);
                canvas.rounded_rect(badge, 3, theme.accent_soft, 1.0);
                text.draw_centered(
                    canvas,
                    item.category,
                    badge.x as f32 + 36.0,
                    (badge.y + 14) as f32,
                    10.0,
                    theme.accent,
                    Face::Bold,
                );
                let label = format!("{} · {}", item.title, item.subtitle);
                let shown = text.truncate(&label, (col_w - 92).max(60) as f32, 11.5, Face::Ui);
                text.draw(
                    canvas,
                    &shown,
                    (badge.right() + 10) as f32,
                    (r.y + 22) as f32,
                    11.5,
                    theme.text,
                    Face::Ui,
                );
                if let Some(clipped) = non_empty_clip(r, viewport) {
                    self.push_hit(clipped, target);
                }
            }
        }

        if !power_matches.is_empty() {
            let py = y + 40 + rows * 38;
            let mut px = left_x;
            for &action in &power_matches {
                let (label, icon) = match action {
                    PowerAction::Reboot => ("重新启动计算机", Icon::Refresh),
                    PowerAction::PowerOff => ("关闭计算机", Icon::Shutdown),
                };
                let pill = Rect::new(px, py, 168, 30);
                let target = StartHit::SearchPower(action);
                ui::button(
                    canvas,
                    text,
                    theme,
                    pill,
                    label,
                    Some(icon),
                    false,
                    self.hover == Some(target),
                );
                if let Some(clipped) = non_empty_clip(pill, viewport) {
                    self.push_hit(clipped, target);
                }
                px += 180;
            }
        }

        y += box_h + 18;
        y
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_single_tile(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        net_status: &NetStatus,
        viewport: Rect,
        orig_idx: usize,
        tile: &PlacedTile,
    ) {
        let r = tile.rect;
        if r.bottom() < viewport.y || r.y > viewport.bottom() {
            return;
        }
        let target = StartHit::Tile(orig_idx);
        let hovered = self.hover == Some(target);
        let accent = tile_accent_color(&tile.id, theme.accent);
        draw_xj380_card(canvas, theme, r, accent, hovered, 0.90);

        let app_mode = tile_app_mode(&tile.id);
        match tile.id.as_str() {
            "status-clock" => self.draw_clock_tile(canvas, text, theme, cfg, tile),
            "status-net" => self.draw_network_tile(canvas, text, theme, net_status, tile),
            "status-audio" => {
                self.draw_audio_tile(canvas, text, theme, cfg, viewport, tile, accent);
            }
            "status-sys" => self.draw_sys_tile(canvas, text, theme, tile, accent),
            _ => self.draw_app_tile_face(canvas, text, theme, tile),
        }

        self.push_tile_hit(r, viewport, target, tile.id.clone(), app_mode);
    }

    fn draw_clock_tile(
        &self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        tile: &PlacedTile,
    ) {
        let r = tile.rect;
        let (year, mon, day, hour, min, sec, wday) = now_local();
        let wday_s = match wday {
            0 => "星期日",
            1 => "星期一",
            2 => "星期二",
            3 => "星期三",
            4 => "星期四",
            5 => "星期五",
            _ => "星期六",
        };
        let hm = if cfg.clock_24h {
            format!("{hour:02}:{min:02}")
        } else {
            let h12 = match hour % 12 {
                0 => 12,
                v => v,
            };
            format!("{h12:02}:{min:02}")
        };
        match tile.size {
            StartTileSize::Small => {
                text.draw_centered(
                    canvas,
                    &hm,
                    r.x as f32 + r.w as f32 * 0.5,
                    (r.y + r.h / 2 + 6) as f32,
                    14.0,
                    theme.text,
                    Face::Bold,
                );
            }
            StartTileSize::Medium => {
                icons::draw(canvas, Icon::DateTime, r.x + 12, r.y + 14, 24, None);
                text.draw(
                    canvas,
                    &hm,
                    (r.x + 12) as f32,
                    (r.y + 68) as f32,
                    22.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    &format!("{mon:02}/{day:02} {wday_s}"),
                    (r.x + 12) as f32,
                    (r.bottom() - 14) as f32,
                    11.0,
                    theme.text_dim,
                    Face::Ui,
                );
            }
            StartTileSize::Wide | StartTileSize::Large => {
                let full_t = if cfg.clock_seconds {
                    format!("{hm}:{sec:02}")
                } else {
                    hm
                };
                icons::draw(canvas, Icon::DateTime, r.x + 16, r.y + 18, 28, None);
                text.draw(
                    canvas,
                    &full_t,
                    (r.x + 54) as f32,
                    (r.y + 52) as f32,
                    if tile.size == StartTileSize::Large {
                        34.0
                    } else {
                        26.0
                    },
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    &format!("{year}年{mon:02}月{day:02}日 · {wday_s}"),
                    (r.x + 16) as f32,
                    (r.y + 84) as f32,
                    12.5,
                    theme.text_dim,
                    Face::Ui,
                );
                text.draw(
                    canvas,
                    &format!("时区: {}", cfg.timezone),
                    (r.x + 16) as f32,
                    (r.bottom() - 14) as f32,
                    11.0,
                    theme.accent,
                    Face::Ui,
                );
            }
        }
    }

    fn draw_network_tile(
        &self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        net: &NetStatus,
        tile: &PlacedTile,
    ) {
        let r = tile.rect;
        let conn_col = if net.connected() {
            theme.success
        } else {
            theme.warning
        };
        let status_label = if net.connected() {
            "网络已连接"
        } else {
            "网络未连接"
        };
        match tile.size {
            StartTileSize::Small => {
                icons::draw(
                    canvas,
                    Icon::Network,
                    r.x + (r.w - 26) / 2,
                    r.y + (r.h - 26) / 2,
                    26,
                    None,
                );
                canvas.circle(
                    (r.right() - 10) as f32,
                    (r.y + 12) as f32,
                    4.0,
                    conn_col,
                    1.0,
                );
            }
            StartTileSize::Medium => {
                icons::draw(canvas, Icon::Network, r.x + 12, r.y + 14, 28, None);
                let ip = net
                    .ipv4
                    .map(|(a, _)| a.to_string())
                    .unwrap_or_else(|| "离线".to_string());
                let shown = text.truncate(&ip, (r.w - 24).max(40) as f32, 12.0, Face::Bold);
                text.draw(
                    canvas,
                    &shown,
                    (r.x + 12) as f32,
                    (r.bottom() - 32) as f32,
                    12.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    status_label,
                    (r.x + 12) as f32,
                    (r.bottom() - 13) as f32,
                    10.5,
                    conn_col,
                    Face::Ui,
                );
            }
            StartTileSize::Wide | StartTileSize::Large => {
                icons::draw(canvas, Icon::Network, r.x + 16, r.y + 18, 28, None);
                text.draw(
                    canvas,
                    status_label,
                    (r.x + 54) as f32,
                    (r.y + 38) as f32,
                    14.5,
                    theme.text,
                    Face::Bold,
                );
                canvas.circle(
                    (r.right() - 18) as f32,
                    (r.y + 24) as f32,
                    5.0,
                    conn_col,
                    1.0,
                );
                let ip_line = net
                    .ipv4
                    .map(|(a, p)| format!("IPv4: {a}/{p}  ({iface})", iface = net.ifname))
                    .unwrap_or_else(|| format!("接口: {} (等待租约)", net.ifname));
                let shown_ip = text.truncate(&ip_line, (r.w - 32).max(80) as f32, 12.0, Face::Ui);
                text.draw(
                    canvas,
                    &shown_ip,
                    (r.x + 16) as f32,
                    (r.y + 68) as f32,
                    12.0,
                    theme.text_dim,
                    Face::Ui,
                );
                let traffic = format!("↓ {} KiB/s   ↑ {} KiB/s", self.rx_kbps, self.tx_kbps);
                text.draw(
                    canvas,
                    &traffic,
                    (r.x + 16) as f32,
                    (r.bottom() - 14) as f32,
                    11.0,
                    conn_col,
                    Face::Ui,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_audio_tile(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        viewport: Rect,
        tile: &PlacedTile,
        accent: u32,
    ) {
        let r = tile.rect;
        let vol = cfg.sound_master_volume.clamp(0, 100);
        let icon = if cfg.sound_muted || vol == 0 {
            Icon::AudioVolumeMuted
        } else {
            Icon::Sound
        };
        match tile.size {
            StartTileSize::Small => {
                icons::draw(
                    canvas,
                    icon,
                    r.x + (r.w - 26) / 2,
                    r.y + (r.h - 26) / 2,
                    26,
                    None,
                );
            }
            StartTileSize::Medium => {
                icons::draw(canvas, icon, r.x + 12, r.y + 14, 28, None);
                let v_str = if cfg.sound_muted {
                    String::from("已静音")
                } else {
                    format!("{vol}%")
                };
                text.draw(
                    canvas,
                    &v_str,
                    (r.x + 12) as f32,
                    (r.y + 74) as f32,
                    20.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    "声音与音乐",
                    (r.x + 12) as f32,
                    (r.bottom() - 13) as f32,
                    11.0,
                    theme.text_dim,
                    Face::Ui,
                );
            }
            StartTileSize::Wide | StartTileSize::Large => {
                icons::draw(canvas, icon, r.x + 16, r.y + 16, 28, None);
                let label = if cfg.sound_muted {
                    format!("音量已静音 ({vol}%)")
                } else {
                    format!("系统音量  {vol}%")
                };
                text.draw(
                    canvas,
                    &label,
                    (r.x + 54) as f32,
                    (r.y + 36) as f32,
                    14.5,
                    theme.text,
                    Face::Bold,
                );
                // Interactive quick-mute pill on the top-right of the tile.
                let mute_r = Rect::new(r.right() - 62, r.y + 14, 48, 22);
                let hov_m = self.hover == Some(StartHit::AudioTileMute);
                canvas.rounded_rect(
                    mute_r,
                    4,
                    if hov_m {
                        theme.accent
                    } else {
                        theme.accent_soft
                    },
                    1.0,
                );
                text.draw_centered(
                    canvas,
                    if cfg.sound_muted {
                        "取消静音"
                    } else {
                        "静音"
                    },
                    mute_r.x as f32 + 24.0,
                    (mute_r.y + 15) as f32,
                    9.5,
                    if hov_m { 0xffffff } else { theme.text },
                    Face::Bold,
                );
                if let Some(clipped) = non_empty_clip(mute_r, viewport) {
                    self.push_hit(clipped, StartHit::AudioTileMute);
                }

                // Visual volume level meter bar.
                let bar_w = (r.w - 32).max(40);
                let bar_r = Rect::new(r.x + 16, r.y + 56, bar_w, 8);
                canvas.rounded_rect(bar_r, 4, theme.panel_border, 0.7);
                let fill_w = ((bar_w * if cfg.sound_muted { 0 } else { vol }) / 100).max(0);
                if fill_w > 0 {
                    canvas.rounded_rect(Rect::new(bar_r.x, bar_r.y, fill_w, 8), 4, accent, 1.0);
                }
                let music_txt = format!("曲库默认: {}", self.music_track);
                let shown = text.truncate(&music_txt, (r.w - 32).max(80) as f32, 11.5, Face::Ui);
                text.draw(
                    canvas,
                    &shown,
                    (r.x + 16) as f32,
                    (r.bottom() - 14) as f32,
                    11.5,
                    theme.text_dim,
                    Face::Ui,
                );
            }
        }
    }

    fn draw_sys_tile(
        &self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        tile: &PlacedTile,
        accent: u32,
    ) {
        let r = tile.rect;
        match tile.size {
            StartTileSize::Small => {
                icons::draw(
                    canvas,
                    Icon::Monitor,
                    r.x + (r.w - 26) / 2,
                    r.y + (r.h - 26) / 2,
                    26,
                    None,
                );
            }
            StartTileSize::Medium => {
                icons::draw(canvas, Icon::Monitor, r.x + 12, r.y + 14, 28, None);
                text.draw(
                    canvas,
                    &format!("CPU {}%", self.cpu_pct),
                    (r.x + 12) as f32,
                    (r.y + 72) as f32,
                    16.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    &format!("内存 {}%", self.mem_pct),
                    (r.x + 12) as f32,
                    (r.bottom() - 14) as f32,
                    11.5,
                    accent,
                    Face::Ui,
                );
            }
            StartTileSize::Wide | StartTileSize::Large => {
                icons::draw(canvas, Icon::Monitor, r.x + 16, r.y + 16, 28, None);
                text.draw(
                    canvas,
                    "实时系统性能监视",
                    (r.x + 54) as f32,
                    (r.y + 36) as f32,
                    14.5,
                    theme.text,
                    Face::Bold,
                );
                let bar_w = ((r.w - 48) / 2).max(40);
                let left_bar = Rect::new(r.x + 16, r.y + 70, bar_w, 8);
                let right_bar = Rect::new(left_bar.right() + 16, r.y + 70, bar_w, 8);

                text.draw(
                    canvas,
                    &format!("CPU {}%", self.cpu_pct),
                    left_bar.x as f32,
                    (r.y + 62) as f32,
                    11.5,
                    theme.text_dim,
                    Face::Bold,
                );
                canvas.rounded_rect(left_bar, 4, theme.panel_border, 0.7);
                let cpu_fill = (bar_w * i32::from(self.cpu_pct)) / 100;
                if cpu_fill > 0 {
                    canvas.rounded_rect(
                        Rect::new(left_bar.x, left_bar.y, cpu_fill, 8),
                        4,
                        accent,
                        1.0,
                    );
                }

                text.draw(
                    canvas,
                    &format!("内存 {}%", self.mem_pct),
                    right_bar.x as f32,
                    (r.y + 62) as f32,
                    11.5,
                    theme.text_dim,
                    Face::Bold,
                );
                canvas.rounded_rect(right_bar, 4, theme.panel_border, 0.7);
                let mem_fill = (bar_w * i32::from(self.mem_pct)) / 100;
                if mem_fill > 0 {
                    canvas.rounded_rect(
                        Rect::new(right_bar.x, right_bar.y, mem_fill, 8),
                        4,
                        theme.accent,
                        1.0,
                    );
                }

                text.draw(
                    canvas,
                    &format!(
                        "已用 {} / {} MiB  ·  点击打开系统监视器",
                        self.mem_used_mb, self.mem_total_mb
                    ),
                    (r.x + 16) as f32,
                    (r.bottom() - 14) as f32,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
                );
            }
        }
    }

    fn draw_app_tile_face(
        &self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        tile: &PlacedTile,
    ) {
        let r = tile.rect;
        let Some(mode) = tile_app_mode(&tile.id) else {
            return;
        };
        let Some(entry) = APPS.iter().find(|a| a.mode == mode) else {
            return;
        };
        match tile.size {
            StartTileSize::Small => {
                let sz = (r.w - 20).clamp(24, 36);
                icons::draw(
                    canvas,
                    entry.icon,
                    r.x + (r.w - sz) / 2,
                    r.y + (r.h - sz) / 2 + 1,
                    sz as u32,
                    None,
                );
            }
            StartTileSize::Medium => {
                let sz = if r.w >= 130 { 44 } else { 36 };
                icons::draw(
                    canvas,
                    entry.icon,
                    r.x + (r.w - sz) / 2,
                    r.y + (r.h - sz - 22) / 2 + 2,
                    sz as u32,
                    None,
                );
                let title = text.truncate(entry.name, (r.w - 18).max(40) as f32, 12.0, Face::Bold);
                text.draw(
                    canvas,
                    &title,
                    (r.x + 12) as f32,
                    (r.bottom() - 12) as f32,
                    12.0,
                    theme.text,
                    Face::Bold,
                );
            }
            StartTileSize::Wide | StartTileSize::Large => {
                let sz = if tile.size == StartTileSize::Large {
                    56
                } else {
                    44
                };
                icons::draw(
                    canvas,
                    entry.icon,
                    r.x + 18,
                    r.y + (r.h - sz) / 2,
                    sz as u32,
                    None,
                );
                let tx = r.x + sz + 30;
                let max_w = (r.right() - tx - 14).max(60) as f32;
                let title = text.truncate(entry.name, max_w, 15.0, Face::Bold);
                let sub = text.truncate(entry.subtitle, max_w, 11.5, Face::Ui);
                text.draw(
                    canvas,
                    &title,
                    tx as f32,
                    (r.y + r.h / 2 - 4) as f32,
                    15.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    &sub,
                    tx as f32,
                    (r.y + r.h / 2 + 16) as f32,
                    11.5,
                    theme.text_dim,
                    Face::Ui,
                );
                if entry.mode == "settings" {
                    text.draw(
                        canvas,
                        "点击直达控制中心设置面板",
                        tx as f32,
                        (r.bottom() - 14) as f32,
                        10.5,
                        theme.accent,
                        Face::Ui,
                    );
                }
            }
        }
    }

    fn draw_apps(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        body: Rect,
    ) {
        let rail_w = if body.w >= 960 { 224 } else { 180 };
        let nav_bg = if theme.mode == ThemeMode::Dark {
            0x0d131d
        } else {
            0xe7ecf3
        };
        let pane_bg = if theme.mode == ThemeMode::Dark {
            0x101620
        } else {
            0xf3f5f8
        };
        canvas.fill_alpha(body, pane_bg, 0.84);
        canvas.fill_alpha(Rect::new(body.x, body.y, rail_w, body.h), nav_bg, 0.94);
        canvas.fill(
            Rect::new(body.x + rail_w - 1, body.y, 1, body.h),
            theme.panel_border,
        );

        text.draw(
            canvas,
            "应用分类 / CABINET",
            (body.x + 18) as f32,
            (body.y + 26) as f32,
            11.0,
            theme.accent,
            Face::Bold,
        );

        let pinned = parse_start_tiles(&cfg.panel_start_tiles);
        for (i, (filter, label, icon)) in AppCabinetFilter::ALL.iter().enumerate() {
            let r = Rect::new(body.x + 10, body.y + 40 + i as i32 * 40, rail_w - 20, 34);
            let active = self.app_filter == *filter;
            let target = StartHit::AppFilter(*filter);
            if active {
                canvas.rounded_rect(r, RADIUS_CONTROL, theme.accent_soft, 1.0);
                canvas.fill(Rect::new(r.x, r.y + 7, 3, r.h - 14), theme.accent);
            } else if self.hover == Some(target) {
                canvas.rounded_rect(r, RADIUS_CONTROL, theme.panel_hover, 1.0);
            }
            icons::draw(canvas, *icon, r.x + 10, r.y + (r.h - 18) / 2, 18, None);
            text.draw(
                canvas,
                label,
                (r.x + 36) as f32,
                (r.y + 22) as f32,
                12.5,
                if active { theme.text } else { theme.text_dim },
                if active { Face::Bold } else { Face::Ui },
            );
            self.push_hit(r, target);
        }

        let pane_x = body.x + rail_w;
        let pane_w = (body.w - rail_w).max(280);
        let pad = 28;
        let grid_w = (pane_w - pad * 2).min(1060);
        let grid_x = pane_x + ((pane_w - grid_w) / 2).max(pad);
        let viewport = Rect::new(pane_x, body.y, pane_w - 1, body.h);

        let filtered = filtered_apps_for(&self.query, Some(self.app_filter), &pinned);
        let scroll_y = self.apps_scroll.offset_i32().max(0);
        let start_y = viewport.y + 20 - scroll_y;
        let mut end_y = start_y;

        canvas.with_clip(viewport, |canvas| {
            let mut y = start_y;
            let heading = format!(
                "应用程序 ({}) · 左键启动，右键可固定到开始屏幕或调整磁贴大小",
                filtered.len()
            );
            text.draw(
                canvas,
                &heading,
                grid_x as f32,
                (y + 16) as f32,
                12.5,
                theme.text_dim,
                Face::Bold,
            );
            y += 32;

            if filtered.is_empty() {
                let card = Rect::new(grid_x, y, grid_w, 96);
                draw_xj380_card(canvas, theme, card, theme.accent, false, 0.90);
                text.draw_centered(
                    canvas,
                    "当前筛选或搜索词没有匹配的应用程序",
                    card.x as f32 + card.w as f32 * 0.5,
                    (card.y + 52) as f32,
                    13.0,
                    theme.text_muted,
                    Face::Ui,
                );
                y += 116;
            } else {
                let cols = ((grid_w + 14) / 270).clamp(1, 3);
                let gap = 14;
                let card_w = (grid_w - (cols - 1) * gap) / cols;
                let card_h = 88;
                for (slot, &app_idx) in filtered.iter().enumerate() {
                    let row = slot as i32 / cols;
                    let col = slot as i32 % cols;
                    let card_r = Rect::new(
                        grid_x + col * (card_w + gap),
                        y + row * (card_h + gap),
                        card_w,
                        card_h,
                    );
                    let target = StartHit::AppCard(app_idx);
                    let app = &APPS[app_idx];
                    let tid = app_tile_id(app.mode);
                    let is_pinned = pinned.iter().any(|t| t.id == tid);
                    let accent = tile_accent_color(&tid, theme.accent);

                    draw_xj380_card(
                        canvas,
                        theme,
                        card_r,
                        accent,
                        self.hover == Some(target),
                        0.92,
                    );
                    icons::draw(canvas, app.icon, card_r.x + 16, card_r.y + 22, 42, None);

                    let name_x = card_r.x + 70;
                    let title_max =
                        (card_r.right() - name_x - if is_pinned { 64 } else { 14 }).max(40);
                    let shown_name = text.truncate(app.name, title_max as f32, 14.5, Face::Bold);
                    text.draw(
                        canvas,
                        &shown_name,
                        name_x as f32,
                        (card_r.y + 38) as f32,
                        14.5,
                        theme.text,
                        Face::Bold,
                    );
                    let shown_sub = text.truncate(
                        app.subtitle,
                        (card_r.right() - name_x - 14).max(40) as f32,
                        11.5,
                        Face::Ui,
                    );
                    text.draw(
                        canvas,
                        &shown_sub,
                        name_x as f32,
                        (card_r.y + 60) as f32,
                        11.5,
                        theme.text_dim,
                        Face::Ui,
                    );
                    if is_pinned {
                        let pin_r = Rect::new(card_r.right() - 56, card_r.y + 14, 44, 18);
                        canvas.rounded_rect(pin_r, 3, theme.accent_soft, 1.0);
                        text.draw_centered(
                            canvas,
                            "已固定",
                            pin_r.x as f32 + 22.0,
                            (pin_r.y + 13) as f32,
                            9.5,
                            theme.accent,
                            Face::Bold,
                        );
                    }
                    self.push_tile_hit(card_r, viewport, target, tid, Some(app.mode));
                }
                let rows = (filtered.len() as i32 + cols - 1) / cols;
                y += rows * (card_h + gap) + 12;
            }

            if !self.query.trim().is_empty() {
                let s_matches = search_settings(&self.query);
                if !s_matches.is_empty() {
                    text.draw(
                        canvas,
                        "相关系统设置（点击直达设置中心）",
                        grid_x as f32,
                        (y + 18) as f32,
                        12.0,
                        theme.accent,
                        Face::Bold,
                    );
                    y += 28;
                    for (slot, item) in s_matches.iter().take(5).enumerate() {
                        let r = Rect::new(grid_x, y, grid_w, 38);
                        let target = StartHit::SearchSetting(slot);
                        draw_xj380_card(
                            canvas,
                            theme,
                            r,
                            theme.accent,
                            self.hover == Some(target),
                            0.90,
                        );
                        text.draw(
                            canvas,
                            &format!("[{}]  {}  ·  {}", item.category, item.title, item.subtitle),
                            (r.x + 14) as f32,
                            (r.y + 24) as f32,
                            12.0,
                            theme.text,
                            Face::Ui,
                        );
                        if let Some(clipped) = non_empty_clip(r, viewport) {
                            self.push_hit(clipped, target);
                        }
                        y += 44;
                    }
                }
            }
            end_y = y + 16;
        });

        let total_h = (end_y - start_y + 12).max(viewport.h);
        self.apps_scroll.draw_v(
            canvas,
            theme,
            Rect::new(
                body.right() - ui::SCROLLBAR_HIT_W - 2,
                viewport.y + 6,
                ui::SCROLLBAR_HIT_W,
                (viewport.h - 12).max(8),
            ),
            viewport.h as f32,
            total_h as f32,
        );
    }

    fn draw_ctx_menu(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        w: i32,
        h: i32,
    ) {
        let Some(ctx) = self.ctx.clone() else {
            return;
        };
        let specs = parse_start_tiles(&cfg.panel_start_tiles);
        let existing = specs.iter().find(|t| t.id == ctx.tile_id);
        let current_size = existing.map(|t| t.size);
        let is_pinned = existing.is_some();

        let rect = ctx.rect(w, h);
        ui::popup_surface(canvas, theme, rect, RADIUS_POPUP);
        canvas.fill(
            Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, 2),
            theme.accent,
        );

        let mut items: Vec<(String, StartHit, bool)> = vec![
            (
                String::from("设为小磁贴 (1×1)"),
                StartHit::CtxResize(StartTileSize::Small),
                current_size == Some(StartTileSize::Small),
            ),
            (
                String::from("设为中磁贴 (2×2)"),
                StartHit::CtxResize(StartTileSize::Medium),
                current_size == Some(StartTileSize::Medium),
            ),
            (
                String::from("设为宽磁贴 (4×2)"),
                StartHit::CtxResize(StartTileSize::Wide),
                current_size == Some(StartTileSize::Wide),
            ),
            (
                String::from("设为大磁贴 (4×4)"),
                StartHit::CtxResize(StartTileSize::Large),
                current_size == Some(StartTileSize::Large),
            ),
            (
                String::from(if is_pinned {
                    "从开始屏幕取消固定"
                } else {
                    "固定到开始屏幕主页"
                }),
                StartHit::CtxTogglePin,
                false,
            ),
            (
                String::from("恢复默认磁贴布局"),
                StartHit::CtxResetTiles,
                false,
            ),
        ];
        if ctx.app_mode == Some("settings") {
            items.insert(
                5,
                (
                    String::from("以独立窗口打开系统设置"),
                    StartHit::CtxOpenStandaloneSettings,
                    false,
                ),
            );
        }

        for (i, (label, target, active)) in items.into_iter().enumerate() {
            let row = Rect::new(rect.x + 6, rect.y + 6 + i as i32 * 32, rect.w - 12, 28);
            if self.hover == Some(target) {
                canvas.rounded_rect(row, RADIUS_CONTROL, theme.panel_hover, 1.0);
            } else if active {
                canvas.rounded_rect(row, RADIUS_CONTROL, theme.accent_soft, 1.0);
            }
            text.draw(
                canvas,
                &label,
                (row.x + 10) as f32,
                (row.y + 19) as f32,
                12.0,
                if active { theme.accent } else { theme.text },
                if active { Face::Bold } else { Face::Ui },
            );
            self.push_hit(row, target);
        }
    }

    fn draw_power_confirm(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        action: PowerAction,
        w: i32,
        h: i32,
    ) {
        self.hits.clear();
        canvas.fill_alpha(Rect::new(0, 0, w, h), 0x000000, 0.62);
        let card = confirm_power_card_rect(w, h);
        draw_xj380_card(canvas, theme, card, theme.accent, false, 0.98);
        canvas.rounded_border(card, RADIUS_POPUP, theme.accent, 0.85, 1);

        let (title, ok_label, ok_icon) = match action {
            PowerAction::Reboot => ("重新启动计算机？", "重启", Icon::Refresh),
            PowerAction::PowerOff => ("关闭计算机？", "关机", Icon::Shutdown),
        };
        text.draw(
            canvas,
            title,
            (card.x + 24) as f32,
            (card.y + 44) as f32,
            18.0,
            theme.text,
            Face::Bold,
        );
        text.draw(
            canvas,
            "将通过 systemg PID 1 依次停止服务、清理挂载并安全断电。",
            (card.x + 24) as f32,
            (card.y + 74) as f32,
            11.5,
            theme.text_muted,
            Face::Ui,
        );

        let cancel = Rect::new(card.right() - 188, card.bottom() - 48, 76, 32);
        let ok = Rect::new(card.right() - 100, card.bottom() - 48, 76, 32);
        ui::button(
            canvas,
            text,
            theme,
            cancel,
            "取消",
            None,
            false,
            self.hover == Some(StartHit::ConfirmPowerCancel),
        );
        ui::button(
            canvas,
            text,
            theme,
            ok,
            ok_label,
            Some(ok_icon),
            true,
            self.hover == Some(StartHit::ConfirmPowerOk),
        );
        self.push_hit(cancel, StartHit::ConfirmPowerCancel);
        self.push_hit(ok, StartHit::ConfirmPowerOk);
    }
}

fn confirm_power_card_rect(w: i32, h: i32) -> Rect {
    let cw = 408.min((w - 32).max(280));
    let ch = 160;
    Rect::new((w - cw) / 2, (h - ch) / 2, cw, ch)
}

fn matching_power_actions(query: &str) -> Vec<PowerAction> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if ["关机", "shutdown", "poweroff", "power off", "电源"]
        .iter()
        .any(|kw| kw.contains(&q) || q.contains(kw))
    {
        out.push(PowerAction::PowerOff);
    }
    if ["重启", "reboot", "restart", "电源"]
        .iter()
        .any(|kw| kw.contains(&q) || q.contains(kw))
    {
        out.push(PowerAction::Reboot);
    }
    out
}

fn non_empty_clip(rect: Rect, viewport: Rect) -> Option<Rect> {
    let clipped = rect.intersect(viewport);
    (!clipped.is_empty()).then_some(clipped)
}

fn app_tile_id(mode: &str) -> String {
    format!("app-{mode}")
}

fn tile_app_mode(tile_id: &str) -> Option<&'static str> {
    let mode = tile_id.strip_prefix("app-")?;
    APPS.iter().find(|a| a.mode == mode).map(|a| a.mode)
}

fn tile_accent_color(tile_id: &str, theme_accent: u32) -> u32 {
    match tile_id {
        "status-clock" | "app-settings" => theme_accent,
        "status-net" | "app-view" => 0x0ea5e9,
        "status-audio" => 0x8b5cf6,
        "status-sys" | "app-monitor" => 0x10b981,
        "app-files" => 0x3b82f6,
        "app-term" => 0x64748b,
        "app-edit" => 0x6366f1,
        "app-calc" => 0xf59e0b,
        "app-mines" => 0x16a34a,
        "app-music" => 0xe11d48,
        "app-help" => 0x14b8a6,
        _ => theme_accent,
    }
}

/// XJ380 signature card: translucent panel + 1px border + 2px top accent bar.
fn draw_xj380_card(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    accent: u32,
    hovered: bool,
    alpha: f32,
) {
    let base = if theme.mode == ThemeMode::Dark {
        mix(theme.panel, accent, if hovered { 0.22 } else { 0.12 })
    } else {
        mix(0xffffff, accent, if hovered { 0.14 } else { 0.06 })
    };
    canvas.rounded_rect(rect, RADIUS_CONTROL, base, alpha);
    canvas.rounded_border(
        rect,
        RADIUS_CONTROL,
        if hovered { accent } else { theme.panel_border },
        if hovered { 0.95 } else { 0.70 },
        1,
    );
    canvas.fill(
        Rect::new(rect.x + 2, rect.y + 1, (rect.w - 4).max(1), 2),
        accent,
    );
}

/// Pack a list of Win8/8.1 tiles cleanly on a uniform cell grid without overlaps.
pub fn pack_tile_group(
    specs: &[(usize, &StartTileSpec)],
    max_cols: i32,
    origin_x: i32,
    origin_y: i32,
    unit: i32,
    gap: i32,
) -> (Vec<(usize, PlacedTile)>, i32) {
    let cols = max_cols.max(4) as usize;
    let mut occ: Vec<Vec<bool>> = Vec::new();
    let mut placed = Vec::with_capacity(specs.len());
    let mut max_rows: i32 = 0;

    for &(orig_idx, spec) in specs {
        let (raw_w, raw_h) = spec.size.grid_span();
        let sw = (raw_w.max(1) as usize).min(cols);
        let sh = raw_h.max(1) as usize;

        let mut gy = 0usize;
        let (gx, gy) = 'search: loop {
            while occ.len() < gy + sh {
                occ.push(vec![false; cols]);
            }
            for gx in 0..=(cols - sw) {
                let mut free = true;
                'check: for dy in 0..sh {
                    for dx in 0..sw {
                        if occ[gy + dy][gx + dx] {
                            free = false;
                            break 'check;
                        }
                    }
                }
                if free {
                    break 'search (gx, gy);
                }
            }
            gy += 1;
        };
        for dy in 0..sh {
            for dx in 0..sw {
                occ[gy + dy][gx + dx] = true;
            }
        }
        let px = origin_x + gx as i32 * (unit + gap);
        let py = origin_y + gy as i32 * (unit + gap);
        let pw = sw as i32 * unit + (sw as i32 - 1) * gap;
        let ph = sh as i32 * unit + (sh as i32 - 1) * gap;
        max_rows = max_rows.max((gy + sh) as i32);
        placed.push((
            orig_idx,
            PlacedTile {
                id: spec.id.clone(),
                size: spec.size,
                rect: Rect::new(px, py, pw, ph),
            },
        ));
    }

    let total_h = if max_rows == 0 {
        0
    } else {
        max_rows * unit + (max_rows - 1) * gap
    };
    (placed, total_h)
}

pub fn filtered_apps_for(
    query: &str,
    filter: Option<AppCabinetFilter>,
    pinned: &[StartTileSpec],
) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    APPS.iter()
        .enumerate()
        .filter(|(_, app)| {
            if let Some(f) = filter {
                if !f.matches(app.mode, app.category, pinned) {
                    return false;
                }
            }
            if q.is_empty() {
                return true;
            }
            app.name.to_lowercase().contains(&q)
                || app.subtitle.to_lowercase().contains(&q)
                || app.keywords.to_lowercase().contains(&q)
                || app.mode.contains(&q)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Build the darkened/softened Start Screen background buffer matching wallpaper & theme.
pub fn build_start_backdrop(
    w: u16,
    h: u16,
    config: &DesktopConfig,
    wallpaper_scaled: &[u32],
) -> Vec<u32> {
    let len = usize::from(w) * usize::from(h);
    let scrim = if config.mode == ThemeMode::Dark {
        0x080d16
    } else {
        0xecf1f7
    };
    let scrim_alpha = if config.mode == ThemeMode::Dark {
        0.68
    } else {
        0.78
    };
    if wallpaper_scaled.len() == len {
        return wallpaper_scaled
            .iter()
            .map(|&px| mix(px, scrim, scrim_alpha))
            .collect();
    }
    let top = mix(config.solid_color, scrim, 0.45);
    let bot = mix(config.solid_color, scrim, 0.80);
    let mut out = Vec::with_capacity(len);
    let hf = h.max(1) as f32;
    for row in 0..usize::from(h) {
        let color = mix(top, bot, row as f32 / hf);
        out.extend(std::iter::repeat_n(color, usize::from(w)));
    }
    out
}

fn read_proc_stat_ticks() -> Option<CpuTicks> {
    let raw = fs::read_to_string("/proc/stat").ok()?;
    let line = raw.lines().find(|l| l.starts_with("cpu "))?;
    let mut vals = line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse::<u64>().ok());
    let user = vals.next()?;
    let nice = vals.next()?;
    let system = vals.next()?;
    let idle = vals.next()?;
    let iowait = vals.next().unwrap_or(0);
    let irq = vals.next().unwrap_or(0);
    let softirq = vals.next().unwrap_or(0);
    let steal = vals.next().unwrap_or(0);
    Some(CpuTicks {
        total: user + nice + system + idle + iowait + irq + softirq + steal,
        idle: idle + iowait,
    })
}

fn read_mem_info_mb() -> Option<(u64, u64)> {
    let raw = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = 0u64;
    let mut avail_kb = 0u64;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest.split_whitespace().next()?.parse().ok()?;
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = rest.split_whitespace().next()?.parse().ok()?;
        }
    }
    let total_mb = total_kb / 1024;
    let used_mb = total_kb.saturating_sub(avail_kb) / 1024;
    Some((used_mb, total_mb))
}

fn read_uptime_label() -> String {
    if let Ok(raw) = fs::read_to_string("/proc/uptime") {
        if let Some(sec) = raw
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<f64>().ok())
        {
            let mins = (sec as u64) / 60;
            let hrs = mins / 60;
            return if hrs > 0 {
                format!("{hrs}小时{}分", mins % 60)
            } else {
                format!("{mins}分")
            };
        }
    }
    String::from("--")
}

fn read_net_bytes() -> Option<(u64, u64)> {
    let raw = fs::read_to_string("/proc/net/dev").ok()?;
    let mut rx_sum = 0u64;
    let mut tx_sum = 0u64;
    for line in raw.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        if iface.trim() == "lo" {
            continue;
        }
        let cols: Vec<&str> = rest.split_whitespace().collect();
        if cols.len() >= 9 {
            rx_sum += cols[0].parse::<u64>().unwrap_or(0);
            tx_sum += cols[8].parse::<u64>().unwrap_or(0);
        }
    }
    Some((rx_sum, tx_sum))
}

fn detect_default_track_title() -> String {
    if fs::metadata("/usr/share/music/qingtian.mp3").is_ok() {
        return String::from("晴天 · MP3");
    }
    if let Ok(entries) = fs::read_dir("/usr/share/music") {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".mp3") {
                    return String::from(name.strip_suffix(".mp3").unwrap_or(name));
                }
            }
        }
    }
    String::from("晴天 · 示例曲库")
}

fn read_kernel_summary() -> String {
    // SAFETY: zero-initialized `utsname` filled cleanly by `libc::uname`.
    unsafe {
        let mut uts: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut uts) == 0 {
            let rel = std::ffi::CStr::from_ptr(uts.release.as_ptr()).to_string_lossy();
            let mach = std::ffi::CStr::from_ptr(uts.machine.as_ptr()).to_string_lossy();
            return format!("{rel} ({mach})");
        }
    }
    String::from("SpaceKernel (x86_64)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_has_no_overlaps_across_resolutions() {
        for (w, h) in [(800, 600), (1024, 768), (1280, 800), (1920, 1080)] {
            let hdr = HeaderLayout::compute(w, h);
            assert!(hdr.tabs[0].right() < hdr.tabs[1].x, "tab0/tab1 @ {w}x{h}");
            assert!(hdr.tabs[1].right() < hdr.tabs[2].x, "tab1/tab2 @ {w}x{h}");
            assert!(
                hdr.tabs[2].right() < hdr.search.x,
                "tabs/search overlap @ {w}x{h}: {} vs {}",
                hdr.tabs[2].right(),
                hdr.search.x
            );
            assert!(
                hdr.search.right() < hdr.clock.x,
                "search/clock overlap @ {w}x{h}: {} vs {}",
                hdr.search.right(),
                hdr.clock.x
            );
            assert!(hdr.clock.right() < hdr.logout.x, "clock/logout @ {w}x{h}");
            assert!(hdr.logout.right() < hdr.reboot.x, "logout/reboot @ {w}x{h}");
            assert!(
                hdr.reboot.right() < hdr.power_off.x,
                "reboot/power_off @ {w}x{h}"
            );
            assert!(
                hdr.power_off.right() < hdr.close_desktop.x,
                "power_off/close @ {w}x{h}"
            );
            assert!(
                hdr.close_desktop.right() <= w,
                "close exceeds width @ {w}x{h}"
            );
        }
    }

    #[test]
    fn search_matches_power_commands() {
        assert_eq!(matching_power_actions("关机"), vec![PowerAction::PowerOff]);
        assert_eq!(matching_power_actions("reboot"), vec![PowerAction::Reboot]);
        assert_eq!(
            matching_power_actions("电源"),
            vec![PowerAction::PowerOff, PowerAction::Reboot]
        );
    }

    #[test]
    fn pack_tile_group_produces_disjoint_grid_without_overflow() {
        let specs = parse_start_tiles(DEFAULT_START_TILES);
        let indexed: Vec<(usize, &StartTileSpec)> = specs.iter().enumerate().collect();
        for cols in [4, 6, 8, 12] {
            let unit = 64;
            let gap = 10;
            let ox = 40;
            let oy = 120;
            let max_w = cols * unit + (cols - 1) * gap;
            let (placed, total_h) = pack_tile_group(&indexed, cols, ox, oy, unit, gap);
            assert_eq!(placed.len(), specs.len());
            assert!(total_h > 0);
            for (i, (_, a)) in placed.iter().enumerate() {
                assert!(a.rect.x >= ox);
                assert!(
                    a.rect.right() <= ox + max_w,
                    "overflow cols={cols}: {:?}",
                    a.rect
                );
                for (_, b) in &placed[i + 1..] {
                    assert!(
                        a.rect.intersect(b.rect).is_empty(),
                        "tile overlap at cols={cols}: {:?} vs {:?}",
                        a.rect,
                        b.rect
                    );
                }
            }
        }
    }

    #[test]
    fn filtered_apps_matches_cabinet_categories_and_keywords() {
        let pinned = parse_start_tiles(DEFAULT_START_TILES);
        assert_eq!(
            filtered_apps_for("", Some(AppCabinetFilter::All), &pinned).len(),
            APPS.len()
        );
        let media = filtered_apps_for("", Some(AppCabinetFilter::Media), &pinned);
        assert_eq!(media.len(), 2);
        let term = filtered_apps_for("终端", None, &pinned);
        assert!(term.iter().any(|&i| APPS[i].mode == "term"));
    }
}
