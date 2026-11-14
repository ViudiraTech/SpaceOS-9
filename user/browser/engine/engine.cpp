#include "container.hpp"
#include "dom_bridge.hpp"
#include "page_internal.hpp"
#include "widgets.hpp"
#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdio>
#include <cstring>

namespace space {

namespace {

static const char* kExtraUaCss = R"CSS(
input, textarea, select, button {
    display: inline-block;
    font-family: sans-serif;
    font-size: 14px;
    box-sizing: border-box;
    vertical-align: middle;
}
input[type="hidden"] {
    display: none;
}
button {
    padding: 5px 12px;
    border-radius: 5px;
    border: 1px solid #94a3b8;
    background-color: #e2e8f0;
    color: #0f172a;
    cursor: pointer;
}
canvas {
    display: inline-block;
    vertical-align: middle;
}
script, style, head, title, meta, link {
    display: none;
}
a {
    color: #2563eb;
    text-decoration: underline;
    cursor: pointer;
}
)CSS";

static const char* kDarkUaOverride = R"CSS(
html, body {
    color: #e2e8f0;
    background-color: #111622;
}
a {
    color: #60a5fa;
}
button {
    border: 1px solid #475569;
    background-color: #1e293b;
    color: #f1f5f9;
}
)CSS";

static const char* kWin98UaOverride = R"CSS(
html, body {
    color: #000000;
    background-color: #ffffff;
}
button {
    border-radius: 0px;
    background-color: #c0c0c0;
    color: #000000;
}
)CSS";

bool contains_ci(const std::string& hay, const std::string& needle) {
    if (needle.empty()) return false;
    std::string h = to_lower_ascii(hay);
    std::string n = to_lower_ascii(needle);
    return h.find(n) != std::string::npos;
}

} // namespace

std::string to_lower_ascii(std::string s) {
    for (char& ch : s) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    return s;
}

std::string trim_ascii(const std::string& s) {
    size_t b = 0;
    while (b < s.size() && std::isspace(static_cast<unsigned char>(s[b]))) ++b;
    size_t e = s.size();
    while (e > b && std::isspace(static_cast<unsigned char>(s[e - 1]))) --e;
    return s.substr(b, e - b);
}

std::string url_encode(const std::string& s) {
    static const char* kHex = "0123456789ABCDEF";
    std::string out;
    for (unsigned char c : s) {
        if (std::isalnum(c) || c == '-' || c == '_' || c == '.' || c == '~') {
            out.push_back(static_cast<char>(c));
        } else if (c == ' ') {
            out.push_back('+');
        } else {
            out.push_back('%');
            out.push_back(kHex[(c >> 4) & 0xf]);
            out.push_back(kHex[c & 0xf]);
        }
    }
    return out;
}

std::string resolve_url(const std::string& base_url, const std::string& ref_raw) {
    std::string ref = trim_ascii(ref_raw);
    if (ref.empty()) return base_url;
    // Absolute scheme?
    size_t colon = ref.find(':');
    if (colon != std::string::npos && colon > 0) {
        bool ok = std::isalpha(static_cast<unsigned char>(ref[0])) != 0;
        for (size_t i = 1; i < colon && ok; ++i) {
            unsigned char c = static_cast<unsigned char>(ref[i]);
            ok = std::isalnum(c) || c == '+' || c == '-' || c == '.';
        }
        if (ok) return ref;
    }

    size_t scheme_end = base_url.find("://");
    std::string scheme = (scheme_end != std::string::npos) ? base_url.substr(0, scheme_end) : "http";
    std::string after_scheme = (scheme_end != std::string::npos) ? base_url.substr(scheme_end + 3) : "";

    if (ref.rfind("//", 0) == 0) {
        return scheme + ":" + ref;
    }

    // Fragment-only
    if (ref[0] == '#') {
        size_t hash = base_url.find('#');
        return (hash == std::string::npos ? base_url : base_url.substr(0, hash)) + ref;
    }

    size_t slash_idx = after_scheme.find('/');
    std::string authority = (slash_idx == std::string::npos) ? after_scheme : after_scheme.substr(0, slash_idx);
    std::string base_path = (slash_idx == std::string::npos) ? "/" : after_scheme.substr(slash_idx);

    if (ref[0] == '/') {
        if (scheme_end != std::string::npos) {
            return scheme + "://" + authority + ref;
        }
        return ref;
    }

    if (ref[0] == '?') {
        size_t q = base_url.find('?');
        size_t h = base_url.find('#');
        size_t cut = std::min(q, h);
        return base_url.substr(0, cut) + ref;
    }

    // Relative path
    std::string clean_path = base_path;
    size_t hpos = clean_path.find('#');
    if (hpos != std::string::npos) clean_path = clean_path.substr(0, hpos);
    size_t qpos = clean_path.find('?');
    if (qpos != std::string::npos) clean_path = clean_path.substr(0, qpos);
    size_t last_sl = clean_path.rfind('/');
    std::string dir = (last_sl == std::string::npos) ? "/" : clean_path.substr(0, last_sl + 1);
    std::string combined = dir + ref;

    // Normalize dot segments for path part (keep query/hash untouched)
    std::string suffix;
    size_t sq = combined.find_first_of("?#");
    if (sq != std::string::npos) {
        suffix = combined.substr(sq);
        combined = combined.substr(0, sq);
    }
    std::vector<std::string> segs;
    size_t p = 0;
    while (p < combined.size()) {
        size_t nxt = combined.find('/', p);
        if (nxt == std::string::npos) nxt = combined.size();
        std::string seg = combined.substr(p, nxt - p);
        if (seg == "..") {
            if (!segs.empty()) segs.pop_back();
        } else if (!seg.empty() && seg != ".") {
            segs.push_back(seg);
        }
        p = nxt + 1;
    }
    std::string norm_path = "/";
    for (size_t i = 0; i < segs.size(); ++i) {
        if (i > 0) norm_path.push_back('/');
        norm_path += segs[i];
    }
    if (!combined.empty() && combined.back() == '/' && norm_path.back() != '/') norm_path.push_back('/');
    if (scheme_end != std::string::npos) {
        return scheme + "://" + authority + norm_path + suffix;
    }
    return norm_path + suffix;
}

uint32_t web_color_to_argb(const litehtml::web_color& c) {
    return (static_cast<uint32_t>(c.alpha) << 24) |
           (static_cast<uint32_t>(c.red) << 16) |
           (static_cast<uint32_t>(c.green) << 8) |
           static_cast<uint32_t>(c.blue);
}

litehtml::uint_ptr create_host_font(const SpaceHostCallbacks& cb, const char* family, int size_px,
                                    int weight, bool italic, int decoration,
                                    litehtml::font_metrics* out_fm) {
    int h = size_px + 2, asc = size_px, dsc = 3, xh = size_px / 2;
    uint64_t handle = 0;
    if (cb.create_font) {
        handle = cb.create_font(cb.user_data, family ? family : "sans-serif", size_px, weight,
                                italic ? 1 : 0, decoration, &h, &asc, &dsc, &xh);
    }
    if (out_fm) {
        out_fm->font_size = static_cast<float>(size_px);
        out_fm->height = static_cast<float>(h);
        out_fm->ascent = static_cast<float>(asc);
        out_fm->descent = static_cast<float>(dsc);
        out_fm->x_height = static_cast<float>(xh);
        out_fm->ch_width = static_cast<float>(std::max(6, size_px / 2));
        out_fm->draw_spaces = true;
    }
    return static_cast<litehtml::uint_ptr>(handle);
}

} // namespace space

SpacePage::SpacePage(const SpaceHostCallbacks* host_cb, int vw, int vh, bool dark, bool win98)
    : viewport_w(std::max(64, vw)),
      viewport_h(std::max(64, vh)),
      dark_mode(dark),
      is_win98(win98) {
    if (host_cb) cb = *host_cb;
    container = std::make_shared<space::SpaceContainer>(this);
    dom = std::make_unique<space::DomBridge>(this);
}

SpacePage::~SpacePage() {
    dom.reset();
    doc.reset();
    if (ui_font != 0 && cb.delete_font) {
        cb.delete_font(cb.user_data, ui_font);
        ui_font = 0;
    }
    container.reset();
}

void SpacePage::ensure_ui_font() {
    if (ui_font == 0 && cb.create_font) {
        ui_font = static_cast<uint64_t>(space::create_host_font(cb, "sans-serif", 14, 400, false, 0));
    }
}

void SpacePage::mark_dirty(bool relayout, bool repaint) {
    if (relayout) relayout_needed = true;
    if (repaint) repaint_needed = true;
    if (cb.on_dirty) {
        cb.on_dirty(cb.user_data, relayout ? 1 : 0, repaint ? 1 : 0);
    }
}

void SpacePage::ensure_restyle() {
    if (doc && restyle_needed) {
        restyle_needed = false;
        doc->restyle();
    }
}

void SpacePage::ensure_layout() {
    if (!doc) return;
    if (restyle_needed) {
        restyle_needed = false;
        doc->restyle();
        relayout_needed = true;
    }
    if (relayout_needed) {
        relayout_needed = false;
        doc->render(static_cast<litehtml::pixel_t>(viewport_w));
    }
}

void SpacePage::submit_form_for(const std::shared_ptr<litehtml::element>& control) {
    if (!control || !cb.on_navigate) return;
    auto form = control->parent();
    while (form && (!form->get_tagName() || space::to_lower_ascii(form->get_tagName()) != "form")) {
        form = form->parent();
    }
    if (!form) return;

    if (dom && dom->dispatch_ui_event(form, "submit", true, true)) {
        return; // prevented by JS!
    }

    const char* action = form->get_attr("action");
    std::string target = action ? space::resolve_url(base_url, action) : url;

    // Collect named inputs
    std::string query;
    std::vector<litehtml::element::ptr> stk = {form};
    while (!stk.empty()) {
        auto cur = stk.back();
        stk.pop_back();
        const char* name = cur->get_attr("name");
        if (name && *name && !cur->get_attr("disabled")) {
            std::string val;
            bool include = false;
            if (auto* inp = dynamic_cast<space::el_space_input*>(cur.get())) {
                std::string t = inp->input_type();
                if (t == "checkbox" || t == "radio") {
                    if (inp->is_checked()) {
                        val = inp->current_value().empty() ? "on" : inp->current_value();
                        include = true;
                    }
                } else if (t != "button" && t != "reset") {
                    val = inp->current_value();
                    include = (t != "submit") || (cur.get() == control.get());
                }
            } else if (auto* ta = dynamic_cast<space::el_space_textarea*>(cur.get())) {
                val = ta->current_value();
                include = true;
            } else if (auto* sel = dynamic_cast<space::el_space_select*>(cur.get())) {
                val = sel->current_value();
                include = true;
            }
            if (include) {
                if (!query.empty()) query.push_back('&');
                query += space::url_encode(name) + "=" + space::url_encode(val);
            }
        }
        for (auto it = cur->children().rbegin(); it != cur->children().rend(); ++it) {
            stk.push_back(*it);
        }
    }

    if (!query.empty()) {
        size_t hash = target.find('#');
        std::string frag = (hash == std::string::npos) ? "" : target.substr(hash);
        if (hash != std::string::npos) target = target.substr(0, hash);
        target += (target.find('?') == std::string::npos ? "?" : "&") + query + frag;
    }
    cb.on_navigate(cb.user_data, target.c_str(), 0);
}

void SpacePage::dispatch_click_or_submit(const std::shared_ptr<litehtml::element>& target) {
    if (!target || !doc) return;
    bool prevented = dom ? dom->dispatch_ui_event(target, "click", true, true) : false;
    if (prevented) return;

    // Trigger anchor or interactive control if needed
    auto cur = target;
    while (cur) {
        if (auto* inp = dynamic_cast<space::el_space_input*>(cur.get())) {
            auto p = cur->get_placement();
            inp->on_mouse_click(static_cast<int>(p.width.value() / 2), static_cast<int>(p.height.value() / 2));
            return;
        }
        if (auto* btn = dynamic_cast<space::el_space_button*>(cur.get())) {
            btn->on_click();
            return;
        }
        if (auto* sel = dynamic_cast<space::el_space_select*>(cur.get())) {
            sel->on_mouse_click(cur->get_placement());
            return;
        }
        if (cur->get_tagName() && space::to_lower_ascii(cur->get_tagName()) == "a") {
            const char* href = cur->get_attr("href");
            if (href && *href) {
                container->on_anchor_click(href, cur);
                return;
            }
        }
        cur = cur->parent();
    }
}

extern "C" {

SpacePage* space_page_create(const SpaceHostCallbacks* cb, int viewport_w, int viewport_h,
                             int dark_mode, int is_win98) {
    return new SpacePage(cb, viewport_w, viewport_h, dark_mode != 0, is_win98 != 0);
}

void space_page_destroy(SpacePage* page) {
    delete page;
}

void space_page_set_theme(SpacePage* page, int dark_mode, int is_win98) {
    if (!page) return;
    page->dark_mode = (dark_mode != 0);
    page->is_win98 = (is_win98 != 0);
    page->mark_dirty(true, true);
}

void space_page_load_html(SpacePage* page, const char* html, int html_len, const char* url) {
    if (!page) return;
    page->url = (url && *url) ? url : "about:blank";
    page->base_url = page->url;
    page->title.clear();
    page->focused_el.reset();
    page->active_el.reset();
    page->scroll_x = 0;
    page->scroll_y = 0;

    page->dom->reset_context();
    std::string html_str = (html && html_len > 0)
                               ? std::string(html, static_cast<size_t>(html_len))
                               : (html ? std::string(html) : std::string("<html><body></body></html>"));

    std::string master = std::string(litehtml::master_css) + "\n" + space::kExtraUaCss;
    std::string user_css;
    if (page->dark_mode && !page->is_win98) user_css = space::kDarkUaOverride;
    else if (page->is_win98) user_css = space::kWin98UaOverride;

    page->doc = litehtml::document::createFromString(
        html_str.c_str(), page->container.get(), master.c_str(), user_css.c_str());

    if (page->doc) {
        page->restyle_needed = false;
        page->relayout_needed = false;
        page->doc->render(static_cast<litehtml::pixel_t>(page->viewport_w));
        page->dom->execute_document_scripts();
        if (page->restyle_needed || page->relayout_needed) {
            page->ensure_layout();
        }
    }
    page->repaint_needed = true;
}

void space_page_resize(SpacePage* page, int viewport_w, int viewport_h) {
    if (!page) return;
    int nw = std::max(64, viewport_w);
    int nh = std::max(64, viewport_h);
    if (nw == page->viewport_w && nh == page->viewport_h) return;
    page->viewport_w = nw;
    page->viewport_h = nh;
    page->relayout_needed = true;
    page->repaint_needed = true;
    if (page->doc) {
        page->doc->media_changed();
        page->doc->render(static_cast<litehtml::pixel_t>(page->viewport_w));
        page->relayout_needed = false;
    }
}

int space_page_relayout(SpacePage* page, int force) {
    if (!page || !page->doc) return 0;
    if (!page->restyle_needed && !page->relayout_needed && !force) return 0;
    if (force) page->relayout_needed = true;
    page->ensure_layout();
    page->repaint_needed = true;
    return 1;
}

int space_page_doc_width(SpacePage* page) {
    if (!page || !page->doc) return 0;
    page->ensure_layout();
    return static_cast<int>(std::ceil(page->doc->width().value()));
}

int space_page_doc_height(SpacePage* page) {
    if (!page || !page->doc) return 0;
    page->ensure_layout();
    return static_cast<int>(std::ceil(page->doc->height().value()));
}

void space_page_draw(SpacePage* page, int scroll_x, int scroll_y, int clip_w, int clip_h) {
    if (!page || !page->doc) return;
    page->scroll_x = scroll_x;
    page->scroll_y = scroll_y;
    page->ensure_layout();
    litehtml::position clip(0, 0, static_cast<litehtml::pixel_t>(clip_w), static_cast<litehtml::pixel_t>(clip_h));
    page->doc->draw(0, static_cast<litehtml::pixel_t>(-scroll_x),
                    static_cast<litehtml::pixel_t>(-scroll_y), &clip);
    page->repaint_needed = false;
}

void space_page_mouse_move(SpacePage* page, int x, int y, int scroll_x, int scroll_y) {
    if (!page || !page->doc) return;
    page->scroll_x = scroll_x;
    page->scroll_y = scroll_y;
    litehtml::position::vector redraw_boxes;
    if (page->doc->on_mouse_over(static_cast<litehtml::pixel_t>(x + scroll_x),
                                 static_cast<litehtml::pixel_t>(y + scroll_y),
                                 static_cast<litehtml::pixel_t>(x),
                                 static_cast<litehtml::pixel_t>(y),
                                 [&](const litehtml::position& pos) { redraw_boxes.push_back(pos); })) {
        page->repaint_needed = true;
    }
}

void space_page_mouse_down(SpacePage* page, int x, int y, int scroll_x, int scroll_y, int button) {
    if (!page || !page->doc) return;
    page->scroll_x = scroll_x;
    page->scroll_y = scroll_y;
    float dx = static_cast<float>(x + scroll_x);
    float dy = static_cast<float>(y + scroll_y);
    litehtml::element::ptr hit;
    if (auto rr = page->doc->root_render()) {
        hit = rr->get_element_by_point(dx, dy, static_cast<float>(x), static_cast<float>(y),
                                       [](const std::shared_ptr<litehtml::render_item>&) { return true; });
    }
    page->active_el = hit;

    // Update focus cleanly on left click
    if (button == 1) {
        litehtml::element::ptr focus_target;
        auto cur = hit;
        while (cur) {
            if (dynamic_cast<space::el_space_input*>(cur.get()) ||
                dynamic_cast<space::el_space_textarea*>(cur.get()) ||
                dynamic_cast<space::el_space_select*>(cur.get()) ||
                dynamic_cast<space::el_space_button*>(cur.get()) ||
                cur->get_attr("tabindex")) {
                focus_target = cur;
                break;
            }
            cur = cur->parent();
        }
        auto prev = page->focused_el.lock();
        if (prev.get() != focus_target.get()) {
            if (prev && page->dom) page->dom->dispatch_ui_event(prev, "blur", false, false);
            page->focused_el = focus_target;
            if (focus_target && page->dom) page->dom->dispatch_ui_event(focus_target, "focus", false, false);
            page->mark_dirty(false, true);
        }
    }

    if (hit && page->dom) {
        page->dom->dispatch_ui_event(hit, "mousedown", true, true, x, y, button);
    }
    if (button == 1) {
        page->doc->on_lbutton_down(dx, dy, static_cast<float>(x), static_cast<float>(y),
                                   [](const litehtml::position&) {});
    }
    page->mark_dirty(false, true);
}

void space_page_mouse_up(SpacePage* page, int x, int y, int scroll_x, int scroll_y, int button) {
    if (!page || !page->doc) return;
    page->scroll_x = scroll_x;
    page->scroll_y = scroll_y;
    float dx = static_cast<float>(x + scroll_x);
    float dy = static_cast<float>(y + scroll_y);
    litehtml::element::ptr hit;
    if (auto rr = page->doc->root_render()) {
        hit = rr->get_element_by_point(dx, dy, static_cast<float>(x), static_cast<float>(y),
                                       [](const std::shared_ptr<litehtml::render_item>&) { return true; });
    }
    auto active = page->active_el.lock();
    page->active_el.reset();

    if (hit && page->dom) {
        page->dom->dispatch_ui_event(hit, "mouseup", true, true, x, y, button);
    }

    if (button == 1 && hit) {
        bool click_prevented = page->dom ? page->dom->dispatch_ui_event(hit, "click", true, true, x, y, 0) : false;
        if (!click_prevented) {
            bool handled_widget = false;
            auto cur = hit;
            while (cur) {
                if (auto* inp = dynamic_cast<space::el_space_input*>(cur.get())) {
                    auto p = cur->get_placement();
                    inp->on_mouse_click(static_cast<int>(dx - p.x.value()), static_cast<int>(dy - p.y.value()));
                    handled_widget = true;
                    break;
                }
                if (auto* ta = dynamic_cast<space::el_space_textarea*>(cur.get())) {
                    auto p = cur->get_placement();
                    ta->on_mouse_click(static_cast<int>(dx - p.x.value()), static_cast<int>(dy - p.y.value()));
                    handled_widget = true;
                    break;
                }
                if (auto* sel = dynamic_cast<space::el_space_select*>(cur.get())) {
                    sel->on_mouse_click(cur->get_placement());
                    handled_widget = true;
                    break;
                }
                cur = cur->parent();
            }
            if (!handled_widget) {
                page->doc->on_lbutton_up(dx, dy, static_cast<float>(x), static_cast<float>(y),
                                         [](const litehtml::position&) {});
            }
        }
    }
    (void)active;
    page->mark_dirty(false, true);
}

void space_page_mouse_leave(SpacePage* page) {
    if (!page || !page->doc) return;
    page->doc->on_mouse_leave([](const litehtml::position&) {});
    page->active_el.reset();
    page->mark_dirty(false, true);
}

int space_page_key_down(SpacePage* page, int key_code, const char* utf8_text, int mods) {
    if (!page || !page->doc) return 0;
    auto focus = page->focused_el.lock();
    bool ctrl = (mods & 4) != 0;
    bool shift = (mods & 1) != 0;
    bool alt = (mods & 8) != 0;
    std::string kstr = utf8_text ? utf8_text : "";
    if (key_code == 0xff0d || key_code == 0xff8d) kstr = "Enter";
    else if (key_code == 0xff08) kstr = "Backspace";
    else if (key_code == 0xff09) kstr = "Tab";
    else if (key_code == 0xff1b) kstr = "Escape";
    else if (key_code == 0xff51) kstr = "ArrowLeft";
    else if (key_code == 0xff52) kstr = "ArrowUp";
    else if (key_code == 0xff53) kstr = "ArrowRight";
    else if (key_code == 0xff54) kstr = "ArrowDown";

    auto event_target = focus ? focus : page->doc->root();
    if (event_target && page->dom) {
        bool prevented = page->dom->dispatch_ui_event(
            event_target, "keydown", true, true, 0, 0, 0, key_code, kstr, ctrl, shift, alt);
        if (prevented) return 1;
    }

    int handled = 0;
    if (auto* inp = dynamic_cast<space::el_space_input*>(focus.get())) {
        if (inp->on_key(key_code, utf8_text, mods)) handled = 1;
    } else if (auto* ta = dynamic_cast<space::el_space_textarea*>(focus.get())) {
        if (ta->on_key(key_code, utf8_text, mods)) handled = 1;
    }

    if (event_target && page->dom) {
        page->dom->dispatch_ui_event(
            event_target, "keyup", true, true, 0, 0, 0, key_code, kstr, ctrl, shift, alt);
    }
    return handled;
}

void space_page_select_option(SpacePage* page, uint64_t node_id, int selected_index) {
    if (!page || !page->dom) return;
    auto el = page->dom->find_node(node_id);
    if (auto* sel = dynamic_cast<space::el_space_select*>(el.get())) {
        sel->set_selected_index(selected_index);
        page->dom->dispatch_ui_event(el, "input", true, true);
        page->dom->dispatch_ui_event(el, "change", true, true);
        page->mark_dirty(false, true);
    }
}

void space_page_resolve_fetch(SpacePage* page, uint64_t req_id, int status,
                              const char* resolved_url, const char* headers_json,
                              const char* body, int body_len, const char* err) {
    if (!page || !page->dom) return;
    page->dom->resolve_fetch(req_id, status, resolved_url, headers_json, body, body_len, err);
}

int space_page_tick(SpacePage* page, double now_ms) {
    if (!page) return 0;
    page->now_ms = now_ms;
    if (page->dom) page->dom->tick(now_ms);
    if ((page->restyle_needed || page->relayout_needed) && page->doc) {
        page->ensure_layout();
        page->repaint_needed = true;
    }
    return (page->repaint_needed || page->relayout_needed) ? 1 : 0;
}

int space_page_eval(SpacePage* page, const char* code, int code_len, char* out_buf, int out_cap) {
    if (!page || !page->dom || !code) return 0;
    std::string src = (code_len > 0) ? std::string(code, static_cast<size_t>(code_len)) : std::string(code);
    std::string res = page->dom->eval_repl(src);
    if (out_buf && out_cap > 0) {
        int copy_len = std::min(out_cap - 1, static_cast<int>(res.size()));
        if (copy_len > 0) std::memcpy(out_buf, res.data(), static_cast<size_t>(copy_len));
        out_buf[copy_len] = '\0';
        return copy_len;
    }
    return static_cast<int>(res.size());
}

int space_page_dump_dom(SpacePage* page, char* out_buf, int out_cap) {
    if (!page || !page->dom) return 0;
    std::string dump = page->dom->dump_dom_summary();
    if (out_buf && out_cap > 0) {
        int copy_len = std::min(out_cap - 1, static_cast<int>(dump.size()));
        if (copy_len > 0) std::memcpy(out_buf, dump.data(), static_cast<size_t>(copy_len));
        out_buf[copy_len] = '\0';
        return copy_len;
    }
    return static_cast<int>(dump.size());
}

int space_page_find_text(SpacePage* page, const char* query, int occurrence_idx, int* out_y) {
    if (!page || !page->doc || !page->doc->root() || !query || !*query) return 0;
    std::string needle(query);
    int total = 0;
    int target_y = 0;
    bool found_target = false;
    std::vector<litehtml::element::ptr> stk = {page->doc->root()};
    while (!stk.empty()) {
        auto cur = stk.back();
        stk.pop_back();
        if (!cur->is_text() && cur->get_tagName()) {
            std::string tag = space::to_lower_ascii(cur->get_tagName());
            if (tag == "script" || tag == "style" || tag == "head" || tag == "title") {
                continue;
            }
        }
        if (cur->is_text()) {
            std::string txt;
            cur->get_text(txt);
            if (space::contains_ci(txt, needle)) {
                if (total == occurrence_idx) {
                    auto par = cur->parent();
                    if (par) target_y = static_cast<int>(par->get_placement().y.value());
                    found_target = true;
                }
                ++total;
            }
        } else {
            for (auto it = cur->children().rbegin(); it != cur->children().rend(); ++it) stk.push_back(*it);
        }
    }
    if (out_y && found_target) *out_y = std::max(0, target_y - 40);
    return total;
}

void space_page_set_focused(SpacePage* page, int focused) {
    if (!page) return;
    bool f = (focused != 0);
    if (page->page_focused != f) {
        page->page_focused = f;
        page->mark_dirty(false, true);
    }
}

} // extern "C"
