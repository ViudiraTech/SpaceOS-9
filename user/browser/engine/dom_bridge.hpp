#pragma once

#include "canvas2d.hpp"
#include "litehtml.h"
#include "quickjs.h"
#include <cstdint>
#include <functional>
#include <list>
#include <map>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

struct SpacePage;

namespace space {

struct TimerEntry {
    uint64_t id = 0;
    double fire_at_ms = 0.0;
    double interval_ms = 0.0;
    bool is_interval = false;
    bool is_raf = false;
    bool cancelled = false;
    JSValue func = JS_UNDEFINED;
    std::vector<JSValue> args;
};

struct PendingFetch {
    uint64_t req_id = 0;
    bool is_xhr = false;
    JSValue resolve_fn = JS_UNDEFINED;
    JSValue reject_fn = JS_UNDEFINED;
    JSValue xhr_obj = JS_UNDEFINED;
    std::string req_url;
};

struct ListenerEntry {
    std::string type;
    JSValue func = JS_UNDEFINED;
    bool capture = false;
    bool once = false;
};

struct NodeRecord {
    uint64_t id = 0;
    std::weak_ptr<litehtml::element> wp;
    litehtml::element::ptr detached_pin;
    uint32_t js_wrappers = 0;
    std::vector<ListenerEntry> listeners;
    std::map<std::string, std::string> inline_styles;
    JSValue on_props[12]; // inline on* event properties
    NodeRecord() {
        for (auto& v : on_props) v = JS_UNDEFINED;
    }
};

class DomBridge {
  public:
    explicit DomBridge(SpacePage* page);
    ~DomBridge();

    DomBridge(const DomBridge&) = delete;
    DomBridge& operator=(const DomBridge&) = delete;

    void reset_context();
    void execute_document_scripts();
    void run_script(const std::string& code, const std::string& filename);
    std::string eval_repl(const std::string& code);

    uint64_t intern_node(const litehtml::element::ptr& el);
    litehtml::element::ptr find_node(uint64_t id);
    JSValue wrap_node(const litehtml::element::ptr& el);
    JSValue wrap_canvas_ctx(const litehtml::element::ptr& el);

    bool dispatch_ui_event(const litehtml::element::ptr& target, const std::string& type,
                           bool bubbles, bool cancelable, int x = 0, int y = 0,
                           int button = 0, int key_code = 0, const std::string& key = "",
                           bool ctrl = false, bool shift = false, bool alt = false);
    void dispatch_window_event(const std::string& type);

    void tick(double now_ms);
    void resolve_fetch(uint64_t req_id, int status, const char* resolved_url,
                       const char* headers_json, const char* body, int body_len,
                       const char* err);

    std::string serialize_element_html(const litehtml::element::ptr& el, bool outer, int depth = 0);
    std::string serialize_text_content(const litehtml::element::ptr& el);
    std::string dump_dom_summary();

    SpacePage* page() const { return page_; }
    JSContext* ctx() const { return ctx_; }
    JSRuntime* rt() const { return rt_; }

    // Watchdog budget per invocation slice
    void begin_watchdog(double budget_ms = 1500.0);
    bool check_watchdog_expired() const;
    void end_watchdog();

    // Class IDs
    JSClassID cls_node() const { return cls_node_; }
    JSClassID cls_style() const { return cls_style_; }
    JSClassID cls_classlist() const { return cls_classlist_; }
    JSClassID cls_canvas_ctx() const { return cls_canvas_ctx_; }
    JSClassID cls_xhr() const { return cls_xhr_; }

    void flush_jobs();
    void notify_console(int level, const std::string& msg);
    void report_exception();
    void sync_node_inline_style(const litehtml::element::ptr& el, NodeRecord& rec);
    NodeRecord* ensure_record(const litehtml::element::ptr& el);
    NodeRecord* find_record(uint64_t id);
    NodeRecord* find_record_by_el(const litehtml::element::ptr& el);

    // Public timer / fetch registries for native bindings
    uint64_t add_timer(TimerEntry&& entry);
    void cancel_timer(uint64_t id);
    void add_pending_fetch(PendingFetch&& pf);

    static std::string js_to_std_string(JSContext* ctx, JSValueConst val);
    static std::string normalize_css_prop(const std::string& name);
    static void collect_elements_by_selector(const litehtml::element::ptr& root,
                                             const std::string& sel,
                                             std::vector<litehtml::element::ptr>& out,
                                             bool first_only);

  private:
    SpacePage* page_ = nullptr;
    JSRuntime* rt_ = nullptr;
    JSContext* ctx_ = nullptr;

    JSClassID cls_node_ = 0;
    JSClassID cls_style_ = 0;
    JSClassID cls_classlist_ = 0;
    JSClassID cls_canvas_ctx_ = 0;
    JSClassID cls_xhr_ = 0;

    uint64_t next_node_id_ = 1;
    uint64_t next_timer_id_ = 1;
    std::unordered_map<const litehtml::element*, uint64_t> ptr_to_id_;
    std::unordered_map<uint64_t, NodeRecord> nodes_;
    std::vector<ListenerEntry> window_listeners_;
    std::vector<ListenerEntry> document_listeners_;
    std::vector<TimerEntry> timers_;
    std::vector<PendingFetch> fetches_;

    bool watchdog_active_ = false;
    double watchdog_deadline_ms_ = 0.0;

    void install_globals();
    void install_dom_apis();
    void install_bootstrap_polyfills();
    void free_all_handles();
    void run_scripts_in_subtree(const litehtml::element::ptr& root, bool only_once_attr);
};

} // namespace space
