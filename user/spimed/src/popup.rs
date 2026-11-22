/*
 *
 *       user/desktop/src/ime/popup.rs
 *       32-bit ARGB override-redirect candidate bar window adapting to both
 *       VortexGlass (frosted glass + drop shadow via `_SPUI_GLASS_EXTENTS`) and
 *       Win98 3D bevel themes, supporting horizontal and vertical layouts.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use spime::decoder::Candidate;
use spui_ui::draw::{mix, Canvas, Rect};
use spui_ui::text::{Face, TextGlow, TextRenderer};
use spui_ui::theme::Theme;
use spui_ui::ui;
use spui_ui::x11::{Result, X};
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt, EventMask, StackMode, Window};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupLayout {
    Horizontal,
    Vertical,
}

impl PopupLayout {
    pub fn from_config(s: &str) -> Self {
        if s.eq_ignore_ascii_case("vertical") {
            Self::Vertical
        } else {
            Self::Horizontal
        }
    }
}

pub enum PopupClick {
    None,
    Select(usize),
    PrevPage,
    NextPage,
    ToggleMode,
}

/// Floating X11 candidate bar owned by `spimed`.
pub struct CandidatePopup {
    pub window: Window,
    pub depth: u8,
    pub argb: bool,
    shaped: bool,
    canvas: Canvas,
    mapped: bool,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    hover_cand: Option<usize>,
    cand_rects: Vec<Rect>,
    prev_rect: Rect,
    next_rect: Rect,
    mode_rect: Rect,
}

impl CandidatePopup {
    pub fn new(x11: &X, theme: &Theme) -> Result<Self> {
        let win = x11.generate_id()?;
        let (window, is_argb) = x11.create_argb_window(
            win,
            x11.root,
            0,
            0,
            240,
            64,
            true,
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::POINTER_MOTION
                | EventMask::LEAVE_WINDOW,
            theme.panel,
        )?;
        let depth = if is_argb { 32 } else { x11.depth };
        let use_glass = is_argb && theme.is_vortexglass();
        let mut canvas = x11.create_canvas(240, 64, theme.panel);
        canvas.set_argb(use_glass);
        if is_argb {
            let _ = x11.set_glass_extents(window, 64, -1, 0, 0, theme.radius_popup());
        }
        let shaped = if !use_glass {
            let _ = x11.set_rounded_shape(window, 240, 64, theme.radius_popup());
            true
        } else {
            false
        };
        Ok(Self {
            window,
            depth,
            argb: is_argb,
            shaped,
            canvas,
            mapped: false,
            width: 240,
            height: 64,
            x: 0,
            y: 0,
            hover_cand: None,
            cand_rects: Vec::new(),
            prev_rect: Rect::default(),
            next_rect: Rect::default(),
            mode_rect: Rect::default(),
        })
    }

    pub fn is_mapped(&self) -> bool {
        self.mapped
    }

    pub fn hide(&mut self, x11: &X) {
        if self.mapped {
            let _ = x11.unmap(self.window);
            let _ = x11.flush();
            self.mapped = false;
            self.hover_cand = None;
        }
    }

    fn cand_index_at(&self, px: i32, py: i32) -> Option<usize> {
        self.cand_rects
            .iter()
            .position(|r| r.expand(3).contains(px, py))
    }

    pub fn on_motion(&mut self, px: i32, py: i32) -> bool {
        let next = self.cand_index_at(px, py);
        if next != self.hover_cand {
            self.hover_cand = next;
            true
        } else {
            false
        }
    }

    pub fn on_leave(&mut self) -> bool {
        if self.hover_cand.take().is_some() {
            true
        } else {
            false
        }
    }

    pub fn hit_test(&self, px: i32, py: i32) -> PopupClick {
        if self.prev_rect.expand(5).contains(px, py) {
            return PopupClick::PrevPage;
        }
        if self.next_rect.expand(5).contains(px, py) {
            return PopupClick::NextPage;
        }
        if self.mode_rect.expand(5).contains(px, py) {
            return PopupClick::ToggleMode;
        }
        if let Some(idx) = self.cand_index_at(px, py) {
            return PopupClick::Select(idx);
        }
        PopupClick::None
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show_and_render(
        &mut self,
        x11: &X,
        text: &mut TextRenderer,
        theme: &Theme,
        composited: bool,
        layout: PopupLayout,
        scheme_tag: &str,
        preedit: &str,
        cands: &[Candidate],
        selected_idx: usize,
        page: usize,
        total_pages: usize,
        spot_x: i32,
        spot_y: i32,
        spot_h: i32,
    ) -> Result<()> {
        let header_h = 28;
        let pad_x = 12;
        let preedit_w = text.measure(preedit, 13.5, Face::Ui).ceil() as i32 + 96;
        let mut item_widths = Vec::with_capacity(cands.len());
        let mut total_horiz_w = pad_x * 2 + 44; // pager room
        let mut max_vert_w = preedit_w.max(220);

        for (i, c) in cands.iter().enumerate() {
            let label = format!("{}. {}", i + 1, c.text);
            let w = text.measure(&label, 14.0, Face::Ui).ceil() as i32 + 16;
            item_widths.push(w);
            total_horiz_w += w + 6;
            max_vert_w = max_vert_w.max(w + pad_x * 2);
        }

        let (raw_w, req_h) = match layout {
            PopupLayout::Horizontal => (
                total_horiz_w.max(preedit_w).clamp(220, 920),
                header_h + 38,
            ),
            PopupLayout::Vertical => (
                max_vert_w.clamp(220, 480),
                header_h + 10 + (cands.len().max(1) as i32) * 28,
            ),
        };
        // Quantize width to 48px buckets with shrink hysteresis so typing letters does not
        // destroy/recreate XShm surfaces and XShape masks on every single keystroke.
        let bucket_w = (((raw_w + 47) / 48) * 48).clamp(220, 920);
        let req_w = if self.mapped
            && self.height == req_h
            && self.width >= raw_w
            && self.width <= bucket_w + 64
        {
            self.width
        } else {
            bucket_w
        };

        let (pos_x, pos_y) = place_near_spot(
            spot_x,
            spot_y,
            spot_h,
            req_w,
            req_h,
            i32::from(x11.width),
            i32::from(x11.height),
        );

        let size_changed = req_w != self.width || req_h != self.height;
        let pos_changed = pos_x != self.x || pos_y != self.y;
        self.width = req_w;
        self.height = req_h;
        self.x = pos_x;
        self.y = pos_y;

        if size_changed || pos_changed {
            x11.move_resize(self.window, pos_x as i16, pos_y as i16, req_w as u16, req_h as u16)?;
        }
        let radius = theme.radius_popup();
        let use_glass = self.argb && composited && theme.is_vortexglass();
        if size_changed {
            x11.release_canvas(&self.canvas);
            self.canvas = x11.create_canvas(req_w as usize, req_h as usize, theme.panel);
            if self.argb {
                let _ = x11.set_glass_extents(self.window, req_h, -1, 0, 0, radius);
            }
        }
        if use_glass {
            if self.shaped {
                let _ = x11.clear_shape(self.window);
                self.shaped = false;
            }
        } else if size_changed || !self.shaped {
            let _ = x11.set_rounded_shape(self.window, req_w as u16, req_h as u16, radius);
            self.shaped = true;
        }
        self.canvas.set_argb(use_glass);

        self.draw_contents(
            text,
            theme,
            use_glass,
            radius,
            layout,
            scheme_tag,
            preedit,
            cands,
            &item_widths,
            selected_idx,
            page,
            total_pages,
        );

        if !self.mapped {
            x11.map(self.window)?;
            let _ = x11.conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );
            self.mapped = true;
        }
        x11.put_canvas_depth(
            self.window,
            &self.canvas,
            Rect::new(0, 0, req_w, req_h),
            self.depth,
        )?;
        x11.flush()?;
        Ok(())
    }

    pub fn present_damage(&self, x11: &X) {
        if self.mapped {
            let _ = x11.put_canvas_depth(
                self.window,
                &self.canvas,
                Rect::new(0, 0, self.width, self.height),
                self.depth,
            );
            let _ = x11.flush();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_contents(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        use_glass: bool,
        radius: i32,
        layout: PopupLayout,
        scheme_tag: &str,
        preedit: &str,
        cands: &[Candidate],
        item_widths: &[i32],
        selected_idx: usize,
        page: usize,
        total_pages: usize,
    ) {
        let full = Rect::new(0, 0, self.width, self.height);
        let dark = theme.is_dark();

        if use_glass {
            self.canvas.clear_transparent(full, theme.panel);
            let (tint, ta) = theme.glass_tint(true);
            self.canvas
                .rounded_rect(full, radius, tint, (ta as f32 / 255.0).max(0.54));
            let inner = full.inset(3);
            let card_bg = if dark { 0x162230 } else { 0xf7fbff };
            self.canvas.rounded_rect(inner, (radius - 2).max(2), card_bg, 0.86);
            self.canvas.rounded_border(full, radius, 0x0f1a28, 0.72, 1);
            self.canvas
                .rounded_border(full.inset(1), (radius - 1).max(1), 0xffffff, 0.56, 1);
        } else {
            self.canvas.clear(theme.panel);
            ui::popup_surface(&mut self.canvas, theme, full, radius);
        }

        let fg = theme.text;
        let muted = theme.text_muted;
        let accent = theme.accent;
        let header_h = 28;

        // Header: preedit string + mode badge + page indicator
        self.mode_rect = Rect::new(8, 5, 22, 18);
        self.canvas.rounded_rect(
            self.mode_rect,
            4,
            mix(accent, theme.panel, if dark { 0.45 } else { 0.78 }),
            0.95,
        );
        self.canvas
            .rounded_border(self.mode_rect, 4, accent, 0.55, 1);
        let tw_mode = text.measure(scheme_tag, 11.0, Face::Bold);
        text.draw(
            &mut self.canvas,
            scheme_tag,
            self.mode_rect.x as f32 + (self.mode_rect.w as f32 - tw_mode) * 0.5,
            self.mode_rect.y as f32 + 13.5,
            11.0,
            if dark { 0xffffff } else { mix(accent, 0x0a1a2e, 0.55) },
            Face::Bold,
        );

        let preedit_max_w = (self.width - 102).max(60) as f32;
        let shown_pre = text.truncate(preedit, preedit_max_w, 13.5, Face::Ui);
        let glow = TextGlow::aero_compact(dark, true);
        if use_glass {
            text.draw_glow(
                &mut self.canvas,
                &shown_pre,
                36.0,
                19.0,
                13.5,
                Face::Ui,
                glow,
            );
        }
        text.draw(
            &mut self.canvas,
            &shown_pre,
            36.0,
            19.0,
            13.5,
            fg,
            Face::Ui,
        );

        // Right-aligned page controls (`< 1/3 >`)
        self.next_rect = Rect::new(self.width - 24, 5, 16, 18);
        self.prev_rect = Rect::new(self.width - 42, 5, 16, 18);
        let page_lbl = format!("{}/{}", page + 1, total_pages.max(1));
        let plw = text.measure(&page_lbl, 11.0, Face::Ui);
        text.draw(
            &mut self.canvas,
            &page_lbl,
            (self.prev_rect.x as f32 - plw - 6.0).max(40.0),
            18.0,
            11.0,
            muted,
            Face::Ui,
        );
        let prev_col = if page > 0 { fg } else { muted };
        let next_col = if page + 1 < total_pages { fg } else { muted };
        text.draw(
            &mut self.canvas,
            "‹",
            (self.prev_rect.x + 5) as f32,
            18.5,
            13.0,
            prev_col,
            Face::Bold,
        );
        text.draw(
            &mut self.canvas,
            "›",
            (self.next_rect.x + 5) as f32,
            18.5,
            13.0,
            next_col,
            Face::Bold,
        );

        // Divider hairline
        self.canvas
            .fill_u8(Rect::new(8, header_h, self.width - 16, 1), theme.bevel_shadow, 150);

        // Candidates
        self.cand_rects.clear();
        match layout {
            PopupLayout::Horizontal => {
                let mut cx = 10;
                let cy = header_h + 4;
                let ch = (self.height - cy - 4).max(26);
                for (i, c) in cands.iter().enumerate() {
                    let w = *item_widths.get(i).unwrap_or(&48);
                    let r = Rect::new(cx, cy, w, ch);
                    self.draw_candidate_chip(text, theme, r, i, &c.text, i == selected_idx);
                    self.cand_rects.push(r);
                    cx += w + 4;
                }
            }
            PopupLayout::Vertical => {
                let mut cy = header_h + 6;
                let ch = 26;
                let cw = (self.width - 16).max(120);
                for (i, c) in cands.iter().enumerate() {
                    let r = Rect::new(8, cy, cw, ch);
                    self.draw_candidate_chip(text, theme, r, i, &c.text, i == selected_idx);
                    self.cand_rects.push(r);
                    cy += ch + 2;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_candidate_chip(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        idx: usize,
        word: &str,
        selected: bool,
    ) {
        let hovered = self.hover_cand == Some(idx);
        let dark = theme.is_dark();
        if selected {
            self.canvas.rounded_rect(rect, 4, theme.accent, 0.92);
        } else if hovered {
            self.canvas.rounded_rect(
                rect,
                4,
                mix(theme.accent, theme.panel, if dark { 0.55 } else { 0.80 }),
                0.75,
            );
        }
        let num_str = format!("{}.", idx + 1);
        let num_col = if selected {
            mix(0xffffff, theme.accent, 0.15)
        } else {
            theme.text_muted
        };
        let txt_col = if selected { 0xffffff } else { theme.text };
        let baseline = rect.y as f32 + 17.5;
        text.draw(
            &mut self.canvas,
            &num_str,
            (rect.x + 7) as f32,
            baseline,
            13.0,
            num_col,
            Face::Ui,
        );
        let nw = text.measure(&num_str, 13.0, Face::Ui);
        text.draw(
            &mut self.canvas,
            word,
            rect.x as f32 + 9.0 + nw,
            baseline,
            14.0,
            txt_col,
            if selected { Face::Bold } else { Face::Ui },
        );
    }
}

/// Place popup cleanly below `(spot_x, spot_y)` or flip above if colliding with bottom taskbar.
pub fn place_near_spot(
    spot_x: i32,
    spot_y: i32,
    spot_h: i32,
    w: i32,
    h: i32,
    scr_w: i32,
    scr_h: i32,
) -> (i32, i32) {
    let px = spot_x.clamp(8, (scr_w - w - 8).max(8));
    let below_y = spot_y + spot_h.max(16) + 6;
    let py = if below_y + h <= scr_h - 50 {
        below_y
    } else {
        (spot_y - h - 6).clamp(8, (scr_h - h - 50).max(8))
    };
    (px, py)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_placement_clamps_right_and_flips_above_taskbar() {
        assert_eq!(place_near_spot(1200, 200, 20, 260, 64, 1280, 800), (1012, 226));
        assert_eq!(place_near_spot(120, 730, 20, 260, 64, 1280, 800), (120, 660));
    }
}
