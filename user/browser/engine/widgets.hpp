#pragma once

#include "canvas2d.hpp"
#include "html_tag.h"
#include "render_image.h"
#include <cstdint>
#include <string>
#include <vector>

struct SpacePage;

namespace space {

class el_script : public litehtml::html_tag {
  public:
    explicit el_script(const std::shared_ptr<litehtml::document>& doc) : litehtml::html_tag(doc) {}
    void parse_attributes() override;
    void compute_styles(bool recursive = true) override;
};

class el_space_input : public litehtml::html_tag {
  public:
    explicit el_space_input(const std::shared_ptr<litehtml::document>& doc, SpacePage* page);

    std::string input_type() const;
    bool is_hidden() const;
    bool is_checkable() const;
    bool is_button_like() const;
    bool is_text_like() const;

    const std::string& current_value() const { return value_; }
    void set_current_value(const std::string& v, bool mark_dirty = true);
    bool is_checked() const { return checked_; }
    void set_checked(bool c, bool handle_radio_group = false);

    void on_mouse_click(int rel_x, int rel_y);
    bool on_key(int key_code, const char* utf8_text, int mods);

    bool is_replaced() const override;
    void parse_attributes() override;
    void compute_styles(bool recursive = true) override;
    void get_content_size(litehtml::size& sz, litehtml::pixel_t max_width) override;
    void draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
              const litehtml::position* clip,
              const std::shared_ptr<litehtml::render_item>& ri) override;
    std::shared_ptr<litehtml::render_item> create_render_item(
        const std::shared_ptr<litehtml::render_item>& parent_ri) override;

  private:
    SpacePage* page_ = nullptr;
    std::string value_;
    bool value_dirty_ = false;
    bool checked_ = false;
    bool checked_dirty_ = false;
    size_t caret_ = 0;

    std::string display_text() const;
    void uncheck_radio_siblings();
};

class el_space_textarea : public litehtml::html_tag {
  public:
    explicit el_space_textarea(const std::shared_ptr<litehtml::document>& doc, SpacePage* page);

    const std::string& current_value() const;
    void set_current_value(const std::string& v);
    bool on_key(int key_code, const char* utf8_text, int mods);
    void on_mouse_click(int rel_x, int rel_y);

    bool is_replaced() const override { return true; }
    void parse_attributes() override;
    void get_content_size(litehtml::size& sz, litehtml::pixel_t max_width) override;
    void draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
              const litehtml::position* clip,
              const std::shared_ptr<litehtml::render_item>& ri) override;
    std::shared_ptr<litehtml::render_item> create_render_item(
        const std::shared_ptr<litehtml::render_item>& parent_ri) override;

  private:
    SpacePage* page_ = nullptr;
    mutable std::string value_;
    bool value_dirty_ = false;
    size_t caret_ = 0;
};

class el_space_button : public litehtml::html_tag {
  public:
    explicit el_space_button(const std::shared_ptr<litehtml::document>& doc, SpacePage* page);
    void parse_attributes() override;
    void draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
              const litehtml::position* clip,
              const std::shared_ptr<litehtml::render_item>& ri) override;
    void on_click() override;
    std::string current_value() const;
    void set_current_value(const std::string& v);

  private:
    SpacePage* page_ = nullptr;
};

struct SelectOptionItem {
    std::string value;
    std::string label;
    bool disabled = false;
};

class el_space_select : public litehtml::html_tag {
  public:
    explicit el_space_select(const std::shared_ptr<litehtml::document>& doc, SpacePage* page);

    void rebuild_options() const;
    const std::vector<SelectOptionItem>& options() const;
    int selected_index() const;
    void set_selected_index(int idx);
    std::string current_value() const;
    void set_current_value(const std::string& v);

    void on_mouse_click(const litehtml::position& abs_pos);

    bool is_replaced() const override { return true; }
    void parse_attributes() override;
    void get_content_size(litehtml::size& sz, litehtml::pixel_t max_width) override;
    void draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
              const litehtml::position* clip,
              const std::shared_ptr<litehtml::render_item>& ri) override;
    std::shared_ptr<litehtml::render_item> create_render_item(
        const std::shared_ptr<litehtml::render_item>& parent_ri) override;

  private:
    SpacePage* page_ = nullptr;
    mutable std::vector<SelectOptionItem> options_;
    mutable int selected_index_ = -1;
    mutable bool init_selected_ = false;
};

class el_space_canvas : public litehtml::html_tag {
  public:
    explicit el_space_canvas(const std::shared_ptr<litehtml::document>& doc, SpacePage* page);

    Canvas2D& canvas() { return canvas_; }
    const Canvas2D& canvas() const { return canvas_; }
    void resize_canvas(int w, int h);

    bool is_replaced() const override { return true; }
    void parse_attributes() override;
    void get_content_size(litehtml::size& sz, litehtml::pixel_t max_width) override;
    void draw(litehtml::uint_ptr hdc, litehtml::pixel_t x, litehtml::pixel_t y,
              const litehtml::position* clip,
              const std::shared_ptr<litehtml::render_item>& ri) override;
    std::shared_ptr<litehtml::render_item> create_render_item(
        const std::shared_ptr<litehtml::render_item>& parent_ri) override;

  private:
    SpacePage* page_ = nullptr;
    Canvas2D canvas_{300, 150};
};

} // namespace space
