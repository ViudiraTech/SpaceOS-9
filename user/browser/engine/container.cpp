#include "container.hpp"
#include "dom_bridge.hpp"
#include "page_internal.hpp"
#include "widgets.hpp"
#include <algorithm>
#include <cmath>
#include <cstring>

namespace space {

using litehtml::operator""_px;

namespace {

int map_border_style(litehtml::border_style st) {
    switch (st) {
    case litehtml::border_style_none:
    case litehtml::border_style_hidden:
        return SPACE_BORDER_NONE;
    case litehtml::border_style_dashed:
        return SPACE_BORDER_DASHED;
    case litehtml::border_style_dotted:
        return SPACE_BORDER_DOTTED;
    case litehtml::border_style_double:
        return SPACE_BORDER_DOUBLE;
    default:
        return SPACE_BORDER_SOLID;
    }
}

void fill_radii8(const litehtml::border_radiuses& r, int out[8]) {
    out[0] = static_cast<int>(std::round(r.top_left_x.value()));
    out[1] = static_cast<int>(std::round(r.top_left_y.value()));
    out[2] = static_cast<int>(std::round(r.top_right_x.value()));
    out[3] = static_cast<int>(std::round(r.top_right_y.value()));
    out[4] = static_cast<int>(std::round(r.bottom_right_x.value()));
    out[5] = static_cast<int>(std::round(r.bottom_right_y.value()));
    out[6] = static_cast<int>(std::round(r.bottom_left_x.value()));
    out[7] = static_cast<int>(std::round(r.bottom_left_y.value()));
}

int to_int(litehtml::pixel_t p) {
    return static_cast<int>(std::round(p.value()));
}

} // namespace

SpaceContainer::SpaceContainer(SpacePage* page) : page_(page) {}

SpaceContainer::~SpaceContainer() = default;

litehtml::uint_ptr SpaceContainer::create_font(const litehtml::font_description& descr,
                                               const litehtml::document* /*doc*/,
                                               litehtml::font_metrics* fm) {
    if (!page_) return 0;
    int size_px = std::max(6, static_cast<int>(std::round(descr.size.value())));
    bool italic = (descr.style == litehtml::font_style_italic);
    litehtml::font_metrics local{};
    auto handle = create_host_font(page_->cb, descr.family.c_str(), size_px, descr.weight,
                                   italic, descr.decoration_line, &local);
    font_decorations_[handle] = descr.decoration_line;
    if (fm) *fm = local;
    return handle;
}

void SpaceContainer::delete_font(litehtml::uint_ptr hFont) {
    font_decorations_.erase(hFont);
    if (page_ && page_->cb.delete_font && hFont != 0) {
        page_->cb.delete_font(page_->cb.user_data, static_cast<uint64_t>(hFont));
    }
}

litehtml::pixel_t SpaceContainer::text_width(const char* text, litehtml::uint_ptr hFont) {
    if (!text || !*text || !page_ || !page_->cb.text_width) return 0_px;
    return static_cast<float>(page_->cb.text_width(page_->cb.user_data, text, static_cast<uint64_t>(hFont)));
}

void SpaceContainer::draw_text(litehtml::uint_ptr /*hdc*/, const char* text, litehtml::uint_ptr hFont,
                               litehtml::web_color color, const litehtml::position& pos) {
    if (!text || !*text || !page_ || !page_->cb.draw_text) return;
    int deco = 0;
    auto it = font_decorations_.find(hFont);
    if (it != font_decorations_.end()) deco = it->second;
    page_->cb.draw_text(page_->cb.user_data, text, static_cast<uint64_t>(hFont),
                        web_color_to_argb(color), to_int(pos.x), to_int(pos.y),
                        to_int(pos.width), to_int(pos.height), deco);
}

litehtml::pixel_t SpaceContainer::pt_to_px(float pt) const {
    return static_cast<float>(pt * 96.0f / 72.0f);
}

litehtml::pixel_t SpaceContainer::get_default_font_size() const {
    return 16_px;
}

const char* SpaceContainer::get_default_font_name() const {
    return "sans-serif";
}

void SpaceContainer::draw_list_marker(litehtml::uint_ptr /*hdc*/, const litehtml::list_marker& marker) {
    if (!page_ || !page_->cb.draw_list_marker) return;
    page_->cb.draw_list_marker(page_->cb.user_data, static_cast<int>(marker.marker_type),
                               web_color_to_argb(marker.color), to_int(marker.pos.x),
                               to_int(marker.pos.y), to_int(marker.pos.width),
                               to_int(marker.pos.height), marker.index);
}

void SpaceContainer::load_image(const char* src, const char* baseurl, bool redraw_on_ready) {
    if (!page_ || !page_->cb.load_image || !src) return;
    const char* eff_base = (baseurl && *baseurl) ? baseurl : page_->base_url.c_str();
    page_->cb.load_image(page_->cb.user_data, src, eff_base, redraw_on_ready ? 1 : 0);
}

void SpaceContainer::get_image_size(const char* src, const char* baseurl, litehtml::size& sz) {
    sz.width = 0_px;
    sz.height = 0_px;
    if (!page_ || !page_->cb.get_image_size || !src) return;
    const char* eff_base = (baseurl && *baseurl) ? baseurl : page_->base_url.c_str();
    int w = 0, h = 0;
    page_->cb.get_image_size(page_->cb.user_data, src, eff_base, &w, &h);
    sz.width = static_cast<float>(w);
    sz.height = static_cast<float>(h);
}

void SpaceContainer::draw_image(litehtml::uint_ptr /*hdc*/, const litehtml::background_layer& layer,
                                const std::string& url, const std::string& base_url) {
    if (!page_ || !page_->cb.draw_image || url.empty()) return;
    const char* eff_base = base_url.empty() ? page_->base_url.c_str() : base_url.c_str();
    page_->cb.draw_image(page_->cb.user_data, url.c_str(), eff_base,
                         to_int(layer.clip_box.x), to_int(layer.clip_box.y),
                         to_int(layer.clip_box.width), to_int(layer.clip_box.height),
                         to_int(layer.origin_box.x), to_int(layer.origin_box.y),
                         to_int(layer.origin_box.width), to_int(layer.origin_box.height),
                         to_int(layer.border_box.x), to_int(layer.border_box.y),
                         to_int(layer.border_box.width), to_int(layer.border_box.height),
                         static_cast<int>(layer.repeat));
}

void SpaceContainer::draw_solid_fill(litehtml::uint_ptr /*hdc*/, const litehtml::background_layer& layer,
                                     const litehtml::web_color& color) {
    if (!page_ || !page_->cb.fill_rect) return;
    int radii[8];
    fill_radii8(layer.border_radius, radii);
    page_->cb.fill_rect(page_->cb.user_data, to_int(layer.border_box.x), to_int(layer.border_box.y),
                        to_int(layer.border_box.width), to_int(layer.border_box.height),
                        web_color_to_argb(color), radii);
}

void SpaceContainer::draw_linear_gradient(litehtml::uint_ptr /*hdc*/, const litehtml::background_layer& layer,
                                          const litehtml::background_layer::linear_gradient& gradient) {
    if (!page_ || gradient.color_points.empty()) return;
    int radii[8];
    fill_radii8(layer.border_radius, radii);
    if (!page_->cb.draw_linear_gradient) {
        if (page_->cb.fill_rect) {
            page_->cb.fill_rect(page_->cb.user_data, to_int(layer.border_box.x), to_int(layer.border_box.y),
                                to_int(layer.border_box.width), to_int(layer.border_box.height),
                                web_color_to_argb(gradient.color_points.front().color), radii);
        }
        return;
    }
    std::vector<uint32_t> colors;
    std::vector<float> stops;
    colors.reserve(gradient.color_points.size());
    stops.reserve(gradient.color_points.size());
    for (const auto& cp : gradient.color_points) {
        colors.push_back(web_color_to_argb(cp.color));
        stops.push_back(cp.offset);
    }
    page_->cb.draw_linear_gradient(
        page_->cb.user_data, to_int(layer.border_box.x), to_int(layer.border_box.y),
        to_int(layer.border_box.width), to_int(layer.border_box.height),
        gradient.start.x, gradient.start.y, gradient.end.x, gradient.end.y,
        colors.data(), stops.data(), static_cast<int>(colors.size()), radii);
}

void SpaceContainer::draw_radial_gradient(litehtml::uint_ptr /*hdc*/, const litehtml::background_layer& layer,
                                          const litehtml::background_layer::radial_gradient& gradient) {
    if (!page_ || gradient.color_points.empty()) return;
    int radii[8];
    fill_radii8(layer.border_radius, radii);
    if (!page_->cb.draw_radial_gradient) {
        if (page_->cb.fill_rect) {
            page_->cb.fill_rect(page_->cb.user_data, to_int(layer.border_box.x), to_int(layer.border_box.y),
                                to_int(layer.border_box.width), to_int(layer.border_box.height),
                                web_color_to_argb(gradient.color_points.front().color), radii);
        }
        return;
    }
    std::vector<uint32_t> colors;
    std::vector<float> stops;
    for (const auto& cp : gradient.color_points) {
        colors.push_back(web_color_to_argb(cp.color));
        stops.push_back(cp.offset);
    }
    page_->cb.draw_radial_gradient(
        page_->cb.user_data, to_int(layer.border_box.x), to_int(layer.border_box.y),
        to_int(layer.border_box.width), to_int(layer.border_box.height),
        gradient.position.x, gradient.position.y, gradient.radius.x, gradient.radius.y,
        colors.data(), stops.data(), static_cast<int>(colors.size()), radii);
}

void SpaceContainer::draw_conic_gradient(litehtml::uint_ptr /*hdc*/, const litehtml::background_layer& layer,
                                         const litehtml::background_layer::conic_gradient& gradient) {
    if (!page_ || !page_->cb.fill_rect || gradient.color_points.empty()) return;
    int radii[8];
    fill_radii8(layer.border_radius, radii);
    page_->cb.fill_rect(page_->cb.user_data, to_int(layer.border_box.x), to_int(layer.border_box.y),
                        to_int(layer.border_box.width), to_int(layer.border_box.height),
                        web_color_to_argb(gradient.color_points.front().color), radii);
}

void SpaceContainer::draw_borders(litehtml::uint_ptr /*hdc*/, const litehtml::borders& borders,
                                  const litehtml::position& draw_pos, bool /*root*/) {
    if (!page_ || !page_->cb.draw_borders) return;
    SpaceBorders b{};
    b.x = to_int(draw_pos.x);
    b.y = to_int(draw_pos.y);
    b.w = to_int(draw_pos.width);
    b.h = to_int(draw_pos.height);
    b.left = {to_int(borders.left.width), web_color_to_argb(borders.left.color), map_border_style(borders.left.style)};
    b.top = {to_int(borders.top.width), web_color_to_argb(borders.top.color), map_border_style(borders.top.style)};
    b.right = {to_int(borders.right.width), web_color_to_argb(borders.right.color), map_border_style(borders.right.style)};
    b.bottom = {to_int(borders.bottom.width), web_color_to_argb(borders.bottom.color), map_border_style(borders.bottom.style)};
    b.radius_tl_x = to_int(borders.radius.top_left_x);
    b.radius_tl_y = to_int(borders.radius.top_left_y);
    b.radius_tr_x = to_int(borders.radius.top_right_x);
    b.radius_tr_y = to_int(borders.radius.top_right_y);
    b.radius_br_x = to_int(borders.radius.bottom_right_x);
    b.radius_br_y = to_int(borders.radius.bottom_right_y);
    b.radius_bl_x = to_int(borders.radius.bottom_left_x);
    b.radius_bl_y = to_int(borders.radius.bottom_left_y);
    page_->cb.draw_borders(page_->cb.user_data, &b);
}

void SpaceContainer::set_caption(const char* caption) {
    if (!page_) return;
    page_->title = caption ? caption : "";
    if (page_->cb.set_title) {
        page_->cb.set_title(page_->cb.user_data, page_->title.c_str());
    }
}

void SpaceContainer::set_base_url(const char* base_url) {
    if (!page_) return;
    if (base_url && *base_url) {
        page_->base_url = resolve_url(page_->url, base_url);
        if (page_->cb.set_base_url) {
            page_->cb.set_base_url(page_->cb.user_data, page_->base_url.c_str());
        }
    }
}

void SpaceContainer::link(const std::shared_ptr<litehtml::document>& doc,
                          const litehtml::element::ptr& el) {
    if (!page_ || !doc || !el) return;
    const char* rel = el->get_attr("rel");
    const char* href = el->get_attr("href");
    if (!rel || !href || !*href) return;
    std::string rel_l = to_lower_ascii(rel);
    if (rel_l.find("stylesheet") == std::string::npos) return;
    std::string css_text;
    std::string base = page_->base_url;
    import_css(css_text, href, base);
    if (!css_text.empty()) {
        doc->add_stylesheet(css_text.c_str(), base.c_str(), el->get_attr("media"));
    }
}

void SpaceContainer::on_anchor_click(const char* url, const litehtml::element::ptr& /*el*/) {
    if (!page_ || !url || !*url || !page_->cb.on_navigate) return;
    std::string resolved = resolve_url(page_->base_url, url);
    page_->cb.on_navigate(page_->cb.user_data, resolved.c_str(), 0);
}

bool SpaceContainer::on_element_click(const litehtml::element::ptr& /*el*/) {
    return false;
}

void SpaceContainer::on_mouse_event(const litehtml::element::ptr& el, litehtml::mouse_event event) {
    if (!page_ || !page_->dom || !el) return;
    page_->dom->dispatch_ui_event(
        el, event == litehtml::mouse_event_enter ? "mouseover" : "mouseout", true, true);
}

void SpaceContainer::set_cursor(const char* cursor) {
    if (page_ && page_->cb.set_cursor) {
        page_->cb.set_cursor(page_->cb.user_data, cursor ? cursor : "default");
    }
}

void SpaceContainer::transform_text(std::string& text, litehtml::text_transform tt) {
    switch (tt) {
    case litehtml::text_transform_uppercase:
        for (char& ch : text) ch = static_cast<char>(std::toupper(static_cast<unsigned char>(ch)));
        break;
    case litehtml::text_transform_lowercase:
        for (char& ch : text) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
        break;
    case litehtml::text_transform_capitalize: {
        bool cap_next = true;
        for (char& ch : text) {
            unsigned char u = static_cast<unsigned char>(ch);
            if (std::isspace(u)) {
                cap_next = true;
            } else if (cap_next) {
                ch = static_cast<char>(std::toupper(u));
                cap_next = false;
            }
        }
        break;
    }
    default:
        break;
    }
}

void SpaceContainer::import_css(std::string& text, const std::string& url, std::string& baseurl) {
    if (!page_ || !page_->cb.fetch_sync || url.empty()) return;
    std::string ref_base = baseurl.empty() ? page_->base_url : baseurl;
    std::string resolved = resolve_url(ref_base, url);
    int status = 0;
    char* body = nullptr;
    int body_len = 0;
    char* final_url = nullptr;
    char* hdrs = nullptr;
    page_->cb.fetch_sync(page_->cb.user_data, "GET", resolved.c_str(), nullptr, nullptr, 0,
                         &status, &body, &body_len, &final_url, &hdrs);
    if (status >= 200 && status < 400 && body && body_len > 0) {
        text.assign(body, static_cast<size_t>(body_len));
        if (final_url && *final_url) baseurl = final_url;
        else baseurl = resolved;
    }
    if (page_->cb.free_host_buf) {
        if (body) page_->cb.free_host_buf(page_->cb.user_data, body);
        if (final_url) page_->cb.free_host_buf(page_->cb.user_data, final_url);
        if (hdrs) page_->cb.free_host_buf(page_->cb.user_data, hdrs);
    }
}

void SpaceContainer::set_clip(const litehtml::position& pos,
                              const litehtml::border_radiuses& bdr_radius) {
    if (!page_ || !page_->cb.set_clip) return;
    int radii[8];
    fill_radii8(bdr_radius, radii);
    page_->cb.set_clip(page_->cb.user_data, to_int(pos.x), to_int(pos.y),
                       to_int(pos.width), to_int(pos.height), radii);
}

void SpaceContainer::del_clip() {
    if (page_ && page_->cb.del_clip) {
        page_->cb.del_clip(page_->cb.user_data);
    }
}

void SpaceContainer::get_viewport(litehtml::position& viewport) const {
    viewport.x = 0_px;
    viewport.y = 0_px;
    viewport.width = static_cast<float>(page_ ? page_->viewport_w : 800);
    viewport.height = static_cast<float>(page_ ? page_->viewport_h : 600);
}

litehtml::element::ptr SpaceContainer::create_element(const char* tag_name,
                                                      const litehtml::string_map& /*attributes*/,
                                                      const std::shared_ptr<litehtml::document>& doc) {
    if (!tag_name) return nullptr;
    std::string tag = to_lower_ascii(tag_name);
    if (tag == "script") return std::make_shared<el_script>(doc);
    if (tag == "input") return std::make_shared<el_space_input>(doc, page_);
    if (tag == "textarea") return std::make_shared<el_space_textarea>(doc, page_);
    if (tag == "button") return std::make_shared<el_space_button>(doc, page_);
    if (tag == "select") return std::make_shared<el_space_select>(doc, page_);
    if (tag == "canvas") return std::make_shared<el_space_canvas>(doc, page_);
    return nullptr;
}

void SpaceContainer::get_media_features(litehtml::media_features& media) const {
    int vw = page_ ? page_->viewport_w : 800;
    int vh = page_ ? page_->viewport_h : 600;
    media.type = litehtml::media_type_screen;
    media.width = static_cast<float>(vw);
    media.height = static_cast<float>(vh);
    media.device_width = static_cast<float>(vw);
    media.device_height = static_cast<float>(vh);
    media.color = 8;
    media.color_index = 0;
    media.monochrome = 0;
    media.resolution = 96_px;
}

void SpaceContainer::get_language(std::string& language, std::string& culture) const {
    language = "zh";
    culture = "CN";
}

} // namespace space
