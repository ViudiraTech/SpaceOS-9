#include "widgets.hpp"
#include "dom_bridge.hpp"
#include "page_internal.hpp"
#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstring>

namespace space {

using litehtml::operator""_px;

namespace {

size_t prev_utf8_boundary(const std::string& s, size_t idx) {
    if (idx == 0 || s.empty()) return 0;
    size_t i = std::min(idx, s.size());
    do {
        --i;
    } while (i > 0 && (static_cast<unsigned char>(s[i]) & 0xC0) == 0x80);
    return i;
}

size_t next_utf8_boundary(const std::string& s, size_t idx) {
    if (idx >= s.size()) return s.size();
    size_t i = idx + 1;
    while (i < s.size() && (static_cast<unsigned char>(s[i]) & 0xC0) == 0x80) {
        ++i;
    }
    return i;
}

litehtml::uint_ptr pick_font(const litehtml::element* el, SpacePage* page, int& out_h) {
    litehtml::uint_ptr fh = el->css().get_font();
    out_h = static_cast<int>(el->css().get_font_metrics().height.value());
    if (fh == 0 && page) {
        page->ensure_ui_font();
        fh = page->ui_font;
        if (out_h <= 0) out_h = 18;
    }
    if (out_h <= 0) out_h = 16;
    return fh;
}

void draw_box_border_uniform(const SpaceHostCallbacks& cb, int x, int y, int w, int h,
                             uint32_t col, int width = 1, int radius = 0) {
    if (!cb.draw_borders || w <= 0 || h <= 0) return;
    SpaceBorders b{};
    b.x = x;
    b.y = y;
    b.w = w;
    b.h = h;
    b.left = {width, col, SPACE_BORDER_SOLID};
    b.top = {width, col, SPACE_BORDER_SOLID};
    b.right = {width, col, SPACE_BORDER_SOLID};
    b.bottom = {width, col, SPACE_BORDER_SOLID};
    b.radius_tl_x = b.radius_tl_y = radius;
    b.radius_tr_x = b.radius_tr_y = radius;
    b.radius_br_x = b.radius_br_y = radius;
    b.radius_bl_x = b.radius_bl_y = radius;
    cb.draw_borders(cb.user_data, &b);
}

void draw_win98_bevel(const SpaceHostCallbacks& cb, int x, int y, int w, int h, bool sunken) {
    if (!cb.draw_borders || w < 4 || h < 4) return;
    uint32_t outer_tl = sunken ? 0xff808080u : 0xffffffffu;
    uint32_t outer_br = sunken ? 0xffffffffu : 0xff404040u;
    uint32_t inner_tl = sunken ? 0xff404040u : 0xffdfdfdfu;
    uint32_t inner_br = sunken ? 0xffdfdfdfu : 0xff808080u;
    SpaceBorders b{};
    b.x = x;
    b.y = y;
    b.w = w;
    b.h = h;
    b.left = {1, outer_tl, SPACE_BORDER_SOLID};
    b.top = {1, outer_tl, SPACE_BORDER_SOLID};
    b.right = {1, outer_br, SPACE_BORDER_SOLID};
    b.bottom = {1, outer_br, SPACE_BORDER_SOLID};
    cb.draw_borders(cb.user_data, &b);

    SpaceBorders b2{};
    b2.x = x + 1;
    b2.y = y + 1;
    b2.w = w - 2;
    b2.h = h - 2;
    b2.left = {1, inner_tl, SPACE_BORDER_SOLID};
    b2.top = {1, inner_tl, SPACE_BORDER_SOLID};
    b2.right = {1, inner_br, SPACE_BORDER_SOLID};
    b2.bottom = {1, inner_br, SPACE_BORDER_SOLID};
    cb.draw_borders(cb.user_data, &b2);
}

void draw_aero_gloss_box(const SpaceHostCallbacks& cb, int x, int y, int w, int h, int rad,
                         uint32_t c0, uint32_t c1, uint32_t c2, uint32_t c3,
                         uint32_t border, uint32_t inner_rim) {
    if (w <= 0 || h <= 0) return;
    int radii[8] = {rad, rad, rad, rad, rad, rad, rad, rad};
    if (cb.draw_linear_gradient) {
        uint32_t colors[4] = {c0, c1, c2, c3};
        float stops[4] = {0.0f, 0.48f, 0.50f, 1.0f};
        cb.draw_linear_gradient(cb.user_data, x, y, w, h,
                                static_cast<float>(x), static_cast<float>(y),
                                static_cast<float>(x), static_cast<float>(y + h),
                                colors, stops, 4, rad > 0 ? radii : nullptr);
    } else if (cb.fill_rect) {
        cb.fill_rect(cb.user_data, x, y, w, h, c2, rad > 0 ? radii : nullptr);
    }
    if (w > 2 && h > 2) {
        draw_box_border_uniform(cb, x + 1, y + 1, w - 2, h - 2, inner_rim, 1, std::max(0, rad - 1));
    }
    draw_box_border_uniform(cb, x, y, w, h, border, 1, rad);
}

bool is_focused_el(const SpacePage* page, const litehtml::element* self) {
    if (!page || !page->page_focused) return false;
    auto f = page->focused_el.lock();
    return f && f.get() == self;
}

bool is_active_el(const SpacePage* page, const litehtml::element* self) {
    if (!page) return false;
    auto a = page->active_el.lock();
    return a && a.get() == self;
}

} // namespace

void el_script::parse_attributes() {
    html_tag::parse_attributes();
}

void el_script::compute_styles(bool recursive) {
    html_tag::compute_styles(recursive);
    css().set_display(litehtml::display_none);
}

// =========================================================================
// <input>
// =========================================================================

el_space_input::el_space_input(const std::shared_ptr<litehtml::document>& doc, SpacePage* page)
    : litehtml::html_tag(doc), page_(page) {}

std::string el_space_input::input_type() const {
    const char* t = get_attr("type");
    if (!t || !*t) return "text";
    return to_lower_ascii(t);
}

bool el_space_input::is_hidden() const {
    return input_type() == "hidden";
}

bool el_space_input::is_checkable() const {
    std::string t = input_type();
    return t == "checkbox" || t == "radio";
}

bool el_space_input::is_button_like() const {
    std::string t = input_type();
    return t == "submit" || t == "button" || t == "reset";
}

bool el_space_input::is_text_like() const {
    return !is_hidden() && !is_checkable() && !is_button_like() && input_type() != "range";
}

bool el_space_input::is_replaced() const {
    return !is_hidden();
}

void el_space_input::parse_attributes() {
    if (!value_dirty_) {
        const char* val = get_attr("value");
        value_ = val ? val : "";
        if (input_type() == "range" && value_.empty()) value_ = "50";
        caret_ = value_.size();
    }
    if (!checked_dirty_) {
        checked_ = (get_attr("checked") != nullptr);
    }
    html_tag::parse_attributes();
}

void el_space_input::compute_styles(bool recursive) {
    html_tag::compute_styles(recursive);
    if (is_hidden()) {
        css().set_display(litehtml::display_none);
    }
}

void el_space_input::set_current_value(const std::string& v, bool mark_dirty_flag) {
    value_ = v;
    value_dirty_ = true;
    caret_ = std::min(caret_, value_.size());
    if (mark_dirty_flag && page_) {
        page_->mark_dirty(is_button_like(), true);
    }
}

void el_space_input::uncheck_radio_siblings() {
    const char* my_name = get_attr("name");
    if (!my_name || !*my_name) return;
    auto doc = get_document();
    if (!doc) return;
    auto root = doc->root();
    if (!root) return;
    std::vector<litehtml::element::ptr> stack = {root};
    while (!stack.empty()) {
        auto cur = stack.back();
        stack.pop_back();
        if (cur.get() != this && cur->get_tagName() && std::strcmp(cur->get_tagName(), "input") == 0) {
            if (auto* other = dynamic_cast<el_space_input*>(cur.get())) {
                if (other->input_type() == "radio") {
                    const char* oname = other->get_attr("name");
                    if (oname && std::strcmp(oname, my_name) == 0) {
                        other->set_checked(false, false);
                    }
                }
            }
        }
        for (const auto& ch : cur->children()) {
            stack.push_back(ch);
        }
    }
}

void el_space_input::set_checked(bool c, bool handle_radio_group) {
    if (handle_radio_group && c && input_type() == "radio") {
        uncheck_radio_siblings();
    }
    checked_ = c;
    checked_dirty_ = true;
    if (page_) page_->mark_dirty(false, true);
}

std::string el_space_input::display_text() const {
    std::string t = input_type();
    if (t == "password") {
        return std::string(value_.size(), '*');
    }
    if (t == "submit") {
        return value_.empty() ? "Submit" : value_;
    }
    if (t == "reset") {
        return value_.empty() ? "Reset" : value_;
    }
    return value_;
}

void el_space_input::get_content_size(litehtml::size& sz, litehtml::pixel_t /*max_width*/) {
    std::string t = input_type();
    if (is_hidden()) {
        sz.width = 0_px;
        sz.height = 0_px;
        return;
    }
    if (is_checkable()) {
        sz.width = 16_px;
        sz.height = 16_px;
        return;
    }
    if (t == "range") {
        sz.width = 140_px;
        sz.height = 22_px;
        return;
    }
    int fh_px = 18;
    litehtml::uint_ptr fh = pick_font(this, page_, fh_px);
    if (is_button_like()) {
        std::string txt = display_text();
        int tw = (page_ && page_->cb.text_width)
                     ? page_->cb.text_width(page_->cb.user_data, txt.c_str(), fh)
                     : static_cast<int>(txt.size() * 8);
        sz.width = static_cast<float>(std::max(54, tw + 18));
        sz.height = static_cast<float>(std::max(24, fh_px + 8));
        return;
    }
    int cols = 20;
    if (const char* sz_attr = get_attr("size")) {
        int parsed = std::atoi(sz_attr);
        if (parsed > 0 && parsed < 200) cols = parsed;
    }
    sz.width = static_cast<float>(std::max(60, cols * 8));
    sz.height = static_cast<float>(std::max(22, fh_px + 6));
}

void el_space_input::draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
                          const litehtml::position* clip,
                          const std::shared_ptr<litehtml::render_item>& ri) {
    if (is_hidden()) return;
    html_tag::draw(hdc, x, y, clip, ri);
    if (!page_) return;

    litehtml::position pos = ri->pos();
    pos.x += x;
    pos.y += y;
    int px = static_cast<int>(std::round(pos.x.value()));
    int py = static_cast<int>(std::round(pos.y.value()));
    int pw = static_cast<int>(std::round(pos.width.value()));
    int ph = static_cast<int>(std::round(pos.height.value()));
    if (pw <= 0 || ph <= 0) return;

    const auto& cb = page_->cb;
    const bool focused = is_focused_el(page_, this);
    const bool active = is_active_el(page_, this);
    const bool dark = page_->dark_mode;
    const bool win98 = page_->is_win98;
    const bool vortex = page_->is_vortex;
    const int rad = win98 ? 0 : (vortex ? 4 : 5);
    int radii[8] = {rad, rad, rad, rad, rad, rad, rad, rad};
    std::string t = input_type();

    int fh_px = 16;
    litehtml::uint_ptr fh = pick_font(this, page_, fh_px);

    if (t == "checkbox") {
        uint32_t bg = win98 ? 0xffffffffu : (checked_ ? 0xff2563ebu : (dark ? 0xff242834u : 0xffffffffu));
        int box_sz = std::min(pw, ph);
        int bx = px + (pw - box_sz) / 2;
        int by = py + (ph - box_sz) / 2;
        int r3[8] = {win98 ? 0 : 3, win98 ? 0 : 3, win98 ? 0 : 3, win98 ? 0 : 3,
                     win98 ? 0 : 3, win98 ? 0 : 3, win98 ? 0 : 3, win98 ? 0 : 3};
        if (vortex) {
            if (checked_) {
                draw_aero_gloss_box(cb, bx, by, box_sz, box_sz, 3,
                                    0xff9ad3fbu, 0xff4fa4eau, 0xff1e75c9u, 0xff3690deu,
                                    0xff2c628bu, 0x80ffffffu);
            } else {
                draw_aero_gloss_box(cb, bx, by, box_sz, box_sz, 3,
                                    dark ? 0xff283446u : 0xffffffffu,
                                    dark ? 0xff202b3bu : 0xfff2f6fbu,
                                    dark ? 0xff18212fu : 0xffe4ecf5u,
                                    dark ? 0xff1e2938u : 0xffeef4fbu,
                                    focused ? (dark ? 0xff52a6ebu : 0xff3c7fb1u) : (dark ? 0xff566b87u : 0xff708299u),
                                    dark ? 0x30ffffffu : 0xd8ffffffu);
            }
        } else {
            if (cb.fill_rect) cb.fill_rect(cb.user_data, bx, by, box_sz, box_sz, bg, win98 ? nullptr : r3);
            if (win98) {
                draw_win98_bevel(cb, bx, by, box_sz, box_sz, true);
            } else {
                uint32_t bcol = focused ? 0xff3b82f6u : (checked_ ? 0xff2563ebu : (dark ? 0xff4b5563u : 0xff94a3b8u));
                draw_box_border_uniform(cb, bx, by, box_sz, box_sz, bcol, 1, 3);
            }
        }
        if (checked_ && cb.fill_rect) {
            uint32_t tick_col = win98 ? 0xff000000u : 0xffffffffu;
            int pad = std::max(3, box_sz / 4);
            cb.fill_rect(cb.user_data, bx + pad, by + pad, box_sz - 2 * pad, box_sz - 2 * pad, tick_col, nullptr);
        }
        return;
    }

    if (t == "radio") {
        int d = std::min(pw, ph);
        int bx = px + (pw - d) / 2;
        int by = py + (ph - d) / 2;
        int hr = d / 2;
        int rrad[8] = {hr, hr, hr, hr, hr, hr, hr, hr};
        uint32_t bg = win98 ? 0xffffffffu : (vortex ? (dark ? 0xff1e2838u : 0xfff8fbffu) : (dark ? 0xff242834u : 0xffffffffu));
        if (cb.fill_rect) cb.fill_rect(cb.user_data, bx, by, d, d, bg, rrad);
        uint32_t bcol = vortex
            ? (focused ? 0xff3c7fb1u : (checked_ ? 0xff2a7ac4u : (dark ? 0xff5a6f8cu : 0xff6e849eu)))
            : (focused ? 0xff3b82f6u : (checked_ ? 0xff2563ebu : (dark ? 0xff6b7280u : 0xff64748bu)));
        draw_box_border_uniform(cb, bx, by, d, d, bcol, win98 ? 1 : (vortex ? 1 : 2), hr);
        if (checked_ && cb.fill_rect) {
            int pad = std::max(4, d / 3);
            int inner = std::max(4, d - 2 * pad);
            int ir = inner / 2;
            int irad[8] = {ir, ir, ir, ir, ir, ir, ir, ir};
            cb.fill_rect(cb.user_data, bx + (d - inner) / 2, by + (d - inner) / 2, inner, inner,
                         win98 ? 0xff000000u : (vortex ? 0xff2582d6u : 0xff2563ebu), irad);
        }
        return;
    }

    if (t == "range") {
        double minv = get_attr("min") ? std::atof(get_attr("min")) : 0.0;
        double maxv = get_attr("max") ? std::atof(get_attr("max")) : 100.0;
        if (maxv <= minv) maxv = minv + 1.0;
        double cur = value_.empty() ? 50.0 : std::atof(value_.c_str());
        double ratio = std::clamp((cur - minv) / (maxv - minv), 0.0, 1.0);
        int track_y = py + ph / 2 - 2;
        int track_r[8] = {2, 2, 2, 2, 2, 2, 2, 2};
        if (cb.fill_rect) {
            cb.fill_rect(cb.user_data, px + 4, track_y, pw - 8, 4,
                         vortex ? (dark ? 0xff1d2838u : 0xffd2dfeeu) : (dark ? 0xff374151u : 0xffcbd5e1u),
                         win98 ? nullptr : track_r);
            cb.fill_rect(cb.user_data, px + 4, track_y, static_cast<int>((pw - 8) * ratio), 4,
                         win98 ? 0xff000080u : (vortex ? 0xff2c8ce0u : 0xff2563ebu), win98 ? nullptr : track_r);
            int kw = 12, kh = 16;
            int kx = px + 4 + static_cast<int>((pw - 16) * ratio) - kw / 2 + 4;
            int ky = py + (ph - kh) / 2;
            int kr[8] = {6, 6, 6, 6, 6, 6, 6, 6};
            if (vortex) {
                draw_aero_gloss_box(cb, kx, ky, kw, kh, 4,
                                    dark ? 0xff3a4d66u : 0xffffffffu,
                                    dark ? 0xff2b3c52u : 0xffeef5fcu,
                                    dark ? 0xff1d2b3eu : 0xffcfe0f3u,
                                    dark ? 0xff25354au : 0xffe1ecfau,
                                    focused ? 0xff3c7fb1u : (dark ? 0xff5a7494u : 0xff6e849eu),
                                    dark ? 0x40ffffffu : 0xd8ffffffu);
            } else {
                cb.fill_rect(cb.user_data, kx, ky, kw, kh, win98 ? 0xffc0c0c0u : 0xffffffffu, win98 ? nullptr : kr);
                if (win98) draw_win98_bevel(cb, kx, ky, kw, kh, false);
                else draw_box_border_uniform(cb, kx, ky, kw, kh, 0xff2563ebu, 1, 6);
            }
        }
        return;
    }

    if (is_button_like()) {
        uint32_t bg = web_color_to_argb(css().get_bg().m_color);
        if (vortex && (bg >> 24) == 0) {
            if (active) {
                draw_aero_gloss_box(cb, px, py, pw, ph, rad,
                                    dark ? 0xff1d3d61u : 0xffe5f4fcu,
                                    dark ? 0xff183554u : 0xffc4e5f6u,
                                    dark ? 0xff112842u : 0xff98d1efu,
                                    dark ? 0xff163150u : 0xffaddcf6u,
                                    0xff2c628bu, dark ? 0x35ffffffu : 0xd0ffffffu);
            } else if (dark) {
                draw_aero_gloss_box(cb, px, py, pw, ph, rad,
                                    0xff324156u, 0xff263346u, 0xff1a2433u, 0xff222e40u,
                                    focused ? 0xff4da6e8u : 0xff4a5d75u, 0x30ffffffu);
            } else {
                draw_aero_gloss_box(cb, px, py, pw, ph, rad,
                                    0xfffefeffu, 0xffeff3f8u, 0xffdbe3eeu, 0xffeaedf5u,
                                    focused ? 0xff3c7fb1u : 0xff707070u, 0xd8ffffffu);
            }
        } else {
            if ((bg >> 24) == 0) {
                bg = win98 ? 0xffc0c0c0u : (active ? 0xff1d4ed8u : (dark ? 0xff374151u : 0xffe2e8f0u));
            }
            if (cb.fill_rect) cb.fill_rect(cb.user_data, px, py, pw, ph, bg, win98 ? nullptr : radii);
            if (win98) {
                draw_win98_bevel(cb, px, py, pw, ph, active);
            } else if (!css().get_borders().is_visible()) {
                draw_box_border_uniform(cb, px, py, pw, ph, focused ? 0xff3b82f6u : (dark ? 0xff4b5563u : 0xff94a3b8u), 1, rad);
            }
        }
        std::string label = display_text();
        uint32_t fg = web_color_to_argb(css().get_color());
        if ((fg >> 24) == 0 || (win98 && fg == 0xff000000u)) {
            fg = win98 ? 0xff000000u : (dark ? 0xfff8fafcu : 0xff0f172au);
        }
        int tw = cb.text_width ? cb.text_width(cb.user_data, label.c_str(), fh) : static_cast<int>(label.size() * 8);
        int tx = px + std::max(4, (pw - tw) / 2) + ((win98 || vortex) && active ? 1 : 0);
        int ty = py + std::max(2, (ph - fh_px) / 2) + ((win98 || vortex) && active ? 1 : 0);
        if (cb.draw_text && !label.empty()) {
            cb.draw_text(cb.user_data, label.c_str(), fh, fg, tx, ty, pw - 8, ph - 4, 0);
        }
        return;
    }

    // Text-like inputs
    uint32_t bg = web_color_to_argb(css().get_bg().m_color);
    if ((bg >> 24) == 0) {
        bg = win98 ? 0xffffffffu : (vortex ? (dark ? 0xff16202eu : 0xffffffffu) : (dark ? 0xff1e2430u : 0xffffffffu));
    }
    if (cb.fill_rect) cb.fill_rect(cb.user_data, px, py, pw, ph, bg, win98 ? nullptr : radii);
    if (win98) {
        draw_win98_bevel(cb, px, py, pw, ph, true);
    } else if (vortex) {
        uint32_t bcol = focused ? (dark ? 0xff52a6ebu : 0xff3d7badu) : (dark ? 0xff465a73u : 0xffabadb3u);
        if (!css().get_borders().is_visible() || focused) {
            draw_box_border_uniform(cb, px, py, pw, ph, bcol, focused ? 2 : 1, rad);
        }
        if (cb.fill_rect && pw > 4) {
            cb.fill_rect(cb.user_data, px + 2, py + 1, pw - 4, 1, dark ? 0x20000000u : 0x18000000u, nullptr);
        }
    } else {
        uint32_t bcol = focused ? 0xff3b82f6u : (dark ? 0xff475569u : 0xffcbd5e1u);
        if (!css().get_borders().is_visible() || focused) {
            draw_box_border_uniform(cb, px, py, pw, ph, bcol, focused ? 2 : 1, rad);
        }
    }

    std::string disp = display_text();
    const char* ph_attr = get_attr("placeholder");
    bool showing_placeholder = disp.empty() && ph_attr && *ph_attr;
    if (showing_placeholder) disp = ph_attr;

    uint32_t fg = web_color_to_argb(css().get_color());
    if (showing_placeholder) fg = dark ? 0xff64748bu : 0xff94a3b8u;
    else if ((fg >> 24) == 0) fg = win98 ? 0xff000000u : (dark ? 0xfff1f5f9u : 0xff0f172au);

    int pad_x = 6;
    int inner_w = std::max(1, pw - pad_x * 2);
    std::string before_caret = display_text().substr(0, std::min(caret_, display_text().size()));
    int caret_px = cb.text_width ? cb.text_width(cb.user_data, before_caret.c_str(), fh)
                                 : static_cast<int>(before_caret.size() * 8);
    int scroll_text_x = 0;
    if (caret_px > inner_w - 4) {
        scroll_text_x = caret_px - (inner_w - 4);
    }

    if (cb.set_clip) cb.set_clip(cb.user_data, px + 3, py + 2, std::max(1, pw - 6), std::max(1, ph - 4), nullptr);
    int ty = py + std::max(2, (ph - fh_px) / 2);
    if (cb.draw_text && !disp.empty()) {
        cb.draw_text(cb.user_data, disp.c_str(), fh, fg, px + pad_x - scroll_text_x, ty,
                     inner_w + scroll_text_x, ph - 4, 0);
    }
    if (focused && cb.fill_rect) {
        bool blink_on = (static_cast<int>(page_->now_ms / 500.0) % 2) == 0;
        if (blink_on) {
            int cx = px + pad_x + caret_px - scroll_text_x;
            cb.fill_rect(cb.user_data, cx, py + 4, 1, std::max(8, ph - 8),
                         win98 ? 0xff000000u : (dark ? 0xfff8fafcu : 0xff0f172au), nullptr);
        }
    }
    if (cb.del_clip) cb.del_clip(cb.user_data);
}

void el_space_input::on_mouse_click(int rel_x, int /*rel_y*/) {
    if (get_attr("disabled")) return;
    std::string t = input_type();
    if (t == "checkbox") {
        set_checked(!checked_, false);
        if (page_ && page_->dom) {
            auto self = shared_from_this();
            page_->dom->dispatch_ui_event(self, "input", true, true);
            page_->dom->dispatch_ui_event(self, "change", true, true);
        }
        return;
    }
    if (t == "radio") {
        if (!checked_) {
            set_checked(true, true);
            if (page_ && page_->dom) {
                auto self = shared_from_this();
                page_->dom->dispatch_ui_event(self, "input", true, true);
                page_->dom->dispatch_ui_event(self, "change", true, true);
            }
        }
        return;
    }
    if (t == "range") {
        auto placement = get_placement();
        int w = std::max(1, static_cast<int>(placement.width.value()));
        double ratio = std::clamp(static_cast<double>(rel_x - 4) / std::max(1, w - 8), 0.0, 1.0);
        double minv = get_attr("min") ? std::atof(get_attr("min")) : 0.0;
        double maxv = get_attr("max") ? std::atof(get_attr("max")) : 100.0;
        double nv = minv + ratio * (maxv - minv);
        char buf[64];
        std::snprintf(buf, sizeof(buf), "%g", std::round(nv));
        set_current_value(buf, true);
        if (page_ && page_->dom) {
            auto self = shared_from_this();
            page_->dom->dispatch_ui_event(self, "input", true, true);
            page_->dom->dispatch_ui_event(self, "change", true, true);
        }
        return;
    }
    if (t == "submit") {
        if (page_) page_->submit_form_for(shared_from_this());
        return;
    }
    if (t == "reset") {
        // Walk up to form and clear controls
        auto p = parent();
        while (p && (!p->get_tagName() || std::strcmp(p->get_tagName(), "form") != 0)) {
            p = p->parent();
        }
        if (p) {
            std::vector<litehtml::element::ptr> stk = {p};
            while (!stk.empty()) {
                auto cur = stk.back();
                stk.pop_back();
                if (auto* inp = dynamic_cast<el_space_input*>(cur.get())) {
                    inp->value_dirty_ = false;
                    inp->checked_dirty_ = false;
                    inp->parse_attributes();
                } else if (auto* ta = dynamic_cast<el_space_textarea*>(cur.get())) {
                    ta->set_current_value("");
                }
                for (const auto& ch : cur->children()) stk.push_back(ch);
            }
            if (page_) page_->mark_dirty(false, true);
        }
        return;
    }
    if (t == "button") return;

    // Text-like: position caret roughly
    if (page_ && page_->cb.text_width) {
        int fh_px = 16;
        litehtml::uint_ptr fh = pick_font(this, page_, fh_px);
        std::string disp = display_text();
        size_t best = disp.size();
        int best_dist = 1000000;
        size_t idx = 0;
        while (true) {
            std::string sub = disp.substr(0, idx);
            int w = page_->cb.text_width(page_->cb.user_data, sub.c_str(), fh);
            int d = std::abs((w + 6) - rel_x);
            if (d < best_dist) {
                best_dist = d;
                best = idx;
            }
            if (idx >= disp.size()) break;
            idx = next_utf8_boundary(disp, idx);
        }
        caret_ = best;
        page_->mark_dirty(false, true);
    } else {
        caret_ = value_.size();
    }
}

bool el_space_input::on_key(int key_code, const char* utf8_text, int mods) {
    if (get_attr("disabled") || get_attr("readonly")) return false;
    std::string t = input_type();
    if (t == "checkbox" || t == "radio") {
        if (key_code == 0x0020 || key_code == 0xff0d) {
            on_mouse_click(8, 8);
            return true;
        }
        return false;
    }
    if (is_button_like()) {
        if (key_code == 0xff0d || key_code == 0x0020) {
            on_mouse_click(8, 8);
            return true;
        }
        return false;
    }
    if (!is_text_like()) return false;

    bool changed = false;
    if (key_code == 0xff08) { // BackSpace
        if (caret_ > 0) {
            size_t prev = prev_utf8_boundary(value_, caret_);
            value_.erase(prev, caret_ - prev);
            caret_ = prev;
            value_dirty_ = true;
            changed = true;
        }
    } else if (key_code == 0xffff) { // Delete
        if (caret_ < value_.size()) {
            size_t nxt = next_utf8_boundary(value_, caret_);
            value_.erase(caret_, nxt - caret_);
            value_dirty_ = true;
            changed = true;
        }
    } else if (key_code == 0xff51) { // Left
        caret_ = prev_utf8_boundary(value_, caret_);
        if (page_) page_->mark_dirty(false, true);
        return true;
    } else if (key_code == 0xff53) { // Right
        caret_ = next_utf8_boundary(value_, caret_);
        if (page_) page_->mark_dirty(false, true);
        return true;
    } else if (key_code == 0xff50) { // Home
        caret_ = 0;
        if (page_) page_->mark_dirty(false, true);
        return true;
    } else if (key_code == 0xff57) { // End
        caret_ = value_.size();
        if (page_) page_->mark_dirty(false, true);
        return true;
    } else if (key_code == 0xff0d || key_code == 0xff8d) { // Enter
        if (page_) page_->submit_form_for(shared_from_this());
        return true;
    } else if ((mods & 4) == 0 && utf8_text && utf8_text[0] &&
               static_cast<unsigned char>(utf8_text[0]) >= 0x20 && utf8_text[0] != 0x7f) {
        size_t len = std::strlen(utf8_text);
        value_.insert(caret_, utf8_text, len);
        caret_ += len;
        value_dirty_ = true;
        changed = true;
    }

    if (changed) {
        if (page_) page_->mark_dirty(false, true);
        if (page_ && page_->dom) {
            auto self = shared_from_this();
            page_->dom->dispatch_ui_event(self, "input", true, true);
            page_->dom->dispatch_ui_event(self, "change", true, true);
        }
        return true;
    }
    return false;
}

std::shared_ptr<litehtml::render_item> el_space_input::create_render_item(
    const std::shared_ptr<litehtml::render_item>& /*parent_ri*/) {
    if (is_hidden()) {
        return litehtml::html_tag::create_render_item(nullptr);
    }
    return std::make_shared<litehtml::render_item_image>(shared_from_this());
}

// =========================================================================
// <textarea>
// =========================================================================

el_space_textarea::el_space_textarea(const std::shared_ptr<litehtml::document>& doc, SpacePage* page)
    : litehtml::html_tag(doc), page_(page) {}

void el_space_textarea::parse_attributes() {
    html_tag::parse_attributes();
    if (!value_dirty_) {
        std::string txt;
        get_text(txt);
        value_ = txt;
        caret_ = value_.size();
    }
}

const std::string& el_space_textarea::current_value() const {
    if (!value_dirty_ && value_.empty()) {
        std::string txt;
        get_text(txt);
        if (!txt.empty()) {
            value_ = txt;
        }
    }
    return value_;
}

void el_space_textarea::set_current_value(const std::string& v) {
    value_ = v;
    value_dirty_ = true;
    caret_ = std::min(caret_, value_.size());
    if (page_) page_->mark_dirty(false, true);
}

void el_space_textarea::get_content_size(litehtml::size& sz, litehtml::pixel_t /*max_width*/) {
    int cols = get_attr("cols") ? std::max(8, std::atoi(get_attr("cols"))) : 28;
    int rows = get_attr("rows") ? std::max(2, std::atoi(get_attr("rows"))) : 4;
    sz.width = static_cast<float>(cols * 8 + 12);
    sz.height = static_cast<float>(rows * 18 + 10);
}

void el_space_textarea::draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
                             const litehtml::position* clip,
                             const std::shared_ptr<litehtml::render_item>& ri) {
    html_tag::draw(hdc, x, y, clip, ri);
    if (!page_) return;

    litehtml::position pos = ri->pos();
    pos.x += x;
    pos.y += y;
    int px = static_cast<int>(std::round(pos.x.value()));
    int py = static_cast<int>(std::round(pos.y.value()));
    int pw = static_cast<int>(std::round(pos.width.value()));
    int ph = static_cast<int>(std::round(pos.height.value()));
    if (pw <= 0 || ph <= 0) return;

    const auto& cb = page_->cb;
    const bool focused = is_focused_el(page_, this);
    const bool dark = page_->dark_mode;
    const bool win98 = page_->is_win98;
    const bool vortex = page_->is_vortex;
    const int rad = win98 ? 0 : (vortex ? 5 : 6);
    int radii[8] = {rad, rad, rad, rad, rad, rad, rad, rad};

    uint32_t bg = web_color_to_argb(css().get_bg().m_color);
    const bool author_bg = (bg >> 24) != 0;
    if (!author_bg) {
        if (win98) bg = 0xffffffffu;
        else if (vortex) bg = dark ? 0xff16202eu : 0xffffffffu;
        else bg = dark ? 0xff1e2430u : 0xffffffffu;
    }
    if (cb.fill_rect) cb.fill_rect(cb.user_data, px, py, pw, ph, bg, win98 ? nullptr : radii);
    if (win98) {
        draw_win98_bevel(cb, px, py, pw, ph, true);
    } else if (vortex && (!css().get_borders().is_visible() || focused)) {
        if (!author_bg && cb.fill_rect && pw > 4 && ph > 4) {
            int srad[8] = {std::max(0, rad - 1), std::max(0, rad - 1), 0, 0, 0, 0, 0, 0};
            cb.fill_rect(cb.user_data, px + 1, py + 1, pw - 2, 2,
                         dark ? 0x33000000u : 0x226888aau, srad);
        }
        uint32_t brd = focused ? 0xff3b94e6u : (dark ? 0xff49627eu : 0xff8ea6c2u);
        draw_box_border_uniform(cb, px, py, pw, ph, brd, focused ? 2 : 1, rad);
        if (pw > 4 && ph > 4) {
            draw_box_border_uniform(cb, px + 1, py + 1, pw - 2, ph - 2,
                                    focused ? (dark ? 0x555cb8ffu : 0x5570bfffu)
                                            : (dark ? 0x2279a6d2u : 0x66ffffffu),
                                    1, std::max(0, rad - 1));
        }
    } else if (!css().get_borders().is_visible() || focused) {
        draw_box_border_uniform(cb, px, py, pw, ph, focused ? 0xff3b82f6u : (dark ? 0xff475569u : 0xffcbd5e1u),
                                focused ? 2 : 1, rad);
    }

    int fh_px = 16;
    litehtml::uint_ptr fh = pick_font(this, page_, fh_px);
    const std::string& val = current_value();
    const char* ph_attr = get_attr("placeholder");
    bool show_ph = val.empty() && ph_attr && *ph_attr;
    const std::string text_to_draw = show_ph ? std::string(ph_attr) : val;
    uint32_t fg = show_ph ? (dark ? 0xff64748bu : 0xff94a3b8u)
                          : (web_color_to_argb(css().get_color()));
    if (!show_ph && (fg >> 24) == 0) fg = win98 ? 0xff000000u : (dark ? 0xfff1f5f9u : 0xff0f172au);

    if (cb.set_clip) cb.set_clip(cb.user_data, px + 4, py + 4, std::max(1, pw - 8), std::max(1, ph - 8), nullptr);
    int line_y = py + 6;
    int line_step = std::max(16, fh_px + 2);
    size_t start = 0;
    size_t caret_drawn = false;
    while (start <= text_to_draw.size()) {
        size_t nl = text_to_draw.find('\n', start);
        size_t end = (nl == std::string::npos) ? text_to_draw.size() : nl;
        std::string line = text_to_draw.substr(start, end - start);
        if (!line.empty() && cb.draw_text) {
            cb.draw_text(cb.user_data, line.c_str(), fh, fg, px + 6, line_y, pw - 12, line_step, 0);
        }
        if (focused && !show_ph && !caret_drawn && caret_ >= start && caret_ <= end && cb.fill_rect) {
            caret_drawn = true;
            if ((static_cast<int>(page_->now_ms / 500.0) % 2) == 0) {
                std::string before = text_to_draw.substr(start, caret_ - start);
                int cw = cb.text_width ? cb.text_width(cb.user_data, before.c_str(), fh)
                                       : static_cast<int>(before.size() * 8);
                cb.fill_rect(cb.user_data, px + 6 + cw, line_y, 1, std::max(12, fh_px),
                             win98 ? 0xff000000u : (dark ? 0xfff8fafcu : 0xff0f172au), nullptr);
            }
        }
        line_y += line_step;
        if (nl == std::string::npos || line_y > py + ph) break;
        start = nl + 1;
    }
    if (focused && show_ph && cb.fill_rect && (static_cast<int>(page_->now_ms / 500.0) % 2) == 0) {
        cb.fill_rect(cb.user_data, px + 6, py + 6, 1, std::max(12, fh_px),
                     win98 ? 0xff000000u : (dark ? 0xfff8fafcu : 0xff0f172au), nullptr);
    }
    if (cb.del_clip) cb.del_clip(cb.user_data);
}

void el_space_textarea::on_mouse_click(int /*rel_x*/, int /*rel_y*/) {
    caret_ = current_value().size();
    if (page_) page_->mark_dirty(false, true);
}

bool el_space_textarea::on_key(int key_code, const char* utf8_text, int mods) {
    if (get_attr("disabled") || get_attr("readonly")) return false;
    std::string& v = value_;
    if (!value_dirty_) v = current_value();
    bool changed = false;

    if (key_code == 0xff08) { // BackSpace
        if (caret_ > 0) {
            size_t prev = prev_utf8_boundary(v, caret_);
            v.erase(prev, caret_ - prev);
            caret_ = prev;
            value_dirty_ = true;
            changed = true;
        }
    } else if (key_code == 0xffff) {
        if (caret_ < v.size()) {
            size_t nxt = next_utf8_boundary(v, caret_);
            v.erase(caret_, nxt - caret_);
            value_dirty_ = true;
            changed = true;
        }
    } else if (key_code == 0xff51) {
        caret_ = prev_utf8_boundary(v, caret_);
        if (page_) page_->mark_dirty(false, true);
        return true;
    } else if (key_code == 0xff53) {
        caret_ = next_utf8_boundary(v, caret_);
        if (page_) page_->mark_dirty(false, true);
        return true;
    } else if (key_code == 0xff50) {
        caret_ = 0;
        if (page_) page_->mark_dirty(false, true);
        return true;
    } else if (key_code == 0xff57) {
        caret_ = v.size();
        if (page_) page_->mark_dirty(false, true);
        return true;
    } else if (key_code == 0xff0d || key_code == 0xff8d) {
        v.insert(caret_, 1, '\n');
        caret_ += 1;
        value_dirty_ = true;
        changed = true;
    } else if ((mods & 4) == 0 && utf8_text && utf8_text[0] &&
               static_cast<unsigned char>(utf8_text[0]) >= 0x20 && utf8_text[0] != 0x7f) {
        size_t len = std::strlen(utf8_text);
        v.insert(caret_, utf8_text, len);
        caret_ += len;
        value_dirty_ = true;
        changed = true;
    }

    if (changed) {
        if (page_) page_->mark_dirty(false, true);
        if (page_ && page_->dom) {
            auto self = shared_from_this();
            page_->dom->dispatch_ui_event(self, "input", true, true);
            page_->dom->dispatch_ui_event(self, "change", true, true);
        }
        return true;
    }
    return false;
}

std::shared_ptr<litehtml::render_item> el_space_textarea::create_render_item(
    const std::shared_ptr<litehtml::render_item>& /*parent_ri*/) {
    return std::make_shared<litehtml::render_item_image>(shared_from_this());
}

// =========================================================================
// <button>
// =========================================================================

el_space_button::el_space_button(const std::shared_ptr<litehtml::document>& doc, SpacePage* page)
    : litehtml::html_tag(doc), page_(page) {}

void el_space_button::parse_attributes() {
    html_tag::parse_attributes();
}

void el_space_button::draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
                           const litehtml::position* clip,
                           const std::shared_ptr<litehtml::render_item>& ri) {
    html_tag::draw(hdc, x, y, clip, ri);
    if (!page_ || (!page_->is_win98 && !page_->is_vortex)) return;
    if (!css().get_borders().is_visible()) {
        litehtml::position pos = ri->pos();
        pos.x += x - ri->padding_left() - ri->border_left();
        pos.y += y - ri->padding_top() - ri->border_top();
        int px = static_cast<int>(std::round(pos.x.value()));
        int py = static_cast<int>(std::round(pos.y.value()));
        int pw = static_cast<int>(std::round(ri->content_width().value()));
        int ph = static_cast<int>(std::round(ri->content_height().value()));
        if (page_->is_win98) {
            draw_win98_bevel(page_->cb, px, py, pw, ph, is_active_el(page_, this));
        } else {
            const bool active = is_active_el(page_, this);
            const bool focused = is_focused_el(page_, this);
            const bool dark = page_->dark_mode;
            // Semi-transparent Win7 glass sheen preserves html_tag text drawn above.
            uint32_t c0 = active ? 0x1e6088b0u : (dark ? 0x2690bce8u : 0x44ffffffu);
            uint32_t c1 = active ? 0x1278a4ccu : (dark ? 0x105882b0u : 0x1cffffffu);
            uint32_t c2 = active ? 0x18386088u : (dark ? 0x1c000000u : 0x146a88a8u);
            uint32_t c3 = active ? 0x1688b8e0u : (dark ? 0x185686b8u : 0x28ffffffu);
            uint32_t brd = focused ? 0xff3b94e6u : (dark ? 0xff5a7594u : 0xff8ea6c2u);
            uint32_t rim = dark ? 0x55a8d4ffu : 0x99ffffffu;
            draw_aero_gloss_box(page_->cb, px, py, pw, ph, 5, c0, c1, c2, c3, brd, rim);
        }
    }
}

void el_space_button::on_click() {
    html_tag::on_click();
    if (get_attr("disabled") || !page_) return;
    const char* t = get_attr("type");
    std::string type = t ? to_lower_ascii(t) : "submit";
    if (type == "submit") {
        page_->submit_form_for(shared_from_this());
    }
}

std::string el_space_button::current_value() const {
    if (const char* v = get_attr("value")) return v;
    std::string t;
    get_text(t);
    return t;
}

void el_space_button::set_current_value(const std::string& v) {
    set_attr("value", v.c_str());
}

// =========================================================================
// <select>
// =========================================================================

el_space_select::el_space_select(const std::shared_ptr<litehtml::document>& doc, SpacePage* page)
    : litehtml::html_tag(doc), page_(page) {}

void el_space_select::parse_attributes() {
    html_tag::parse_attributes();
    init_selected_ = false;
    rebuild_options();
}

void el_space_select::rebuild_options() const {
    options_.clear();
    int first_sel = -1;
    std::vector<litehtml::element::ptr> stk;
    for (const auto& ch : static_cast<const litehtml::element*>(this)->children()) stk.push_back(ch);
    for (size_t i = 0; i < stk.size(); ++i) {
        const auto& el = stk[i];
        const char* tag = el->get_tagName();
        if (tag && std::strcmp(tag, "option") == 0) {
            std::string label;
            el->get_text(label);
            label = trim_ascii(label);
            const char* val_attr = el->get_attr("value");
            std::string val = val_attr ? std::string(val_attr) : label;
            bool disabled = (el->get_attr("disabled") != nullptr);
            if (el->get_attr("selected") != nullptr && first_sel < 0) {
                first_sel = static_cast<int>(options_.size());
            }
            options_.push_back({val, label, disabled});
        } else if (tag && std::strcmp(tag, "optgroup") == 0) {
            for (const auto& gc : el->children()) stk.push_back(gc);
        }
    }
    if (!init_selected_) {
        init_selected_ = true;
        if (first_sel >= 0) selected_index_ = first_sel;
        else if (!options_.empty()) selected_index_ = 0;
        else selected_index_ = -1;
    } else if (selected_index_ >= static_cast<int>(options_.size())) {
        selected_index_ = options_.empty() ? -1 : 0;
    }
}

const std::vector<SelectOptionItem>& el_space_select::options() const {
    rebuild_options();
    return options_;
}

int el_space_select::selected_index() const {
    rebuild_options();
    return selected_index_;
}

void el_space_select::set_selected_index(int idx) {
    rebuild_options();
    if (idx >= -1 && idx < static_cast<int>(options_.size())) {
        selected_index_ = idx;
        init_selected_ = true;
        if (page_) page_->mark_dirty(false, true);
    }
}

std::string el_space_select::current_value() const {
    rebuild_options();
    if (selected_index_ >= 0 && selected_index_ < static_cast<int>(options_.size())) {
        return options_[selected_index_].value;
    }
    return "";
}

void el_space_select::set_current_value(const std::string& v) {
    rebuild_options();
    for (size_t i = 0; i < options_.size(); ++i) {
        if (options_[i].value == v) {
            selected_index_ = static_cast<int>(i);
            init_selected_ = true;
            if (page_) page_->mark_dirty(false, true);
            return;
        }
    }
}

void el_space_select::get_content_size(litehtml::size& sz, litehtml::pixel_t /*max_width*/) {
    rebuild_options();
    int fh_px = 16;
    litehtml::uint_ptr fh = pick_font(this, page_, fh_px);
    int max_w = 80;
    for (const auto& opt : options_) {
        int tw = (page_ && page_->cb.text_width)
                     ? page_->cb.text_width(page_->cb.user_data, opt.label.c_str(), fh)
                     : static_cast<int>(opt.label.size() * 8);
        max_w = std::max(max_w, tw + 34);
    }
    sz.width = static_cast<float>(max_w);
    sz.height = static_cast<float>(std::max(24, fh_px + 8));
}

void el_space_select::draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
                           const litehtml::position* clip,
                           const std::shared_ptr<litehtml::render_item>& ri) {
    html_tag::draw(hdc, x, y, clip, ri);
    if (!page_) return;
    rebuild_options();

    litehtml::position pos = ri->pos();
    pos.x += x;
    pos.y += y;
    int px = static_cast<int>(std::round(pos.x.value()));
    int py = static_cast<int>(std::round(pos.y.value()));
    int pw = static_cast<int>(std::round(pos.width.value()));
    int ph = static_cast<int>(std::round(pos.height.value()));
    if (pw <= 0 || ph <= 0) return;

    const auto& cb = page_->cb;
    const bool focused = is_focused_el(page_, this);
    const bool dark = page_->dark_mode;
    const bool win98 = page_->is_win98;
    const bool vortex = page_->is_vortex;
    const int rad = win98 ? 0 : (vortex ? 5 : 5);
    int radii[8] = {rad, rad, rad, rad, rad, rad, rad, rad};

    uint32_t bg = web_color_to_argb(css().get_bg().m_color);
    const bool author_bg = (bg >> 24) != 0;
    if (!author_bg) {
        if (win98) bg = 0xffffffffu;
        else if (vortex) bg = dark ? 0xff162334u : 0xffeef6ffu;
        else bg = dark ? 0xff1e2430u : 0xffffffffu;
    }

    if (win98) {
        if (cb.fill_rect) cb.fill_rect(cb.user_data, px, py, pw, ph, bg, nullptr);
        draw_win98_bevel(cb, px, py, pw, ph, true);
        int bw = std::min(18, pw - 4);
        if (cb.fill_rect) cb.fill_rect(cb.user_data, px + pw - bw - 2, py + 2, bw, ph - 4, 0xffc0c0c0u, nullptr);
        draw_win98_bevel(cb, px + pw - bw - 2, py + 2, bw, ph - 4, false);
    } else if (vortex) {
        uint32_t brd = focused ? 0xff3b94e6u : (dark ? 0xff526e8eu : 0xff8ea6c2u);
        uint32_t rim = dark ? 0x449ac5f0u : 0x99ffffffu;
        if (!author_bg) {
            uint32_t c0 = dark ? 0xff2a3c52u : 0xffffffffu;
            uint32_t c1 = dark ? 0xff1f2e42u : 0xfff0f6fdu;
            uint32_t c2 = dark ? 0xff162334u : 0xffe0ecf9u;
            uint32_t c3 = dark ? 0xff21334au : 0xffeef6ffu;
            draw_aero_gloss_box(cb, px, py, pw, ph, rad, c0, c1, c2, c3, brd, rim);
        } else {
            if (cb.fill_rect) cb.fill_rect(cb.user_data, px, py, pw, ph, bg, radii);
            draw_box_border_uniform(cb, px, py, pw, ph, brd, focused ? 2 : 1, rad);
        }
        int bw = std::min(20, pw - 6);
        if (bw > 6 && ph > 6 && cb.fill_rect) {
            int btn_x = px + pw - bw - 2;
            cb.fill_rect(cb.user_data, btn_x - 1, py + 3, 1, ph - 6,
                         dark ? 0x557094b8u : 0x557fa0c4u, nullptr);
            int br[8] = {0, std::max(0, rad - 1), std::max(0, rad - 1), 0, 0, 0, 0, 0};
            cb.fill_rect(cb.user_data, btn_x, py + 2, bw, (ph - 4) / 2,
                         dark ? 0x2690bce8u : 0x66ffffffu, br);
        }
        if (focused) {
            draw_box_border_uniform(cb, px, py, pw, ph, 0xff3b94e6u, 2, rad);
        }
    } else {
        if (cb.fill_rect) cb.fill_rect(cb.user_data, px, py, pw, ph, bg, radii);
        draw_box_border_uniform(cb, px, py, pw, ph, focused ? 0xff3b82f6u : (dark ? 0xff475569u : 0xffcbd5e1u),
                                focused ? 2 : 1, rad);
    }

    int fh_px = 16;
    litehtml::uint_ptr fh = pick_font(this, page_, fh_px);
    uint32_t fg = web_color_to_argb(css().get_color());
    if ((fg >> 24) == 0) fg = win98 ? 0xff000000u : (dark ? 0xfff1f5f9u : 0xff0f172au);

    std::string label;
    if (selected_index_ >= 0 && selected_index_ < static_cast<int>(options_.size())) {
        label = options_[selected_index_].label;
    }
    int ty = py + std::max(2, (ph - fh_px) / 2);
    if (cb.draw_text && !label.empty()) {
        cb.draw_text(cb.user_data, label.c_str(), fh, fg, px + 7, ty, std::max(1, pw - 28), ph - 4, 0);
    }
    if (cb.draw_text) {
        cb.draw_text(cb.user_data, "v", fh, fg, px + pw - 16, ty, 14, ph - 4, 0);
    }
}

void el_space_select::on_mouse_click(const litehtml::position& abs_pos) {
    if (get_attr("disabled") || !page_ || !page_->cb.on_select_popup || !page_->dom) return;
    rebuild_options();
    if (options_.empty()) return;
    std::string items_tsv;
    for (size_t i = 0; i < options_.size(); ++i) {
        if (i > 0) items_tsv.push_back('\n');
        for (char ch : options_[i].label) {
            items_tsv.push_back(ch == '\n' ? ' ' : ch);
        }
    }
    uint64_t nid = page_->dom->intern_node(shared_from_this());
    page_->cb.on_select_popup(
        page_->cb.user_data, nid,
        static_cast<int>(abs_pos.x.value()) - page_->scroll_x,
        static_cast<int>(abs_pos.y.value()) - page_->scroll_y,
        static_cast<int>(abs_pos.width.value()),
        static_cast<int>(abs_pos.height.value()),
        items_tsv.c_str(), selected_index_);
}

std::shared_ptr<litehtml::render_item> el_space_select::create_render_item(
    const std::shared_ptr<litehtml::render_item>& /*parent_ri*/) {
    return std::make_shared<litehtml::render_item_image>(shared_from_this());
}

// =========================================================================
// <canvas>
// =========================================================================

el_space_canvas::el_space_canvas(const std::shared_ptr<litehtml::document>& doc, SpacePage* page)
    : litehtml::html_tag(doc), page_(page) {}

void el_space_canvas::parse_attributes() {
    int w = 300;
    int h = 150;
    if (const char* wa = get_attr("width")) {
        int pw = std::atoi(wa);
        if (pw > 0 && pw <= 4096) w = pw;
    }
    if (const char* ha = get_attr("height")) {
        int ph = std::atoi(ha);
        if (ph > 0 && ph <= 4096) h = ph;
    }
    if (w != canvas_.width || h != canvas_.height) {
        canvas_.resize(w, h);
    }
    html_tag::parse_attributes();
}

void el_space_canvas::resize_canvas(int w, int h) {
    w = std::clamp(w, 1, 4096);
    h = std::clamp(h, 1, 4096);
    canvas_.resize(w, h);
    if (page_) page_->mark_dirty(true, true);
}

void el_space_canvas::get_content_size(litehtml::size& sz, litehtml::pixel_t /*max_width*/) {
    sz.width = static_cast<float>(canvas_.width);
    sz.height = static_cast<float>(canvas_.height);
}

void el_space_canvas::draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
                           const litehtml::position* clip,
                           const std::shared_ptr<litehtml::render_item>& ri) {
    html_tag::draw(hdc, x, y, clip, ri);
    if (!page_) return;

    litehtml::position pos = ri->pos();
    pos.x += x;
    pos.y += y;
    int px = static_cast<int>(std::round(pos.x.value()));
    int py = static_cast<int>(std::round(pos.y.value()));
    int pw = static_cast<int>(std::round(pos.width.value()));
    int ph = static_cast<int>(std::round(pos.height.value()));
    if (pw <= 0 || ph <= 0) return;

    const auto& cb = page_->cb;
    if (cb.draw_rgba_blit && !canvas_.pixels.empty()) {
        cb.draw_rgba_blit(cb.user_data, canvas_.pixels.data(), canvas_.width, canvas_.height, px, py, pw, ph);
    }

    if (!canvas_.text_cmds.empty() && cb.create_font && cb.draw_text && cb.delete_font) {
        double sx = static_cast<double>(pw) / std::max(1, canvas_.width);
        double sy = static_cast<double>(ph) / std::max(1, canvas_.height);
        if (cb.set_clip) cb.set_clip(cb.user_data, px, py, pw, ph, nullptr);
        for (const auto& cmd : canvas_.text_cmds) {
            int fsz = std::max(6, static_cast<int>(std::round(cmd.size * sy)));
            int h_out = fsz, a_out = fsz, d_out = 0, x_out = fsz / 2;
            uint64_t fh = cb.create_font(cb.user_data, cmd.family.c_str(), fsz, cmd.weight,
                                         cmd.italic ? 1 : 0, 0, &h_out, &a_out, &d_out, &x_out);
            int tx = px + static_cast<int>(std::round(cmd.x * sx));
            int ty = py + static_cast<int>(std::round(cmd.y * sy));
            cb.draw_text(cb.user_data, cmd.text.c_str(), fh, cmd.argb, tx, ty, pw, h_out + 4, 0);
            cb.delete_font(cb.user_data, fh);
        }
        if (cb.del_clip) cb.del_clip(cb.user_data);
    }
}

std::shared_ptr<litehtml::render_item> el_space_canvas::create_render_item(
    const std::shared_ptr<litehtml::render_item>& /*parent_ri*/) {
    return std::make_shared<litehtml::render_item_image>(shared_from_this());
}

} // namespace space
