#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SpacePage SpacePage;

enum SpaceBorderStyle {
    SPACE_BORDER_NONE = 0,
    SPACE_BORDER_SOLID = 1,
    SPACE_BORDER_DASHED = 2,
    SPACE_BORDER_DOTTED = 3,
    SPACE_BORDER_DOUBLE = 4,
};

typedef struct SpaceBorderSide {
    int width;
    uint32_t color;
    int style;
} SpaceBorderSide;

typedef struct SpaceBorders {
    int x;
    int y;
    int w;
    int h;
    SpaceBorderSide left;
    SpaceBorderSide top;
    SpaceBorderSide right;
    SpaceBorderSide bottom;
    int radius_tl_x;
    int radius_tl_y;
    int radius_tr_x;
    int radius_tr_y;
    int radius_br_x;
    int radius_br_y;
    int radius_bl_x;
    int radius_bl_y;
} SpaceBorders;

typedef struct SpaceHostCallbacks {
    void* user_data;

    uint64_t (*create_font)(void* ud, const char* family, int size_px, int weight, int italic,
                            int decoration, int* out_height, int* out_ascent, int* out_descent,
                            int* out_x_height);
    void (*delete_font)(void* ud, uint64_t font_handle);
    int32_t (*text_width)(void* ud, const char* text, uint64_t font_handle);
    void (*draw_text)(void* ud, const char* text, uint64_t font_handle, uint32_t argb,
                      int x, int y, int w, int h, int decoration);

    void (*fill_rect)(void* ud, int x, int y, int w, int h, uint32_t argb, const int* radii8);
    void (*draw_linear_gradient)(void* ud, int x, int y, int w, int h,
                                 float x0, float y0, float x1, float y1,
                                 const uint32_t* colors, const float* stops, int stop_count,
                                 const int* radii8);
    void (*draw_radial_gradient)(void* ud, int x, int y, int w, int h,
                                 float cx, float cy, float rx, float ry,
                                 const uint32_t* colors, const float* stops, int stop_count,
                                 const int* radii8);
    void (*draw_borders)(void* ud, const SpaceBorders* borders);
    void (*draw_list_marker)(void* ud, int marker_type, uint32_t argb, int x, int y, int w, int h, int index);
    void (*set_clip)(void* ud, int x, int y, int w, int h, const int* radii8);
    void (*del_clip)(void* ud);
    void (*draw_rgba_blit)(void* ud, const uint32_t* pixels_argb, int src_w, int src_h,
                           int dst_x, int dst_y, int dst_w, int dst_h);

    void (*load_image)(void* ud, const char* src, const char* baseurl, int redraw_on_ready);
    void (*get_image_size)(void* ud, const char* src, const char* baseurl, int* out_w, int* out_h);
    void (*draw_image)(void* ud, const char* src, const char* baseurl,
                       int clip_x, int clip_y, int clip_w, int clip_h,
                       int orig_x, int orig_y, int orig_w, int orig_h,
                       int pos_x, int pos_y, int pos_w, int pos_h,
                       int repeat_mode);

    void (*fetch_sync)(void* ud, const char* method, const char* url,
                       const char* headers_json, const char* body, int body_len,
                       int* out_status, char** out_body, int* out_len,
                       char** out_resolved_url, char** out_headers_json);
    uint64_t (*fetch_async)(void* ud, const char* method, const char* url,
                            const char* headers_json, const char* body, int body_len);
    void (*free_host_buf)(void* ud, char* ptr);

    void (*set_title)(void* ud, const char* title);
    void (*set_base_url)(void* ud, const char* base_url);
    void (*set_cursor)(void* ud, const char* cursor);
    void (*on_navigate)(void* ud, const char* url, int replace);
    void (*on_history_op)(void* ud, int op, int delta, const char* url, const char* title);
    void (*on_console)(void* ud, int level, const char* msg);
    void (*on_alert)(void* ud, int kind, const char* msg, const char* def, char** out_reply);
    void (*on_select_popup)(void* ud, uint64_t node_id, int x, int y, int w, int h,
                            const char* items_tsv, int selected_idx);
    void (*on_dirty)(void* ud, int relayout, int repaint);

    char* (*storage_get)(void* ud, int is_session, const char* key);
    void (*storage_set)(void* ud, int is_session, const char* key, const char* val);
    void (*storage_remove)(void* ud, int is_session, const char* key);
    void (*storage_clear)(void* ud, int is_session);
    int (*storage_len)(void* ud, int is_session);
    char* (*storage_key)(void* ud, int is_session, int idx);

    char* (*cookies_get)(void* ud);
    void (*cookies_set)(void* ud, const char* set_cookie_kv);
    double (*now_ms)(void* ud);
    void (*prefetch_urls)(void* ud, const char* urls_nl);
} SpaceHostCallbacks;

SpacePage* space_page_create(const SpaceHostCallbacks* cb, int viewport_w, int viewport_h,
                             int dark_mode, int is_win98);
void space_page_destroy(SpacePage* page);
void space_page_set_theme(SpacePage* page, int dark_mode, int is_win98);
void space_page_load_html(SpacePage* page, const char* html, int html_len, const char* url);
void space_page_resize(SpacePage* page, int viewport_w, int viewport_h);
int space_page_relayout(SpacePage* page, int force);
int space_page_doc_width(SpacePage* page);
int space_page_doc_height(SpacePage* page);
void space_page_draw(SpacePage* page, int scroll_x, int scroll_y, int clip_w, int clip_h);
void space_page_mouse_move(SpacePage* page, int x, int y, int scroll_x, int scroll_y);
void space_page_mouse_down(SpacePage* page, int x, int y, int scroll_x, int scroll_y, int button);
void space_page_mouse_up(SpacePage* page, int x, int y, int scroll_x, int scroll_y, int button);
void space_page_mouse_leave(SpacePage* page);
int space_page_key_down(SpacePage* page, int key_code, const char* utf8_text, int mods);
void space_page_select_option(SpacePage* page, uint64_t node_id, int selected_index);
void space_page_resolve_fetch(SpacePage* page, uint64_t req_id, int status,
                              const char* resolved_url, const char* headers_json,
                              const char* body, int body_len, const char* err);
int space_page_tick(SpacePage* page, double now_ms);
int space_page_eval(SpacePage* page, const char* code, int code_len, char* out_buf, int out_cap);
int space_page_dump_dom(SpacePage* page, char* out_buf, int out_cap);
int space_page_find_text(SpacePage* page, const char* query, int occurrence_idx, int* out_y);
void space_page_set_focused(SpacePage* page, int focused);

#ifdef __cplusplus
}
#endif
