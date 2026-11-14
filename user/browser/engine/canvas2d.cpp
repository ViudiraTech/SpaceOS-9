#include "canvas2d.hpp"
#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <vector>

namespace space {

namespace {

constexpr double kPi = 3.14159265358979323846;

uint8_t clamp_u8(int v) {
    return static_cast<uint8_t>(std::clamp(v, 0, 255));
}

uint32_t apply_alpha(uint32_t argb, double alpha) {
    uint8_t a = static_cast<uint8_t>(std::clamp(((argb >> 24) & 0xff) * alpha, 0.0, 255.0));
    return (static_cast<uint32_t>(a) << 24) | (argb & 0x00ffffffu);
}

uint32_t hsl_to_argb(double h, double s, double l, double a) {
    h = std::fmod(std::fmod(h, 360.0) + 360.0, 360.0) / 360.0;
    s = std::clamp(s, 0.0, 1.0);
    l = std::clamp(l, 0.0, 1.0);
    auto hue2rgb = [](double p, double q, double t) {
        if (t < 0.0) t += 1.0;
        if (t > 1.0) t -= 1.0;
        if (t < 1.0 / 6.0) return p + (q - p) * 6.0 * t;
        if (t < 1.0 / 2.0) return q;
        if (t < 2.0 / 3.0) return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        return p;
    };
    double r, g, b;
    if (s == 0.0) {
        r = g = b = l;
    } else {
        double q = l < 0.5 ? l * (1.0 + s) : l + s - l * s;
        double p = 2.0 * l - q;
        r = hue2rgb(p, q, h + 1.0 / 3.0);
        g = hue2rgb(p, q, h);
        b = hue2rgb(p, q, h - 1.0 / 3.0);
    }
    uint8_t aa = clamp_u8(static_cast<int>(std::round(std::clamp(a, 0.0, 1.0) * 255.0)));
    uint8_t rr = clamp_u8(static_cast<int>(std::round(r * 255.0)));
    uint8_t gg = clamp_u8(static_cast<int>(std::round(g * 255.0)));
    uint8_t bb = clamp_u8(static_cast<int>(std::round(b * 255.0)));
    return (static_cast<uint32_t>(aa) << 24) | (static_cast<uint32_t>(rr) << 16) |
           (static_cast<uint32_t>(gg) << 8) | bb;
}

std::string trim_lower(const char* raw) {
    if (!raw) return "";
    std::string out;
    for (const char* p = raw; *p; ++p) {
        if (!std::isspace(static_cast<unsigned char>(*p))) {
            out.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(*p))));
        }
    }
    return out;
}

int hex_digit(char ch) {
    if (ch >= '0' && ch <= '9') return ch - '0';
    if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
    if (ch >= 'A' && ch <= 'F') return ch - 'A' + 10;
    return -1;
}

} // namespace

Canvas2D::Canvas2D(int w, int h) {
    resize(w, h);
}

void Canvas2D::resize(int w, int h) {
    width = std::clamp(w, 1, 2048);
    height = std::clamp(h, 1, 2048);
    pixels.assign(static_cast<size_t>(width) * static_cast<size_t>(height), 0x00000000u);
    text_cmds.clear();
    state = CanvasState{};
    stack.clear();
    subpaths.clear();
}

void Canvas2D::save() {
    if (stack.size() < 64) {
        stack.push_back(state);
    }
}

void Canvas2D::restore() {
    if (!stack.empty()) {
        state = stack.back();
        stack.pop_back();
    }
}

void Canvas2D::transform(double a, double b, double c, double d, double e, double f) {
    double m0 = state.m[0], m1 = state.m[1], m2 = state.m[2], m3 = state.m[3], m4 = state.m[4], m5 = state.m[5];
    state.m[0] = m0 * a + m2 * b;
    state.m[1] = m1 * a + m3 * b;
    state.m[2] = m0 * c + m2 * d;
    state.m[3] = m1 * c + m3 * d;
    state.m[4] = m0 * e + m2 * f + m4;
    state.m[5] = m1 * e + m3 * f + m5;
}

void Canvas2D::set_transform(double a, double b, double c, double d, double e, double f) {
    state.m[0] = a;
    state.m[1] = b;
    state.m[2] = c;
    state.m[3] = d;
    state.m[4] = e;
    state.m[5] = f;
}

void Canvas2D::reset_transform() {
    set_transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
}

void Canvas2D::translate(double tx, double ty) {
    transform(1.0, 0.0, 0.0, 1.0, tx, ty);
}

void Canvas2D::scale(double sx, double sy) {
    transform(sx, 0.0, 0.0, sy, 0.0, 0.0);
}

void Canvas2D::rotate(double rad) {
    double cs = std::cos(rad);
    double sn = std::sin(rad);
    transform(cs, sn, -sn, cs, 0.0, 0.0);
}

uint32_t Canvas2D::parse_css_color(const char* css, uint32_t fallback) {
    std::string s = trim_lower(css);
    if (s.empty()) return fallback;
    if (s[0] == '#') {
        if (s.size() == 4 || s.size() == 5) {
            int r = hex_digit(s[1]), g = hex_digit(s[2]), b = hex_digit(s[3]);
            int a = s.size() == 5 ? hex_digit(s[4]) : 15;
            if (r >= 0 && g >= 0 && b >= 0 && a >= 0) {
                return (static_cast<uint32_t>(a * 17) << 24) | (static_cast<uint32_t>(r * 17) << 16) |
                       (static_cast<uint32_t>(g * 17) << 8) | static_cast<uint32_t>(b * 17);
            }
        } else if (s.size() == 7 || s.size() == 9) {
            auto byte_at = [&](size_t idx) -> int {
                int hi = hex_digit(s[idx]);
                int lo = hex_digit(s[idx + 1]);
                return (hi >= 0 && lo >= 0) ? (hi * 16 + lo) : -1;
            };
            int r = byte_at(1), g = byte_at(3), b = byte_at(5);
            int a = s.size() == 9 ? byte_at(7) : 255;
            if (r >= 0 && g >= 0 && b >= 0 && a >= 0) {
                return (static_cast<uint32_t>(a) << 24) | (static_cast<uint32_t>(r) << 16) |
                       (static_cast<uint32_t>(g) << 8) | static_cast<uint32_t>(b);
            }
        }
        return fallback;
    }
    if (s.rfind("rgba(", 0) == 0 || s.rfind("rgb(", 0) == 0) {
        double r = 0, g = 0, b = 0, a = 1.0;
        const char* p = std::strchr(s.c_str(), '(');
        if (p) {
            int n = std::sscanf(p + 1, "%lf,%lf,%lf,%lf", &r, &g, &b, &a);
            if (n >= 3) {
                uint8_t aa = clamp_u8(static_cast<int>(std::round(std::clamp(a, 0.0, 1.0) * 255.0)));
                return (static_cast<uint32_t>(aa) << 24) |
                       (static_cast<uint32_t>(clamp_u8(static_cast<int>(r))) << 16) |
                       (static_cast<uint32_t>(clamp_u8(static_cast<int>(g))) << 8) |
                       clamp_u8(static_cast<int>(b));
            }
        }
    }
    if (s.rfind("hsla(", 0) == 0 || s.rfind("hsl(", 0) == 0) {
        double h = 0, sat = 0, lig = 0, a = 1.0;
        const char* p = std::strchr(s.c_str(), '(');
        if (p) {
            int n = std::sscanf(p + 1, "%lf,%lf%%,%lf%%,%lf", &h, &sat, &lig, &a);
            if (n >= 3) {
                return hsl_to_argb(h, sat / 100.0, lig / 100.0, a);
            }
        }
    }
    struct Named {
        const char* name;
        uint32_t color;
    };
    static constexpr Named kNamed[] = {
        {"transparent", 0x00000000u}, {"black", 0xff000000u},   {"white", 0xffffffffu},
        {"red", 0xffff0000u},         {"green", 0xff008000u},   {"lime", 0xff00ff00u},
        {"blue", 0xff0000ffu},        {"yellow", 0xffffff00u},  {"cyan", 0xff00ffffu},
        {"aqua", 0xff00ffffu},        {"magenta", 0xffff00ffu}, {"gray", 0xff808080u},
        {"grey", 0xff808080u},        {"silver", 0xffc0c0c0u},  {"navy", 0xff000080u},
        {"teal", 0xff008080u},        {"purple", 0xff800080u},  {"orange", 0xffffa500u},
        {"pink", 0xffffc0cbu},        {"brown", 0xffa52a2au},   {"gold", 0xffffd700u},
        {"indigo", 0xff4b0082u},      {"violet", 0xffee82eeu},
    };
    for (const auto& entry : kNamed) {
        if (s == entry.name) return entry.color;
    }
    return fallback;
}

void Canvas2D::set_fill_style(const char* css) {
    state.fill_argb = parse_css_color(css, state.fill_argb);
}

void Canvas2D::set_stroke_style(const char* css) {
    state.stroke_argb = parse_css_color(css, state.stroke_argb);
}

void Canvas2D::set_font(const char* css) {
    if (!css) return;
    state.font = css;
    std::string s(css);
    std::string lower = s;
    for (char& ch : lower) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    state.font_italic = (lower.find("italic") != std::string::npos);
    state.font_weight = (lower.find("bold") != std::string::npos) ? 700 : 400;
    size_t px_pos = lower.find("px");
    if (px_pos != std::string::npos) {
        size_t start = px_pos;
        while (start > 0 && (std::isdigit(static_cast<unsigned char>(lower[start - 1])) || lower[start - 1] == '.')) {
            --start;
        }
        if (start < px_pos) {
            int sz = std::atoi(lower.substr(start, px_pos - start).c_str());
            if (sz > 0) state.font_size = std::clamp(sz, 6, 160);
        }
        size_t fam = px_pos + 2;
        while (fam < s.size() && std::isspace(static_cast<unsigned char>(s[fam]))) ++fam;
        if (fam < s.size()) {
            state.font_family = s.substr(fam);
        }
    }
}

CanvasPoint Canvas2D::map(double x, double y) const {
    return {
        state.m[0] * x + state.m[2] * y + state.m[4],
        state.m[1] * x + state.m[3] * y + state.m[5],
    };
}

void Canvas2D::blend_pixel(int x, int y, uint32_t argb) {
    if (x < 0 || y < 0 || x >= width || y >= height) return;
    uint32_t sa = (argb >> 24) & 0xff;
    if (sa == 0) return;
    size_t idx = static_cast<size_t>(y) * static_cast<size_t>(width) + static_cast<size_t>(x);
    if (sa == 255) {
        pixels[idx] = argb;
        return;
    }
    uint32_t dst = pixels[idx];
    uint32_t da = (dst >> 24) & 0xff;
    uint32_t sr = (argb >> 16) & 0xff, sg = (argb >> 8) & 0xff, sb = argb & 0xff;
    uint32_t dr = (dst >> 16) & 0xff, dg = (dst >> 8) & 0xff, db = dst & 0xff;
    uint32_t inv = 255 - sa;
    uint32_t oa = sa + ((da * inv + 127) / 255);
    if (oa == 0) {
        pixels[idx] = 0;
        return;
    }
    uint32_t r = (sr * sa + ((dr * da * inv + 127) / 255)) / oa;
    uint32_t g = (sg * sa + ((dg * da * inv + 127) / 255)) / oa;
    uint32_t b = (sb * sa + ((db * da * inv + 127) / 255)) / oa;
    pixels[idx] = (std::min(oa, 255u) << 24) | (std::min(r, 255u) << 16) |
                  (std::min(g, 255u) << 8) | std::min(b, 255u);
}

void Canvas2D::begin_path() {
    subpaths.clear();
    subpath_closed = false;
}

void Canvas2D::close_path() {
    if (!subpaths.empty() && subpaths.back().size() >= 2) {
        subpaths.back().push_back(subpaths.back().front());
        subpath_closed = true;
    }
}

void Canvas2D::move_to(double x, double y) {
    subpaths.push_back({map(x, y)});
    subpath_closed = false;
}

void Canvas2D::line_to(double x, double y) {
    if (subpaths.empty()) {
        move_to(x, y);
        return;
    }
    subpaths.back().push_back(map(x, y));
}

void Canvas2D::rect(double x, double y, double w, double h) {
    move_to(x, y);
    line_to(x + w, y);
    line_to(x + w, y + h);
    line_to(x, y + h);
    close_path();
}

void Canvas2D::arc(double cx, double cy, double r, double a0, double a1, bool ccw) {
    if (r < 0.0) return;
    double sweep = a1 - a0;
    if (ccw) {
        while (sweep > 0.0) sweep -= 2.0 * kPi;
        if (sweep == 0.0 && a0 != a1) sweep = -2.0 * kPi;
    } else {
        while (sweep < 0.0) sweep += 2.0 * kPi;
        if (sweep == 0.0 && a0 != a1) sweep = 2.0 * kPi;
    }
    if (std::abs(a1 - a0) >= 2.0 * kPi - 1e-6) {
        sweep = ccw ? -2.0 * kPi : 2.0 * kPi;
    }
    int steps = std::clamp(static_cast<int>(std::ceil(std::abs(sweep) * std::max(r, 4.0) / 3.0)), 12, 128);
    for (int i = 0; i <= steps; ++i) {
        double t = a0 + sweep * (static_cast<double>(i) / steps);
        double px = cx + std::cos(t) * r;
        double py = cy + std::sin(t) * r;
        if (i == 0 && subpaths.empty()) {
            move_to(px, py);
        } else {
            line_to(px, py);
        }
    }
}

void Canvas2D::quadratic_curve_to(double cpx, double cpy, double x, double y) {
    if (subpaths.empty()) {
        move_to(cpx, cpy);
        return;
    }
    // Transform endpoints back or evaluate in current transform by transforming control point.
    CanvasPoint p0 = subpaths.back().back();
    CanvasPoint p1 = map(cpx, cpy);
    CanvasPoint p2 = map(x, y);
    const int steps = 20;
    for (int i = 1; i <= steps; ++i) {
        double t = static_cast<double>(i) / steps;
        double u = 1.0 - t;
        subpaths.back().push_back({
            u * u * p0.x + 2.0 * u * t * p1.x + t * t * p2.x,
            u * u * p0.y + 2.0 * u * t * p1.y + t * t * p2.y,
        });
    }
}

void Canvas2D::bezier_curve_to(double cp1x, double cp1y, double cp2x, double cp2y, double x, double y) {
    if (subpaths.empty()) {
        move_to(cp1x, cp1y);
        return;
    }
    CanvasPoint p0 = subpaths.back().back();
    CanvasPoint p1 = map(cp1x, cp1y);
    CanvasPoint p2 = map(cp2x, cp2y);
    CanvasPoint p3 = map(x, y);
    const int steps = 28;
    for (int i = 1; i <= steps; ++i) {
        double t = static_cast<double>(i) / steps;
        double u = 1.0 - t;
        subpaths.back().push_back({
            u * u * u * p0.x + 3.0 * u * u * t * p1.x + 3.0 * u * t * t * p2.x + t * t * t * p3.x,
            u * u * u * p0.y + 3.0 * u * u * t * p1.y + 3.0 * u * t * t * p2.y + t * t * t * p3.y,
        });
    }
}

void Canvas2D::clear_rect(double x, double y, double w, double h) {
    if (std::abs(state.m[1]) < 1e-6 && std::abs(state.m[2]) < 1e-6) {
        CanvasPoint p0 = map(x, y);
        CanvasPoint p1 = map(x + w, y + h);
        int x0 = std::max(0, static_cast<int>(std::floor(std::min(p0.x, p1.x))));
        int y0 = std::max(0, static_cast<int>(std::floor(std::min(p0.y, p1.y))));
        int x1 = std::min(width, static_cast<int>(std::ceil(std::max(p0.x, p1.x))));
        int y1 = std::min(height, static_cast<int>(std::ceil(std::max(p0.y, p1.y))));
        if (x0 <= 0 && y0 <= 0 && x1 >= width && y1 >= height) {
            text_cmds.clear();
        } else if (!text_cmds.empty()) {
            text_cmds.erase(
                std::remove_if(text_cmds.begin(), text_cmds.end(), [&](const CanvasTextCmd& tc) {
                    return tc.x >= x0 && tc.y >= y0 && tc.x + tc.w <= x1 && tc.y + tc.h <= y1;
                }),
                text_cmds.end());
        }
        for (int py = y0; py < y1; ++py) {
            for (int px = x0; px < x1; ++px) {
                pixels[static_cast<size_t>(py) * width + px] = 0x00000000u;
            }
        }
        return;
    }
    std::vector<std::vector<CanvasPoint>> poly = {{map(x, y), map(x + w, y), map(x + w, y + h), map(x, y + h)}};
    for (int py = 0; py < height; ++py) {
        std::vector<double> xs;
        for (size_t i = 0; i < 4; ++i) {
            CanvasPoint a = poly[0][i], b = poly[0][(i + 1) % 4];
            if ((a.y <= py + 0.5 && b.y > py + 0.5) || (b.y <= py + 0.5 && a.y > py + 0.5)) {
                xs.push_back(a.x + (py + 0.5 - a.y) * (b.x - a.x) / (b.y - a.y));
            }
        }
        std::sort(xs.begin(), xs.end());
        for (size_t k = 0; k + 1 < xs.size(); k += 2) {
            int ix0 = std::max(0, static_cast<int>(std::floor(xs[k])));
            int ix1 = std::min(width, static_cast<int>(std::ceil(xs[k + 1])));
            for (int px = ix0; px < ix1; ++px) {
                pixels[static_cast<size_t>(py) * width + px] = 0x00000000u;
            }
        }
    }
}

void Canvas2D::fill_polygon(const std::vector<std::vector<CanvasPoint>>& polys, uint32_t argb) {
    uint32_t col = apply_alpha(argb, state.global_alpha);
    if ((col >> 24) == 0) return;
    double min_y = height, max_y = 0;
    for (const auto& p : polys) {
        for (const auto& pt : p) {
            min_y = std::min(min_y, pt.y);
            max_y = std::max(max_y, pt.y);
        }
    }
    int y0 = std::max(0, static_cast<int>(std::floor(min_y)));
    int y1 = std::min(height - 1, static_cast<int>(std::ceil(max_y)));
    std::vector<double> xs;
    for (int py = y0; py <= y1; ++py) {
        xs.clear();
        double sy = py + 0.5;
        for (const auto& poly : polys) {
            if (poly.size() < 2) continue;
            for (size_t i = 0; i < poly.size(); ++i) {
                CanvasPoint a = poly[i];
                CanvasPoint b = poly[(i + 1) % poly.size()];
                if ((a.y <= sy && b.y > sy) || (b.y <= sy && a.y > sy)) {
                    xs.push_back(a.x + (sy - a.y) * (b.x - a.x) / (b.y - a.y));
                }
            }
        }
        std::sort(xs.begin(), xs.end());
        for (size_t i = 0; i + 1 < xs.size(); i += 2) {
            int ix0 = std::max(0, static_cast<int>(std::floor(xs[i])));
            int ix1 = std::min(width, static_cast<int>(std::ceil(xs[i + 1])));
            for (int px = ix0; px < ix1; ++px) {
                blend_pixel(px, py, col);
            }
        }
    }
}

void Canvas2D::draw_segment(CanvasPoint p0, CanvasPoint p1, uint32_t argb, double thickness) {
    uint32_t col = apply_alpha(argb, state.global_alpha);
    if ((col >> 24) == 0) return;
    double dx = p1.x - p0.x, dy = p1.y - p0.y;
    double len = std::hypot(dx, dy);
    double r = std::max(0.5, thickness * 0.5);
    int steps = std::max(1, static_cast<int>(std::ceil(len * 1.5)));
    int ir = static_cast<int>(std::ceil(r + 0.5));
    for (int i = 0; i <= steps; ++i) {
        double t = static_cast<double>(i) / steps;
        double cx = p0.x + dx * t;
        double cy = p0.y + dy * t;
        int ix0 = static_cast<int>(std::floor(cx)) - ir;
        int iy0 = static_cast<int>(std::floor(cy)) - ir;
        int ix1 = static_cast<int>(std::ceil(cx)) + ir;
        int iy1 = static_cast<int>(std::ceil(cy)) + ir;
        for (int py = iy0; py <= iy1; ++py) {
            for (int px = ix0; px <= ix1; ++px) {
                double d = std::hypot(px + 0.5 - cx, py + 0.5 - cy);
                if (d <= r) {
                    blend_pixel(px, py, col);
                }
            }
        }
    }
}

void Canvas2D::fill_rect(double x, double y, double w, double h) {
    std::vector<std::vector<CanvasPoint>> poly = {{map(x, y), map(x + w, y), map(x + w, y + h), map(x, y + h)}};
    fill_polygon(poly, state.fill_argb);
}

void Canvas2D::stroke_rect(double x, double y, double w, double h) {
    CanvasPoint p0 = map(x, y), p1 = map(x + w, y), p2 = map(x + w, y + h), p3 = map(x, y + h);
    draw_segment(p0, p1, state.stroke_argb, state.line_width);
    draw_segment(p1, p2, state.stroke_argb, state.line_width);
    draw_segment(p2, p3, state.stroke_argb, state.line_width);
    draw_segment(p3, p0, state.stroke_argb, state.line_width);
}

void Canvas2D::fill_path() {
    if (!subpaths.empty()) {
        fill_polygon(subpaths, state.fill_argb);
    }
}

void Canvas2D::stroke_path() {
    for (const auto& sp : subpaths) {
        for (size_t i = 0; i + 1 < sp.size(); ++i) {
            draw_segment(sp[i], sp[i + 1], state.stroke_argb, state.line_width);
        }
    }
}

double Canvas2D::measure_text(const SpaceHostCallbacks* cb, const char* text) const {
    if (!cb || !text || !text[0]) return 0.0;
    int h = 0, asc = 0, desc = 0, xh = 0;
    uint64_t fh = cb->create_font(cb->user_data, state.font_family.c_str(), state.font_size,
                                  state.font_weight, state.font_italic ? 1 : 0, 0, &h, &asc, &desc, &xh);
    int w = cb->text_width(cb->user_data, text, fh);
    cb->delete_font(cb->user_data, fh);
    return static_cast<double>(w);
}

void Canvas2D::fill_text(const SpaceHostCallbacks* cb, const char* text, double x, double y) {
    if (!cb || !text || !text[0]) return;
    int h = 0, asc = 0, desc = 0, xh = 0;
    uint64_t fh = cb->create_font(cb->user_data, state.font_family.c_str(), state.font_size,
                                  state.font_weight, state.font_italic ? 1 : 0, 0, &h, &asc, &desc, &xh);
    int w = cb->text_width(cb->user_data, text, fh);
    CanvasPoint p = map(x, y);
    double dx = p.x;
    if (state.text_align == "center") dx -= w * 0.5;
    else if (state.text_align == "right" || state.text_align == "end") dx -= w;
    double dy = p.y;
    if (state.text_baseline == "alphabetic" || state.text_baseline == "ideographic") dy -= asc;
    else if (state.text_baseline == "middle") dy -= h * 0.5;
    else if (state.text_baseline == "bottom") dy -= h;

    // Record high-DPI native TTF text draw overlay rendered during <canvas> paint.
    uint32_t col = apply_alpha(state.fill_argb, state.global_alpha);
    if ((col >> 24) != 0 && text_cmds.size() < 4096) {
        text_cmds.push_back(CanvasTextCmd{
            text,
            state.font_family,
            state.font_size,
            state.font_weight,
            state.font_italic,
            col,
            static_cast<int>(std::lround(dx)),
            static_cast<int>(std::lround(dy)),
            std::max(w, 1),
            std::max(h, 1),
        });
    }
    cb->delete_font(cb->user_data, fh);
}

} // namespace space
