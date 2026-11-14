#pragma once

#include "engine_api.h"
#include "litehtml.h"
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

namespace space {

class SpaceContainer;
class DomBridge;

std::string to_lower_ascii(std::string s);
std::string trim_ascii(const std::string& s);
std::string resolve_url(const std::string& base_url, const std::string& ref);
std::string url_encode(const std::string& s);
uint32_t web_color_to_argb(const litehtml::web_color& c);
litehtml::uint_ptr create_host_font(const SpaceHostCallbacks& cb, const char* family, int size_px,
                                    int weight, bool italic, int decoration,
                                    litehtml::font_metrics* out_fm = nullptr);

} // namespace space

struct SpacePage {
    SpaceHostCallbacks cb{};
    int viewport_w = 800;
    int viewport_h = 600;
    int scroll_x = 0;
    int scroll_y = 0;
    bool dark_mode = false;
    bool is_win98 = false;
    bool restyle_needed = false;
    bool relayout_needed = false;
    bool repaint_needed = false;
    bool page_focused = true;
    double now_ms = 0.0;

    std::string url = "about:blank";
    std::string base_url = "about:blank";
    std::string title;

    std::shared_ptr<space::SpaceContainer> container;
    std::shared_ptr<litehtml::document> doc;
    std::unique_ptr<space::DomBridge> dom;

    std::weak_ptr<litehtml::element> focused_el;
    std::weak_ptr<litehtml::element> active_el;

    uint64_t ui_font = 0;

    SpacePage(const SpaceHostCallbacks* host_cb, int vw, int vh, bool dark, bool win98);
    ~SpacePage();

    void mark_dirty(bool relayout, bool repaint);
    void ensure_restyle();
    void ensure_layout();
    void ensure_ui_font();
    void dispatch_click_or_submit(const std::shared_ptr<litehtml::element>& el);
    void submit_form_for(const std::shared_ptr<litehtml::element>& control);
};
