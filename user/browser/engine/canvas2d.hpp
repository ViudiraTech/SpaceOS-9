#pragma once

#include "engine_api.h"
#include <stdint.h>
#include <string>
#include <vector>

namespace space {

struct CanvasPoint {
    double x;
    double y;
};

struct CanvasTextCmd {
    std::string text;
    std::string family;
    int size;
    int weight;
    bool italic;
    uint32_t argb;
    int x;
    int y;
    int w;
    int h;
};

struct CanvasState {
    uint32_t fill_argb = 0xff000000u;
    uint32_t stroke_argb = 0xff000000u;
    double line_width = 1.0;
    double global_alpha = 1.0;
    std::string font = "14px sans-serif";
    int font_size = 14;
    int font_weight = 400;
    bool font_italic = false;
    std::string font_family = "sans-serif";
    std::string text_align = "left";
    std::string text_baseline = "alphabetic";
    // 2D affine matrix: x' = m[0]*x + m[2]*y + m[4], y' = m[1]*x + m[3]*y + m[5]
    double m[6] = {1.0, 0.0, 0.0, 1.0, 0.0, 0.0};
};

class Canvas2D {
  public:
    int width = 300;
    int height = 150;
    std::vector<uint32_t> pixels; // ARGB 0xAARRGGBB
    std::vector<CanvasTextCmd> text_cmds;
    CanvasState state;
    std::vector<CanvasState> stack;
    std::vector<std::vector<CanvasPoint>> subpaths;
    bool subpath_closed = false;

    explicit Canvas2D(int w = 300, int h = 150);
    void resize(int w, int h);
    void save();
    void restore();
    void translate(double tx, double ty);
    void scale(double sx, double sy);
    void rotate(double rad);
    void set_transform(double a, double b, double c, double d, double e, double f);
    void transform(double a, double b, double c, double d, double e, double f);
    void reset_transform();

    void set_fill_style(const char* css);
    void set_stroke_style(const char* css);
    void set_font(const char* css);

    void begin_path();
    void close_path();
    void move_to(double x, double y);
    void line_to(double x, double y);
    void rect(double x, double y, double w, double h);
    void arc(double cx, double cy, double r, double a0, double a1, bool ccw);
    void quadratic_curve_to(double cpx, double cpy, double x, double y);
    void bezier_curve_to(double cp1x, double cp1y, double cp2x, double cp2y, double x, double y);

    void clear_rect(double x, double y, double w, double h);
    void fill_rect(double x, double y, double w, double h);
    void stroke_rect(double x, double y, double w, double h);
    void fill_path();
    void stroke_path();

    double measure_text(const SpaceHostCallbacks* cb, const char* text) const;
    void fill_text(const SpaceHostCallbacks* cb, const char* text, double x, double y);

    static uint32_t parse_css_color(const char* css, uint32_t fallback = 0xff000000u);

  private:
    CanvasPoint map(double x, double y) const;
    void blend_pixel(int x, int y, uint32_t argb);
    void draw_segment(CanvasPoint p0, CanvasPoint p1, uint32_t argb, double thickness);
    void fill_polygon(const std::vector<std::vector<CanvasPoint>>& polys, uint32_t argb);
};

} // namespace space
