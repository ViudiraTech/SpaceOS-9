/*
 *
 *       user/desktop/src/ime/daemon.rs
 *       `spimed`: SpaceOS-9 Chinese IME daemon multiplexing SPUI native UDS protocol,
 *       pure-Rust XIM X11 protocol server, live `uxconfd` settings, and candidate popup UI.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use crate::popup::{CandidatePopup, PopupClick, PopupLayout};
use crate::xim::{XimAction, XimServer};
use spime::decoder::{recompute, select_candidate, ComposeSession, EngineConfig};
use spime::dict::{SpimeDict, SpimeLm};
use spime::pinyin::FuzzyPinyin;
use spime::proto::{read_frame, socket_path, write_frame, ClientRequest, DaemonReply, PreeditInfo};
use spime::punct::PunctState;
use spime::shuangpin::ShuangpinScheme;
use spime::userdb::UserDb;
use spui_ui::text::TextRenderer;
use spui_ui::theme::Theme;
use spui_ui::x11::{Result, X};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use uxconf::client::{UxConfClient, UxConfWatch};
use uxconf::DesktopConfig;
use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt, EventMask, Window};
use x11rb::protocol::Event;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveTarget {
    None,
    Socket { client_id: u32, window_id: Window },
    Xim {
        server_comm: Window,
        im_id: u16,
        ic_id: u16,
    },
}

struct SocketClient {
    stream: UnixStream,
    buf: Vec<u8>,
    client_id: Option<u32>,
}

struct Daemon {
    x: X,
    text: TextRenderer,
    config: DesktopConfig,
    watch: Option<UxConfWatch>,
    theme: Theme,
    composited: bool,
    dict: SpimeDict,
    lm: SpimeLm,
    userdb: UserDb,
    engine_cfg: EngineConfig,
    layout: PopupLayout,
    sess: ComposeSession,
    punct: PunctState,
    chinese_mode: bool,
    left_shift_down: bool,
    left_shift_clean: bool,
    spot: (i32, i32, i32),
    active: ActiveTarget,
    popup: CandidatePopup,
    xim: XimServer,
    listener: UnixListener,
    sock_path: std::path::PathBuf,
    clients: Vec<SocketClient>,
    pending_notify_replies: HashMap<u32, VecDeque<DaemonReply>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.userdb.flush();
        self.x.set_spime_status(false, false);
        let _ = self.x.flush();
        let _ = fs::remove_file(&self.sock_path);
    }
}

impl Daemon {
    fn config_to_engine(cfg: &DesktopConfig) -> (EngineConfig, PopupLayout) {
        let fuzzy = if cfg.ime_fuzzy_pinyin {
            FuzzyPinyin {
                zh_z: true,
                ch_c: true,
                sh_s: true,
                l_n: true,
                f_h: false,
                r_l: false,
                an_ang: true,
                en_eng: true,
                in_ing: true,
                ian_iang: false,
                uan_uang: false,
            }
        } else {
            FuzzyPinyin::default()
        };
        let eng = EngineConfig {
            scheme: ShuangpinScheme::from_config_str(&cfg.ime_scheme),
            fuzzy,
            mixed_english: cfg.ime_mixed_english,
            cn_punct: cfg.ime_cn_punct,
            page_size: (cfg.ime_page_size as usize).clamp(5, 9),
        };
        let layout = PopupLayout::from_config(&cfg.ime_layout);
        (eng, layout)
    }

    fn poll_uxconf(&mut self) {
        if self.watch.is_none() {
            return;
        }
        let mut next_cfg = self.config.clone();
        let (changed, _) = self
            .watch
            .as_mut()
            .map_or((false, Vec::new()), |w| w.poll_into(&mut next_cfg));
        if changed {
            self.apply_config(next_cfg);
        }
    }

    fn sync_ime_enabled_from_uxconf(&mut self) {
        self.poll_uxconf();
        if !self.config.ime_enabled {
            if let Ok(mut client) = UxConfClient::connect() {
                let latest = client.load_desktop_config();
                if latest != self.config {
                    self.apply_config(latest);
                }
            }
        }
    }

    fn apply_config(&mut self, cfg: DesktopConfig) {
        let prev = self.config.clone();
        self.config = cfg;
        self.theme = Theme::from_config(&self.config);
        self.text.set_ui_scale(self.theme.ui_scale());
        let (eng, layout) = Self::config_to_engine(&self.config);
        self.engine_cfg = eng;
        self.layout = layout;
        if prev.ime_enabled != self.config.ime_enabled {
            if !self.config.ime_enabled {
                self.reset_compose();
            }
            self.publish_status();
        }
        if self.sess.is_active() {
            recompute(&self.dict, &self.lm, &self.userdb, &self.engine_cfg, &mut self.sess);
            self.sync_popup();
        }
    }

    fn publish_status(&self) {
        self.x
            .set_spime_status(self.chinese_mode, self.config.ime_enabled);
        let _ = self.x.flush();
    }

    fn reset_compose(&mut self) {
        self.sess.clear();
        self.popup.hide(&self.x);
    }

    fn scheme_badge(&self) -> &'static str {
        if !self.chinese_mode {
            "英"
        } else if self.engine_cfg.scheme == ShuangpinScheme::Full {
            "拼"
        } else {
            "双"
        }
    }

    fn sync_popup(&mut self) {
        if !self.chinese_mode || !self.config.ime_enabled || !self.sess.is_active() {
            self.popup.hide(&self.x);
            return;
        }
        let ps = self.engine_cfg.page_size;
        let total_pages = self.sess.page_count(ps);
        let cands = self.sess.current_page_slice(ps).to_vec();
        let preedit = self.sess.snapshot.preedit_display.clone();
        let (sx, sy, sh) = self.spot;
        let badge = self.scheme_badge();
        let _ = self.popup.show_and_render(
            &self.x,
            &mut self.text,
            &self.theme,
            self.composited,
            self.layout,
            badge,
            &preedit,
            &cands,
            self.sess.selected_in_page,
            self.sess.page,
            total_pages,
            sx,
            sy,
            sh,
        );
    }

    fn reply_snapshot(&self, consumed: bool, commit_text: String) -> DaemonReply {
        let preedit_text = if self.sess.is_active() {
            self.sess.snapshot.preedit_display.clone()
        } else {
            String::new()
        };
        let cursor_bytes = preedit_text.len().min(u16::MAX as usize) as u16;
        DaemonReply {
            consumed,
            chinese_mode: self.chinese_mode && self.config.ime_enabled,
            commit_text,
            preedit: PreeditInfo {
                text: preedit_text,
                cursor_bytes,
            },
        }
    }

    fn handle_key(
        &mut self,
        is_press: bool,
        keysym: u32,
        state: u16,
    ) -> DaemonReply {
        // Track Left-Shift clean tap (toggle CN/EN if enabled).
        let is_left_shift = keysym == 0xffe1;
        let allow_shift_toggle = self.config.ime_enabled && self.config.ime_toggle_key != "ctrl-space";

        if is_left_shift {
            if is_press {
                // Disarm if combined with Ctrl/Alt/Super.
                if (state & (4 | 8 | 64)) == 0 {
                    self.left_shift_down = true;
                    self.left_shift_clean = true;
                } else {
                    self.left_shift_clean = false;
                }
            } else if self.left_shift_down {
                let do_toggle = self.left_shift_clean && allow_shift_toggle;
                self.left_shift_down = false;
                self.left_shift_clean = false;
                if do_toggle {
                    return self.toggle_cn_mode_with_commit(true);
                }
            }
            return self.reply_snapshot(false, String::new());
        }

        if is_press && self.left_shift_down {
            self.left_shift_clean = false;
        }

        if !is_press || !self.config.ime_enabled {
            return self.reply_snapshot(false, String::new());
        }

        // Ctrl+Space always toggles Chinese/English mode.
        let ctrl = (state & 4) != 0;
        let alt_or_super = (state & (8 | 64)) != 0;
        if ctrl && !alt_or_super && keysym == 0x0020 {
            return self.toggle_cn_mode_with_commit(true);
        }

        if !self.chinese_mode {
            return self.reply_snapshot(false, String::new());
        }

        // Ignore pure modifier presses inside Chinese mode.
        if is_modifier_sym(keysym) {
            return self.reply_snapshot(false, String::new());
        }

        // Candidate delete shortcut while composing: Ctrl+1..=9 or Delete key.
        if self.sess.is_active() && !alt_or_super {
            if ctrl && (0x0031..=0x0039).contains(&keysym) {
                let idx_in_page = (keysym - 0x0031) as usize;
                self.delete_page_candidate(idx_in_page);
                return self.reply_snapshot(true, String::new());
            }
            if !ctrl && keysym == 0xffff {
                self.delete_page_candidate(self.sess.selected_in_page);
                return self.reply_snapshot(true, String::new());
            }
        }

        if ctrl || alt_or_super {
            return self.reply_snapshot(false, String::new());
        }

        if self.sess.is_active() {
            self.handle_composing_key(keysym, state)
        } else {
            self.handle_idle_chinese_key(keysym)
        }
    }

    fn toggle_cn_mode_with_commit(&mut self, flush_raw: bool) -> DaemonReply {
        let had_active = self.sess.is_active();
        let mut commit = String::new();
        if had_active {
            if flush_raw {
                commit = format!("{}{}", self.sess.committed_prefix, self.sess.raw);
            }
            self.sess.clear();
            self.popup.hide(&self.x);
        }
        self.chinese_mode = !self.chinese_mode;
        self.publish_status();
        let consumed = !commit.is_empty();
        self.reply_snapshot(consumed, commit)
    }

    fn notify_active_target_of_external_toggle(
        &mut self,
        requester_client_id: u32,
        had_compose: bool,
        rep: &DaemonReply,
    ) {
        if !had_compose && rep.commit_text.is_empty() {
            return;
        }
        match self.active {
            ActiveTarget::Xim {
                server_comm,
                im_id,
                ic_id,
            } => {
                if !rep.commit_text.is_empty() {
                    self.punct.note_committed(&rep.commit_text);
                    self.xim
                        .commit_text(&self.x, server_comm, im_id, ic_id, 0, &rep.commit_text);
                }
            }
            ActiveTarget::Socket {
                client_id,
                window_id,
            } if client_id != requester_client_id => {
                if !rep.commit_text.is_empty() {
                    self.punct.note_committed(&rep.commit_text);
                }
                let mut notify_rep = rep.clone();
                notify_rep.consumed = true;
                self.pending_notify_replies
                    .entry(client_id)
                    .or_default()
                    .push_back(notify_rep);
                if window_id != 0 {
                    self.x.send_spime_notify(window_id);
                    let _ = self.x.flush();
                }
            }
            _ => {}
        }
    }

    fn delete_page_candidate(&mut self, idx_in_page: usize) {
        let ps = self.engine_cfg.page_size;
        let global = self.sess.page * ps + idx_in_page;
        if let Some(cand) = self.sess.snapshot.candidates.get(global).cloned() {
            if !cand.syl_key.is_empty() && !cand.is_english_or_raw {
                self.userdb.delete_candidate(&cand.syl_key, &cand.text);
                self.userdb.flush();
                recompute(
                    &self.dict,
                    &self.lm,
                    &self.userdb,
                    &self.engine_cfg,
                    &mut self.sess,
                );
                self.sync_popup();
            }
        }
    }

    fn handle_composing_key(&mut self, keysym: u32, _state: u16) -> DaemonReply {
        let ps = self.engine_cfg.page_size;
        let is_ms_or_sg = matches!(
            self.engine_cfg.scheme,
            ShuangpinScheme::Microsoft | ShuangpinScheme::Sogou
        );
        let ch_opt = keysym_to_ascii(keysym);

        if let Some(ch) = ch_opt {
            if ch.is_ascii_alphabetic() || ch == '\'' || (ch == ';' && is_ms_or_sg) {
                self.sess.raw.push(ch);
                recompute(
                    &self.dict,
                    &self.lm,
                    &self.userdb,
                    &self.engine_cfg,
                    &mut self.sess,
                );
                self.sync_popup();
                return self.reply_snapshot(true, String::new());
            }
        }

        match keysym {
            // Backspace
            0xff08 => {
                if !self.sess.raw.is_empty() {
                    self.sess.raw.pop();
                } else if !self.sess.committed_prefix.is_empty() {
                    self.sess.committed_prefix.pop();
                    self.sess.committed_syl_keys.pop();
                }
                if self.sess.is_active() {
                    recompute(
                        &self.dict,
                        &self.lm,
                        &self.userdb,
                        &self.engine_cfg,
                        &mut self.sess,
                    );
                }
                self.sync_popup();
                self.reply_snapshot(true, String::new())
            }
            // Escape
            0xff1b => {
                self.reset_compose();
                self.reply_snapshot(true, String::new())
            }
            // Enter commits raw ASCII preedit cleanly
            0xff0d | 0xff8d => {
                let raw_out = format!("{}{}", self.sess.committed_prefix, self.sess.raw);
                self.punct.note_committed(&raw_out);
                self.reset_compose();
                self.reply_snapshot(true, raw_out)
            }
            // Space commits currently highlighted candidate
            0x0020 => {
                let gidx = self.sess.page * ps + self.sess.selected_in_page;
                let commit = select_candidate(
                    &self.dict,
                    &self.lm,
                    &mut self.userdb,
                    &self.engine_cfg,
                    &mut self.sess,
                    gidx,
                )
                .unwrap_or_default();
                if !commit.is_empty() {
                    self.punct.note_committed(&commit);
                }
                self.sync_popup();
                self.reply_snapshot(true, commit)
            }
            // Digits 1..=9 select candidate on current page
            0x0031..=0x0039 => {
                let digit = (keysym - 0x0031) as usize;
                let page_len = self.sess.current_page_slice(ps).len();
                if digit < ps && digit < page_len {
                    let gidx = self.sess.page * ps + digit;
                    let commit = select_candidate(
                        &self.dict,
                        &self.lm,
                        &mut self.userdb,
                        &self.engine_cfg,
                        &mut self.sess,
                        gidx,
                    )
                    .unwrap_or_default();
                    if !commit.is_empty() {
                        self.punct.note_committed(&commit);
                    }
                    self.sync_popup();
                    self.reply_snapshot(true, commit)
                } else {
                    self.reply_snapshot(true, String::new())
                }
            }
            // Page Up / '-' / ','
            0xff55 | 0x002d | 0x002c => {
                self.prev_page();
                self.reply_snapshot(true, String::new())
            }
            // Page Down / '=' / '.'
            0xff56 | 0x003d | 0x002e => {
                self.next_page();
                self.reply_snapshot(true, String::new())
            }
            // Left / Up move selection backward
            0xff51 | 0xff52 => {
                if self.sess.selected_in_page > 0 {
                    self.sess.selected_in_page -= 1;
                } else if self.sess.page > 0 {
                    self.sess.page -= 1;
                    let len = self.sess.current_page_slice(ps).len();
                    self.sess.selected_in_page = len.saturating_sub(1);
                }
                self.sync_popup();
                self.reply_snapshot(true, String::new())
            }
            // Right / Down move selection forward
            0xff53 | 0xff54 => {
                let len = self.sess.current_page_slice(ps).len();
                let total_pages = self.sess.page_count(ps);
                if self.sess.selected_in_page + 1 < len {
                    self.sess.selected_in_page += 1;
                } else if self.sess.page + 1 < total_pages {
                    self.sess.page += 1;
                    self.sess.selected_in_page = 0;
                }
                self.sync_popup();
                self.reply_snapshot(true, String::new())
            }
            _ => {
                if let Some(ch) = ch_opt {
                    // Auto-commit current top candidate + trailing punctuation cleanly.
                    let gidx = self.sess.page * ps + self.sess.selected_in_page;
                    let mut out = select_candidate(
                        &self.dict,
                        &self.lm,
                        &mut self.userdb,
                        &self.engine_cfg,
                        &mut self.sess,
                        gidx,
                    )
                    .unwrap_or_else(|| {
                        let part = self.sess.committed_prefix.clone();
                        self.sess.clear();
                        part
                    });
                    if !out.is_empty() {
                        self.punct.note_committed(&out);
                    }
                    if let Some(mapped) = self.punct.map_char(ch, self.engine_cfg.cn_punct) {
                        out.push_str(mapped);
                    } else {
                        out.push(ch);
                        if ch.is_ascii_digit() {
                            self.punct.note_committed(&out);
                        }
                    }
                    self.sync_popup();
                    self.reply_snapshot(true, out)
                } else {
                    self.reply_snapshot(false, String::new())
                }
            }
        }
    }

    fn handle_idle_chinese_key(&mut self, keysym: u32) -> DaemonReply {
        let Some(ch) = keysym_to_ascii(keysym) else {
            return self.reply_snapshot(false, String::new());
        };
        if ch.is_ascii_lowercase() {
            self.punct.note_committed("");
            self.sess.raw.push(ch);
            recompute(
                &self.dict,
                &self.lm,
                &self.userdb,
                &self.engine_cfg,
                &mut self.sess,
            );
            self.sync_popup();
            return self.reply_snapshot(true, String::new());
        }
        if let Some(mapped) = self.punct.map_char(ch, self.engine_cfg.cn_punct) {
            return self.reply_snapshot(true, mapped.to_string());
        }
        self.reply_snapshot(false, String::new())
    }

    fn prev_page(&mut self) {
        if self.sess.page > 0 {
            self.sess.page -= 1;
            self.sess.selected_in_page = 0;
            self.sync_popup();
        }
    }

    fn next_page(&mut self) {
        let total = self.sess.page_count(self.engine_cfg.page_size);
        if self.sess.page + 1 < total {
            self.sess.page += 1;
            self.sess.selected_in_page = 0;
            self.sync_popup();
        }
    }

    fn commit_popup_selection(&mut self, idx_in_page: usize) {
        let ps = self.engine_cfg.page_size;
        let gidx = self.sess.page * ps + idx_in_page;
        let commit_opt = select_candidate(
            &self.dict,
            &self.lm,
            &mut self.userdb,
            &self.engine_cfg,
            &mut self.sess,
            gidx,
        );
        self.sync_popup();

        match self.active {
            ActiveTarget::Xim {
                server_comm,
                im_id,
                ic_id,
            } => {
                if let Some(text) = commit_opt {
                    self.punct.note_committed(&text);
                    self.xim.commit_text(&self.x, server_comm, im_id, ic_id, 0, &text);
                }
            }
            ActiveTarget::Socket {
                client_id,
                window_id,
            } => {
                let commit_text = commit_opt.unwrap_or_default();
                if !commit_text.is_empty() {
                    self.punct.note_committed(&commit_text);
                }
                let mut rep = self.reply_snapshot(true, commit_text);
                rep.consumed = true;
                self.pending_notify_replies
                    .entry(client_id)
                    .or_default()
                    .push_back(rep);
                if window_id != 0 {
                    self.x.send_spime_notify(window_id);
                    let _ = self.x.flush();
                }
            }
            ActiveTarget::None => {}
        }
    }

    fn process_socket_request(&mut self, req: ClientRequest) -> DaemonReply {
        match req {
            ClientRequest::FocusIn {
                client_id,
                window_id,
            } => {
                let next = ActiveTarget::Socket {
                    client_id,
                    window_id,
                };
                if self.active != next {
                    self.reset_compose();
                    self.punct.reset();
                    self.active = next;
                }
                self.reply_snapshot(true, String::new())
            }
            ClientRequest::FocusOut {
                client_id,
                window_id,
            } => {
                if self.active
                    == (ActiveTarget::Socket {
                        client_id,
                        window_id,
                    })
                {
                    self.reset_compose();
                    self.active = ActiveTarget::None;
                }
                self.reply_snapshot(true, String::new())
            }
            ClientRequest::SetSpot {
                client_id,
                x,
                y,
                h,
            } => {
                if matches!(self.active, ActiveTarget::Socket { client_id: cid, .. } if cid == client_id)
                {
                    let next_spot = (i32::from(x), i32::from(y), i32::from(h));
                    if self.spot != next_spot {
                        self.spot = next_spot;
                        if self.popup.is_mapped() {
                            self.sync_popup();
                        }
                    }
                }
                self.reply_snapshot(true, String::new())
            }
            ClientRequest::KeyEvent {
                client_id,
                window_id,
                is_press,
                keysym,
                state,
                ..
            } => {
                self.active = ActiveTarget::Socket {
                    client_id,
                    window_id,
                };
                self.handle_key(is_press, keysym, state)
            }
            ClientRequest::Reset { client_id } => {
                if matches!(self.active, ActiveTarget::Socket { client_id: cid, .. } if cid == client_id)
                {
                    self.reset_compose();
                }
                self.reply_snapshot(true, String::new())
            }
            ClientRequest::ToggleMode { client_id } => {
                self.poll_uxconf();
                if !self.config.ime_enabled {
                    return self.reply_snapshot(false, String::new());
                }
                let had_compose = self.sess.is_active();
                let rep = self.toggle_cn_mode_with_commit(true);
                self.notify_active_target_of_external_toggle(client_id, had_compose, &rep);
                rep
            }
            ClientRequest::SetChineseMode { client_id, chinese } => {
                if chinese {
                    self.sync_ime_enabled_from_uxconf();
                } else {
                    self.poll_uxconf();
                }
                if self.chinese_mode != chinese {
                    let had_compose = self.sess.is_active();
                    let rep = self.toggle_cn_mode_with_commit(true);
                    self.notify_active_target_of_external_toggle(client_id, had_compose, &rep);
                    return rep;
                }
                self.publish_status();
                self.reply_snapshot(false, String::new())
            }
            ClientRequest::QueryStatus => {
                self.poll_uxconf();
                self.reply_snapshot(false, String::new())
            }
            ClientRequest::PollPending { client_id } => {
                if let Some(rep) = self
                    .pending_notify_replies
                    .get_mut(&client_id)
                    .and_then(VecDeque::pop_front)
                {
                    rep
                } else {
                    self.reply_snapshot(false, String::new())
                }
            }
        }
    }

    fn handle_x11_event(&mut self, ev: Event) {
        if let Event::MappingNotify(mn) = &ev {
            let _ = mn;
            // Keyboard map updates handled cleanly on demand if needed.
        }
        if let Event::PropertyNotify(pn) = &ev {
            if pn.window == self.x.root && pn.atom == self.x.atoms.spui_compositor {
                self.composited = self.x.is_compositor_active();
                if self.popup.is_mapped() {
                    self.sync_popup();
                }
            }
        }
        if let Some(xim_act) = self.xim.handle_event(&self.x, &ev) {
            match xim_act {
                XimAction::FocusIn {
                    conn_idx,
                    ic_id,
                    spot,
                } => {
                    let next = ActiveTarget::Xim {
                        server_comm: conn_idx,
                        im_id: 1,
                        ic_id,
                    };
                    if self.active != next {
                        self.reset_compose();
                        self.punct.reset();
                        self.active = next;
                    }
                    self.spot = spot;
                }
                XimAction::FocusOut { conn_idx, ic_id } => {
                    if matches!(
                        self.active,
                        ActiveTarget::Xim { server_comm, ic_id: cur_ic, .. }
                        if server_comm == conn_idx && cur_ic == ic_id
                    ) {
                        self.reset_compose();
                        self.active = ActiveTarget::None;
                    }
                }
                XimAction::SpotUpdated {
                    conn_idx,
                    ic_id,
                    spot,
                } => {
                    if matches!(
                        self.active,
                        ActiveTarget::Xim { server_comm, ic_id: cur_ic, .. }
                        if server_comm == conn_idx && cur_ic == ic_id
                    ) {
                        self.spot = spot;
                        if self.popup.is_mapped() {
                            self.sync_popup();
                        }
                    }
                }
                XimAction::ResetIc { .. } => self.reset_compose(),
                XimAction::ForwardKey {
                    conn_idx,
                    im_id,
                    ic_id,
                    sync_flag,
                    serial,
                    wire_event,
                    is_press,
                    keycode,
                    state,
                } => {
                    self.active = ActiveTarget::Xim {
                        server_comm: conn_idx,
                        im_id,
                        ic_id,
                    };
                    let sym = self.x.key_symbol(keycode, state);
                    let rep = self.handle_key(is_press, sym, state);
                    if !rep.consumed {
                        self.xim.forward_unconsumed(
                            &self.x,
                            conn_idx,
                            im_id,
                            ic_id,
                            sync_flag,
                            serial,
                            &wire_event,
                        );
                    } else if !rep.commit_text.is_empty() {
                        self.xim.commit_text(
                            &self.x,
                            conn_idx,
                            im_id,
                            ic_id,
                            sync_flag,
                            &rep.commit_text,
                        );
                    } else if sync_flag != 0 {
                        self.xim.send_sync_reply(&self.x, conn_idx, im_id, ic_id);
                    }
                }
            }
        }

        match ev {
            Event::Expose(e) if e.window == self.popup.window => {
                self.popup.present_damage(&self.x);
            }
            Event::MotionNotify(e) if e.event == self.popup.window => {
                if self.popup.on_motion(i32::from(e.event_x), i32::from(e.event_y)) {
                    self.sync_popup();
                }
            }
            Event::LeaveNotify(e) if e.event == self.popup.window => {
                if self.popup.on_leave() {
                    self.sync_popup();
                }
            }
            Event::ButtonPress(e) if e.event == self.popup.window && e.detail == 4 => {
                self.prev_page();
            }
            Event::ButtonPress(e) if e.event == self.popup.window && e.detail == 5 => {
                self.next_page();
            }
            Event::ButtonPress(e) if e.event == self.popup.window && e.detail == 1 => {
                match self
                    .popup
                    .hit_test(i32::from(e.event_x), i32::from(e.event_y))
                {
                    PopupClick::Select(idx) => self.commit_popup_selection(idx),
                    PopupClick::PrevPage => self.prev_page(),
                    PopupClick::NextPage => self.next_page(),
                    PopupClick::ToggleMode => {
                        let rep = self.toggle_cn_mode_with_commit(true);
                        if !rep.commit_text.is_empty() {
                            match self.active {
                                ActiveTarget::Xim {
                                    server_comm,
                                    im_id,
                                    ic_id,
                                } => {
                                    self.xim.commit_text(
                                        &self.x,
                                        server_comm,
                                        im_id,
                                        ic_id,
                                        0,
                                        &rep.commit_text,
                                    );
                                }
                                ActiveTarget::Socket {
                                    client_id,
                                    window_id,
                                } => {
                                    let mut rep = rep;
                                    rep.consumed = true;
                                    self.pending_notify_replies
                                        .entry(client_id)
                                        .or_default()
                                        .push_back(rep);
                                    if window_id != 0 {
                                        self.x.send_spime_notify(window_id);
                                        let _ = self.x.flush();
                                    }
                                }
                                ActiveTarget::None => {}
                            }
                        }
                    }
                    PopupClick::None => {}
                }
            }
            _ => {}
        }
    }

    fn poll_once(&mut self) {
        let mut fds = Vec::with_capacity(4 + self.clients.len());
        let x_fd = self.x.conn.stream().as_raw_fd();
        let lst_fd = self.listener.as_raw_fd();
        let ux_fd = self.watch.as_ref().map(UxConfWatch::as_raw_fd);
        fds.push(libc::pollfd {
            fd: x_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        fds.push(libc::pollfd {
            fd: lst_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        if let Some(ufd) = ux_fd {
            fds.push(libc::pollfd {
                fd: ufd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let client_fd_base = fds.len();
        for c in &self.clients {
            fds.push(libc::pollfd {
                fd: c.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        unsafe {
            libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 250);
        }

        // 1. Accept any waiting SPUI socket connections.
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(true);
                    self.clients.push(SocketClient {
                        stream,
                        buf: Vec::with_capacity(256),
                        client_id: None,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        // 2. Drain X11 / XIM / CandidatePopup events.
        while let Ok(Some(ev)) = self.x.poll() {
            self.handle_x11_event(ev);
        }

        // 3. Poll live uxconf updates.
        self.poll_uxconf();

        // 4. Service connected socket clients cleanly.
        let mut idx = 0usize;
        while idx < self.clients.len() {
            let revents = fds.get(client_fd_base + idx).map_or(0, |p| p.revents);
            if revents == 0 {
                idx += 1;
                continue;
            }
            let mut closed = false;
            let mut tmp = [0u8; 2048];
            loop {
                match self.clients[idx].stream.read(&mut tmp) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => self.clients[idx].buf.extend_from_slice(&tmp[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        closed = true;
                        break;
                    }
                }
            }

            while self.clients[idx].buf.len() >= 4 {
                let len = u32::from_le_bytes(
                    self.clients[idx].buf[0..4].try_into().unwrap_or([0; 4]),
                ) as usize;
                if len == 0 || len > spime::proto::MAX_PACKET_BYTES {
                    closed = true;
                    break;
                }
                if self.clients[idx].buf.len() < 4 + len {
                    break;
                }
                let payload: Vec<u8> = self.clients[idx].buf.drain(..4 + len).skip(4).collect();
                if let Ok(req) = ClientRequest::decode(&payload) {
                    match &req {
                        ClientRequest::FocusIn { client_id, .. }
                        | ClientRequest::KeyEvent { client_id, .. } => {
                            self.clients[idx].client_id = Some(*client_id);
                        }
                        _ => {}
                    }
                    let reply = self.process_socket_request(req);
                    let enc = reply.encode();
                    let res = match write_frame(&mut self.clients[idx].stream, &enc) {
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            let _ = self.clients[idx].stream.set_nonblocking(false);
                            let retry = write_frame(&mut self.clients[idx].stream, &enc);
                            let _ = self.clients[idx].stream.set_nonblocking(true);
                            retry
                        }
                        other => other,
                    };
                    if res.is_err() {
                        closed = true;
                        break;
                    }
                }
            }

            if closed {
                let dropped = self.clients.swap_remove(idx);
                if let Some(cid) = dropped.client_id {
                    self.pending_notify_replies.remove(&cid);
                    if matches!(self.active, ActiveTarget::Socket { client_id, .. } if client_id == cid)
                    {
                        self.reset_compose();
                        self.active = ActiveTarget::None;
                    }
                }
            } else {
                idx += 1;
            }
        }
    }
}

fn keysym_to_ascii(sym: u32) -> Option<char> {
    if (0x0020..=0x007e).contains(&sym) {
        char::from_u32(sym)
    } else {
        None
    }
}

fn is_modifier_sym(sym: u32) -> bool {
    (0xffe1..=0xffee).contains(&sym) || (0xfe01..=0xfe1f).contains(&sym)
}

pub fn run() -> Result<()> {
    let x = X::connect()?;
    let _ = x.conn.change_window_attributes(
        x.root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    );

    let config = UxConfClient::connect()
        .ok()
        .as_mut()
        .map(UxConfClient::load_desktop_config)
        .unwrap_or_default();
    let watch = UxConfWatch::subscribe(None).ok();
    let theme = Theme::from_config(&config);
    let mut text = TextRenderer::load()?;
    text.set_ui_scale(theme.ui_scale());
    let composited = x.is_compositor_active();

    let sock_path = socket_path();
    if let Some(parent) = sock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;
    listener.set_nonblocking(true)?;

    let xim = XimServer::start(&x)?;
    let popup = CandidatePopup::new(&x, &theme)?;
    let dict = SpimeDict::open_default();
    let lm = SpimeLm::open_default();
    let (dict_keys, dict_entries, dict_fallback) = dict.stats();
    let userdb = UserDb::open_default();
    let (engine_cfg, layout) = Daemon::config_to_engine(&config);

    let mut daemon = Daemon {
        x,
        text,
        config,
        watch,
        theme,
        composited,
        dict,
        lm,
        userdb,
        engine_cfg,
        layout,
        sess: ComposeSession::default(),
        punct: PunctState::default(),
        chinese_mode: false,
        left_shift_down: false,
        left_shift_clean: false,
        spot: (200, 560, 22),
        active: ActiveTarget::None,
        popup,
        xim,
        listener,
        sock_path,
        clients: Vec::new(),
        pending_notify_replies: HashMap::new(),
    };
    daemon.publish_status();
    eprintln!(
        "spimed: ready (socket={}, XIM=@im=spime, dict_keys={}, dict_entries={}, fallback={})",
        daemon.sock_path.display(),
        dict_keys,
        dict_entries,
        dict_fallback
    );

    let _unused = read_frame::<io::Empty>;
    loop {
        daemon.poll_once();
    }
}
