#include "dom_bridge.hpp"
#include "el_text.h"
#include "page_internal.hpp"
#include "widgets.hpp"
#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <sstream>

namespace space {

namespace {

static const char* const kOnEventNames[12] = {
    "click", "change", "input", "submit", "keydown", "keyup",
    "mouseover", "mouseout", "mousedown", "mouseup", "focus", "blur",
};

struct NodeOpaque {
    DomBridge* bridge;
    uint64_t id;
};

struct CanvasOpaque {
    DomBridge* bridge;
    uint64_t id;
};

struct XhrOpaque {
    DomBridge* bridge;
    std::string method = "GET";
    std::string url;
    bool async = true;
    std::vector<std::pair<std::string, std::string>> req_headers;
};

struct WatchdogScope {
    DomBridge* b;
    bool own;
    WatchdogScope(DomBridge* bridge, double budget_ms = 1500.0) : b(bridge), own(false) {
        if (b && !b->check_watchdog_expired()) {
            b->begin_watchdog(budget_ms);
            own = true;
        }
    }
    ~WatchdogScope() {
        if (b && own) b->end_watchdog();
    }
};

int64_t monotonic_ms() {
    auto now = std::chrono::steady_clock::now().time_since_epoch();
    return std::chrono::duration_cast<std::chrono::milliseconds>(now).count();
}

DomBridge* get_bridge(JSContext* ctx) {
    return static_cast<DomBridge*>(JS_GetRuntimeOpaque(JS_GetRuntime(ctx)));
}

void def_fn(JSContext* ctx, JSValueConst obj, const char* name, JSCFunction* fn, int argc = 0) {
    JS_SetPropertyStr(ctx, obj, name, JS_NewCFunction(ctx, fn, name, argc));
}

void def_fn_magic(JSContext* ctx, JSValueConst obj, const char* name, JSCFunctionMagic* fn, int argc, int magic) {
    JS_SetPropertyStr(ctx, obj, name, JS_NewCFunctionMagic(ctx, fn, name, argc, JS_CFUNC_generic_magic, magic));
}

void def_getset(JSContext* ctx, JSValueConst obj, const char* name,
                JSCFunction* getter, JSCFunction* setter = nullptr) {
    JSAtom atom = JS_NewAtom(ctx, name);
    JSValue g = getter ? JS_NewCFunction(ctx, getter, name, 0) : JS_UNDEFINED;
    JSValue s = setter ? JS_NewCFunction(ctx, setter, name, 1) : JS_UNDEFINED;
    JS_DefinePropertyGetSet(ctx, obj, atom, g, s, JS_PROP_CONFIGURABLE);
    JS_FreeAtom(ctx, atom);
}

void def_getset_magic(JSContext* ctx, JSValueConst obj, const char* name, int magic,
                      JSCFunctionMagic* getter, JSCFunctionMagic* setter = nullptr) {
    JSAtom atom = JS_NewAtom(ctx, name);
    JSValue g = getter ? JS_NewCFunctionMagic(ctx, getter, name, 0, JS_CFUNC_generic_magic, magic) : JS_UNDEFINED;
    JSValue s = setter ? JS_NewCFunctionMagic(ctx, setter, name, 1, JS_CFUNC_generic_magic, magic) : JS_UNDEFINED;
    JS_DefinePropertyGetSet(ctx, obj, atom, g, s, JS_PROP_CONFIGURABLE);
    JS_FreeAtom(ctx, atom);
}

template <typename T>
T* unwrap_cls(JSContext* ctx, JSValueConst val, JSClassID id) {
    (void)ctx;
    return static_cast<T*>(JS_GetOpaque(val, id));
}

litehtml::element::ptr unwrap_node(JSContext* ctx, JSValueConst val, DomBridge*& out_bridge, uint64_t* out_id = nullptr) {
    DomBridge* b = get_bridge(ctx);
    out_bridge = b;
    if (!b) return nullptr;
    auto* op = static_cast<NodeOpaque*>(JS_GetOpaque(val, b->cls_node()));
    if (!op) return nullptr;
    if (out_id) *out_id = op->id;
    return b->find_node(op->id);
}

el_space_canvas* unwrap_canvas(JSContext* ctx, JSValueConst val, DomBridge*& out_bridge) {
    DomBridge* b = get_bridge(ctx);
    out_bridge = b;
    if (!b) return nullptr;
    auto* op = static_cast<CanvasOpaque*>(JS_GetOpaque(val, b->cls_canvas_ctx()));
    if (!op) return nullptr;
    auto el = b->find_node(op->id);
    if (!el) return nullptr;
    return dynamic_cast<el_space_canvas*>(el.get());
}

int js_interrupt_handler(JSContext* ctx, void* opaque) {
    auto* bridge = static_cast<DomBridge*>(opaque);
    (void)ctx;
    return (bridge && bridge->check_watchdog_expired()) ? 1 : 0;
}

std::string json_escape(const std::string& s) {
    std::string out = "\"";
    for (unsigned char c : s) {
        switch (c) {
        case '"': out += "\\\""; break;
        case '\\': out += "\\\\"; break;
        case '\n': out += "\\n"; break;
        case '\r': out += "\\r"; break;
        case '\t': out += "\\t"; break;
        default:
            if (c < 0x20) {
                char buf[8];
                std::snprintf(buf, sizeof(buf), "\\u%04x", c);
                out += buf;
            } else {
                out.push_back(static_cast<char>(c));
            }
            break;
        }
    }
    out.push_back('"');
    return out;
}

std::string serialize_style_map(const std::map<std::string, std::string>& m) {
    std::string out;
    for (const auto& kv : m) {
        if (kv.second.empty()) continue;
        out += kv.first;
        out += ": ";
        out += kv.second;
        out += "; ";
    }
    return out;
}

std::map<std::string, std::string> parse_inline_style_attr(const char* raw) {
    std::map<std::string, std::string> out;
    if (!raw) return out;
    std::string s(raw);
    size_t pos = 0;
    while (pos < s.size()) {
        size_t semi = s.find(';', pos);
        if (semi == std::string::npos) semi = s.size();
        std::string decl = trim_ascii(s.substr(pos, semi - pos));
        size_t colon = decl.find(':');
        if (colon != std::string::npos) {
            std::string k = to_lower_ascii(trim_ascii(decl.substr(0, colon)));
            std::string v = trim_ascii(decl.substr(colon + 1));
            if (!k.empty() && !v.empty()) out[k] = v;
        }
        pos = semi + 1;
    }
    return out;
}

std::vector<litehtml::element::ptr> element_children_only(const litehtml::element::ptr& el) {
    std::vector<litehtml::element::ptr> out;
    if (!el) return out;
    for (const auto& ch : el->children()) {
        if (!ch->is_text()) out.push_back(ch);
    }
    return out;
}

litehtml::element::ptr find_doc_body_or_root(const litehtml::document::ptr& doc, const char* want_tag) {
    if (!doc) return nullptr;
    auto r = doc->root();
    if (!r) return nullptr;
    if (want_tag == nullptr) return r;
    if (r->get_tagName() && to_lower_ascii(r->get_tagName()) == want_tag) return r;
    for (const auto& ch : r->children()) {
        if (!ch->is_text() && ch->get_tagName() && to_lower_ascii(ch->get_tagName()) == want_tag) {
            return ch;
        }
    }
    return doc->root()->select_one(want_tag);
}

void trigger_restyle_relayout(DomBridge* b, bool relayout = true) {
    if (!b || !b->page()) return;
    if (relayout) {
        b->page()->restyle_needed = true;
    }
    b->page()->mark_dirty(relayout, true);
}

// =========================================================================
// Console / Alert / Base64
// =========================================================================

JSValue js_console_msg(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv, int level) {
    auto* b = get_bridge(ctx);
    std::string line;
    for (int i = 0; i < argc; ++i) {
        if (i > 0) line.push_back(' ');
        if (JS_IsObject(argv[i]) && !JS_IsNull(argv[i])) {
            JSValue j = JS_JSONStringify(ctx, argv[i], JS_UNDEFINED, JS_UNDEFINED);
            if (!JS_IsException(j) && JS_IsString(j)) {
                line += DomBridge::js_to_std_string(ctx, j);
                JS_FreeValue(ctx, j);
                continue;
            }
            JS_FreeValue(ctx, j);
        }
        line += DomBridge::js_to_std_string(ctx, argv[i]);
    }
    if (b) b->notify_console(level, line);
    return JS_UNDEFINED;
}

JSValue js_alert_dialog(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv, int kind) {
    auto* b = get_bridge(ctx);
    std::string msg = argc > 0 ? DomBridge::js_to_std_string(ctx, argv[0]) : "";
    std::string def = argc > 1 ? DomBridge::js_to_std_string(ctx, argv[1]) : "";
    if (!b || !b->page() || !b->page()->cb.on_alert) {
        if (kind == 1) return JS_NewBool(ctx, 1);
        if (kind == 2) return JS_NewString(ctx, def.c_str());
        return JS_UNDEFINED;
    }
    char* reply = nullptr;
    b->page()->cb.on_alert(b->page()->cb.user_data, kind, msg.c_str(), def.c_str(), &reply);
    if (kind == 0) {
        if (reply && b->page()->cb.free_host_buf) b->page()->cb.free_host_buf(b->page()->cb.user_data, reply);
        return JS_UNDEFINED;
    }
    if (kind == 1) {
        bool ok = (reply && (reply[0] == '1' || reply[0] == 't' || reply[0] == 'y'));
        if (reply && b->page()->cb.free_host_buf) b->page()->cb.free_host_buf(b->page()->cb.user_data, reply);
        return JS_NewBool(ctx, ok ? 1 : 0);
    }
    JSValue res = reply ? JS_NewString(ctx, reply) : JS_NULL;
    if (reply && b->page()->cb.free_host_buf) b->page()->cb.free_host_buf(b->page()->cb.user_data, reply);
    return res;
}

static const char kB64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

JSValue js_btoa(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    if (argc < 1) return JS_NewString(ctx, "");
    std::string in = DomBridge::js_to_std_string(ctx, argv[0]);
    std::string out;
    int val = 0, valb = -6;
    for (unsigned char c : in) {
        val = (val << 8) + c;
        valb += 8;
        while (valb >= 0) {
            out.push_back(kB64[(val >> valb) & 0x3F]);
            valb -= 6;
        }
    }
    if (valb > -6) out.push_back(kB64[((val << 8) >> (valb + 8)) & 0x3F]);
    while (out.size() % 4) out.push_back('=');
    return JS_NewStringLen(ctx, out.data(), out.size());
}

JSValue js_atob(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    if (argc < 1) return JS_NewString(ctx, "");
    std::string in = DomBridge::js_to_std_string(ctx, argv[0]);
    std::vector<int> T(256, -1);
    for (int i = 0; i < 64; i++) T[static_cast<unsigned char>(kB64[i])] = i;
    std::string out;
    int val = 0, valb = -8;
    for (unsigned char c : in) {
        if (T[c] == -1) continue;
        val = (val << 6) + T[c];
        valb += 6;
        if (valb >= 0) {
            out.push_back(static_cast<char>((val >> valb) & 0xFF));
            valb -= 8;
        }
    }
    return JS_NewStringLen(ctx, out.data(), out.size());
}

// =========================================================================
// Timers & rAF & queueMicrotask & performance
// =========================================================================

JSValue js_set_timer(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv, int magic) {
    auto* b = get_bridge(ctx);
    if (!b || argc < 1) return JS_NewInt32(ctx, 0);
    TimerEntry te;
    te.is_interval = (magic == 1);
    te.is_raf = (magic == 2);
    double delay = 0.0;
    if (!te.is_raf && argc >= 2) {
        JS_ToFloat64(ctx, &delay, argv[1]);
        if (delay < 0.0 || std::isnan(delay)) delay = 0.0;
    } else if (te.is_raf) {
        delay = 16.0;
    }
    te.interval_ms = delay;
    double now = (b->page() && b->page()->cb.now_ms)
                     ? b->page()->cb.now_ms(b->page()->cb.user_data)
                     : static_cast<double>(monotonic_ms());
    te.fire_at_ms = now + delay;
    if (JS_IsFunction(ctx, argv[0])) {
        te.func = JS_DupValue(ctx, argv[0]);
        for (int i = 2; i < argc; ++i) {
            te.args.push_back(JS_DupValue(ctx, argv[i]));
        }
    } else {
        std::string code = DomBridge::js_to_std_string(ctx, argv[0]);
        std::string wrapped = "(function(){" + code + "})";
        JSValue fn = JS_Eval(ctx, wrapped.c_str(), wrapped.size(), "<timer>", JS_EVAL_TYPE_GLOBAL);
        if (JS_IsException(fn)) {
            b->report_exception();
            return JS_NewInt32(ctx, 0);
        }
        te.func = fn;
    }
    uint64_t tid = b->add_timer(std::move(te));
    return JS_NewInt64(ctx, static_cast<int64_t>(tid));
}

JSValue js_clear_timer(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    auto* b = get_bridge(ctx);
    if (b && argc >= 1) {
        int64_t tid = 0;
        JS_ToInt64(ctx, &tid, argv[0]);
        if (tid > 0) b->cancel_timer(static_cast<uint64_t>(tid));
    }
    return JS_UNDEFINED;
}

JSValue js_queue_microtask(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    if (argc < 1 || !JS_IsFunction(ctx, argv[0])) return JS_UNDEFINED;
    JSValue global = JS_GetGlobalObject(ctx);
    JSValue p_ctor = JS_GetPropertyStr(ctx, global, "Promise");
    JSValue res_fn = JS_GetPropertyStr(ctx, p_ctor, "resolve");
    JSValue p = JS_Call(ctx, res_fn, p_ctor, 0, nullptr);
    JSValue then_fn = JS_GetPropertyStr(ctx, p, "then");
    JSValue r = JS_Call(ctx, then_fn, p, 1, argv);
    JS_FreeValue(ctx, r);
    JS_FreeValue(ctx, then_fn);
    JS_FreeValue(ctx, p);
    JS_FreeValue(ctx, res_fn);
    JS_FreeValue(ctx, p_ctor);
    JS_FreeValue(ctx, global);
    return JS_UNDEFINED;
}

JSValue js_perf_now(JSContext* ctx, JSValueConst /*this_val*/, int /*argc*/, JSValueConst* /*argv*/) {
    auto* b = get_bridge(ctx);
    double t = (b && b->page() && b->page()->cb.now_ms)
                   ? b->page()->cb.now_ms(b->page()->cb.user_data)
                   : static_cast<double>(monotonic_ms());
    return JS_NewFloat64(ctx, t);
}

// =========================================================================
// Location / History / Storage / Cookies / Fetch / XHR
// =========================================================================

JSValue js_location_get_href(JSContext* ctx, JSValueConst /*this_val*/, int /*argc*/, JSValueConst* /*argv*/) {
    auto* b = get_bridge(ctx);
    return JS_NewString(ctx, (b && b->page()) ? b->page()->url.c_str() : "about:blank");
}

JSValue js_location_set_href(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    auto* b = get_bridge(ctx);
    if (b && b->page() && b->page()->cb.on_navigate && argc >= 1) {
        std::string target = DomBridge::js_to_std_string(ctx, argv[0]);
        std::string resolved = resolve_url(b->page()->base_url, target);
        b->page()->cb.on_navigate(b->page()->cb.user_data, resolved.c_str(), 0);
    }
    return JS_UNDEFINED;
}

JSValue js_location_replace(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    auto* b = get_bridge(ctx);
    if (b && b->page() && b->page()->cb.on_navigate && argc >= 1) {
        std::string target = DomBridge::js_to_std_string(ctx, argv[0]);
        std::string resolved = resolve_url(b->page()->base_url, target);
        b->page()->cb.on_navigate(b->page()->cb.user_data, resolved.c_str(), 1);
    }
    return JS_UNDEFINED;
}

JSValue js_location_reload(JSContext* ctx, JSValueConst /*this_val*/, int /*argc*/, JSValueConst* /*argv*/) {
    auto* b = get_bridge(ctx);
    if (b && b->page() && b->page()->cb.on_navigate) {
        b->page()->cb.on_navigate(b->page()->cb.user_data, b->page()->url.c_str(), 1);
    }
    return JS_UNDEFINED;
}

JSValue js_history_op(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv, int op) {
    auto* b = get_bridge(ctx);
    if (!b || !b->page() || !b->page()->cb.on_history_op) return JS_UNDEFINED;
    int delta = 0;
    std::string title, next_url;
    if (op == 0) delta = -1;
    else if (op == 1) delta = 1;
    else if (op == 2 && argc >= 1) JS_ToInt32(ctx, &delta, argv[0]);
    else if ((op == 3 || op == 4)) {
        if (argc >= 2) title = DomBridge::js_to_std_string(ctx, argv[1]);
        if (argc >= 3 && !JS_IsNull(argv[2]) && !JS_IsUndefined(argv[2])) {
            next_url = resolve_url(b->page()->base_url, DomBridge::js_to_std_string(ctx, argv[2]));
            b->page()->url = next_url;
            b->page()->base_url = next_url;
        }
    }
    b->page()->cb.on_history_op(b->page()->cb.user_data, op, delta, next_url.c_str(), title.c_str());
    return JS_UNDEFINED;
}

JSValue js_storage_fn(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    auto* b = get_bridge(ctx);
    if (!b || !b->page()) return JS_UNDEFINED;
    JSValue sess_v = JS_GetPropertyStr(ctx, this_val, "__isSession");
    int is_sess = JS_ToBool(ctx, sess_v);
    JS_FreeValue(ctx, sess_v);
    const auto& cb = b->page()->cb;
    switch (magic) {
    case 0: { // getItem
        if (argc < 1 || !cb.storage_get) return JS_NULL;
        std::string k = DomBridge::js_to_std_string(ctx, argv[0]);
        char* val = cb.storage_get(cb.user_data, is_sess, k.c_str());
        if (!val) return JS_NULL;
        JSValue r = JS_NewString(ctx, val);
        if (cb.free_host_buf) cb.free_host_buf(cb.user_data, val);
        return r;
    }
    case 1: { // setItem
        if (argc >= 2 && cb.storage_set) {
            std::string k = DomBridge::js_to_std_string(ctx, argv[0]);
            std::string v = DomBridge::js_to_std_string(ctx, argv[1]);
            cb.storage_set(cb.user_data, is_sess, k.c_str(), v.c_str());
        }
        return JS_UNDEFINED;
    }
    case 2: { // removeItem
        if (argc >= 1 && cb.storage_remove) {
            std::string k = DomBridge::js_to_std_string(ctx, argv[0]);
            cb.storage_remove(cb.user_data, is_sess, k.c_str());
        }
        return JS_UNDEFINED;
    }
    case 3: { // clear
        if (cb.storage_clear) cb.storage_clear(cb.user_data, is_sess);
        return JS_UNDEFINED;
    }
    case 4: { // key
        if (argc < 1 || !cb.storage_key) return JS_NULL;
        int idx = 0;
        JS_ToInt32(ctx, &idx, argv[0]);
        char* val = cb.storage_key(cb.user_data, is_sess, idx);
        if (!val) return JS_NULL;
        JSValue r = JS_NewString(ctx, val);
        if (cb.free_host_buf) cb.free_host_buf(cb.user_data, val);
        return r;
    }
    case 5: { // length getter
        int len = cb.storage_len ? cb.storage_len(cb.user_data, is_sess) : 0;
        return JS_NewInt32(ctx, len);
    }
    default:
        return JS_UNDEFINED;
    }
}

JSValue js_cookie_get(JSContext* ctx, JSValueConst /*this_val*/, int /*argc*/, JSValueConst* /*argv*/) {
    auto* b = get_bridge(ctx);
    if (!b || !b->page() || !b->page()->cb.cookies_get) return JS_NewString(ctx, "");
    char* s = b->page()->cb.cookies_get(b->page()->cb.user_data);
    if (!s) return JS_NewString(ctx, "");
    JSValue r = JS_NewString(ctx, s);
    if (b->page()->cb.free_host_buf) b->page()->cb.free_host_buf(b->page()->cb.user_data, s);
    return r;
}

JSValue js_cookie_set(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    auto* b = get_bridge(ctx);
    if (b && b->page() && b->page()->cb.cookies_set && argc >= 1) {
        std::string kv = DomBridge::js_to_std_string(ctx, argv[0]);
        b->page()->cb.cookies_set(b->page()->cb.user_data, kv.c_str());
    }
    return JS_UNDEFINED;
}

std::string serialize_headers_obj(JSContext* ctx, JSValueConst hdrs) {
    if (JS_IsUndefined(hdrs) || JS_IsNull(hdrs) || !JS_IsObject(hdrs)) return "{}";
    JSValue j = JS_JSONStringify(ctx, hdrs, JS_UNDEFINED, JS_UNDEFINED);
    if (JS_IsException(j) || !JS_IsString(j)) {
        JS_FreeValue(ctx, j);
        return "{}";
    }
    std::string out = DomBridge::js_to_std_string(ctx, j);
    JS_FreeValue(ctx, j);
    return out;
}

JSValue make_response_object(JSContext* ctx, int status, const std::string& resolved_url,
                             const std::string& headers_json, const std::string& body) {
    JSValue resp = JS_NewObject(ctx);
    JS_SetPropertyStr(ctx, resp, "status", JS_NewInt32(ctx, status));
    JS_SetPropertyStr(ctx, resp, "ok", JS_NewBool(ctx, status >= 200 && status < 300));
    JS_SetPropertyStr(ctx, resp, "statusText", JS_NewString(ctx, (status >= 200 && status < 300) ? "OK" : "Error"));
    JS_SetPropertyStr(ctx, resp, "url", JS_NewString(ctx, resolved_url.c_str()));
    JS_SetPropertyStr(ctx, resp, "_body", JS_NewStringLen(ctx, body.data(), body.size()));
    JSValue hdr_map = JS_ParseJSON(ctx, headers_json.c_str(), headers_json.size(), "<headers>");
    if (JS_IsException(hdr_map) || !JS_IsObject(hdr_map)) {
        JS_FreeValue(ctx, hdr_map);
        hdr_map = JS_NewObject(ctx);
    }
    JS_SetPropertyStr(ctx, resp, "_headers", hdr_map);

    const char* helper_code =
        "(function(r){"
        "r.headers={get(k){k=String(k).toLowerCase();for(var p in r._headers){if(p.toLowerCase()===k)return r._headers[p];}return null;},"
        "has(k){return this.get(k)!==null;}};"
        "r.text=function(){return Promise.resolve(this._body);};"
        "r.json=function(){return Promise.resolve().then(()=>JSON.parse(this._body));};"
        "return r;})";
    JSValue helper = JS_Eval(ctx, helper_code, std::strlen(helper_code), "<response>", JS_EVAL_TYPE_GLOBAL);
    JSValue final_resp = JS_Call(ctx, helper, JS_UNDEFINED, 1, &resp);
    JS_FreeValue(ctx, helper);
    JS_FreeValue(ctx, resp);
    return final_resp;
}

JSValue js_fetch(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    auto* b = get_bridge(ctx);
    if (!b || !b->page() || argc < 1) {
        return JS_ThrowTypeError(ctx, "fetch requires URL");
    }
    std::string raw_url = DomBridge::js_to_std_string(ctx, argv[0]);
    std::string url = resolve_url(b->page()->base_url, raw_url);
    std::string method = "GET";
    std::string headers_json = "{}";
    std::string body;

    if (argc >= 2 && JS_IsObject(argv[1])) {
        JSValue mv = JS_GetPropertyStr(ctx, argv[1], "method");
        if (!JS_IsUndefined(mv) && !JS_IsNull(mv)) method = DomBridge::js_to_std_string(ctx, mv);
        JS_FreeValue(ctx, mv);
        JSValue hv = JS_GetPropertyStr(ctx, argv[1], "headers");
        headers_json = serialize_headers_obj(ctx, hv);
        JS_FreeValue(ctx, hv);
        JSValue bv = JS_GetPropertyStr(ctx, argv[1], "body");
        if (!JS_IsUndefined(bv) && !JS_IsNull(bv)) body = DomBridge::js_to_std_string(ctx, bv);
        JS_FreeValue(ctx, bv);
    }

    JSValue resolving[2] = {JS_UNDEFINED, JS_UNDEFINED};
    JSValue promise = JS_NewPromiseCapability(ctx, resolving);
    if (JS_IsException(promise)) return promise;

    if (b->page()->cb.fetch_async) {
        uint64_t req_id = b->page()->cb.fetch_async(
            b->page()->cb.user_data, method.c_str(), url.c_str(), headers_json.c_str(),
            body.data(), static_cast<int>(body.size()));
        PendingFetch pf;
        pf.req_id = req_id;
        pf.is_xhr = false;
        pf.resolve_fn = resolving[0];
        pf.reject_fn = resolving[1];
        pf.req_url = url;
        b->add_pending_fetch(std::move(pf));
        return promise;
    }

    // Fallback to sync fetch wrapped in resolved promise
    int status = 0, out_len = 0;
    char *out_body = nullptr, *out_url = nullptr, *out_hdrs = nullptr;
    if (b->page()->cb.fetch_sync) {
        b->page()->cb.fetch_sync(b->page()->cb.user_data, method.c_str(), url.c_str(), headers_json.c_str(),
                                 body.data(), static_cast<int>(body.size()),
                                 &status, &out_body, &out_len, &out_url, &out_hdrs);
    }
    std::string rbody = (out_body && out_len > 0) ? std::string(out_body, out_len) : "";
    std::string rurl = (out_url && *out_url) ? out_url : url;
    std::string rhdrs = (out_hdrs && *out_hdrs) ? out_hdrs : "{}";
    if (b->page()->cb.free_host_buf) {
        if (out_body) b->page()->cb.free_host_buf(b->page()->cb.user_data, out_body);
        if (out_url) b->page()->cb.free_host_buf(b->page()->cb.user_data, out_url);
        if (out_hdrs) b->page()->cb.free_host_buf(b->page()->cb.user_data, out_hdrs);
    }
    JSValue resp = make_response_object(ctx, status, rurl, rhdrs, rbody);
    JSValue res = JS_Call(ctx, resolving[0], JS_UNDEFINED, 1, &resp);
    JS_FreeValue(ctx, res);
    JS_FreeValue(ctx, resp);
    JS_FreeValue(ctx, resolving[0]);
    JS_FreeValue(ctx, resolving[1]);
    return promise;
}

JSValue js_xhr_ctor(JSContext* ctx, JSValueConst /*new_target*/, int /*argc*/, JSValueConst* /*argv*/) {
    auto* b = get_bridge(ctx);
    JSValue obj = JS_NewObjectClass(ctx, b->cls_xhr());
    auto* op = new XhrOpaque();
    op->bridge = b;
    JS_SetOpaque(obj, op);
    JS_SetPropertyStr(ctx, obj, "readyState", JS_NewInt32(ctx, 0));
    JS_SetPropertyStr(ctx, obj, "status", JS_NewInt32(ctx, 0));
    JS_SetPropertyStr(ctx, obj, "responseText", JS_NewString(ctx, ""));
    JS_SetPropertyStr(ctx, obj, "response", JS_NewString(ctx, ""));
    JS_SetPropertyStr(ctx, obj, "_resHeaders", JS_NewObject(ctx));
    return obj;
}

JSValue js_xhr_open(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    auto* b = get_bridge(ctx);
    if (!b || !b->page() || argc < 2) return JS_UNDEFINED;
    auto* op = unwrap_cls<XhrOpaque>(ctx, this_val, b->cls_xhr());
    if (!op) return JS_UNDEFINED;
    op->method = DomBridge::js_to_std_string(ctx, argv[0]);
    op->url = resolve_url(b->page()->base_url, DomBridge::js_to_std_string(ctx, argv[1]));
    op->async = (argc >= 3) ? JS_ToBool(ctx, argv[2]) : true;
    op->req_headers.clear();
    JS_SetPropertyStr(ctx, this_val, "readyState", JS_NewInt32(ctx, 1));
    return JS_UNDEFINED;
}

JSValue js_xhr_set_header(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    auto* b = get_bridge(ctx);
    auto* op = b ? unwrap_cls<XhrOpaque>(ctx, this_val, b->cls_xhr()) : nullptr;
    if (op && argc >= 2) {
        op->req_headers.emplace_back(DomBridge::js_to_std_string(ctx, argv[0]),
                                     DomBridge::js_to_std_string(ctx, argv[1]));
    }
    return JS_UNDEFINED;
}

JSValue js_xhr_get_header(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    if (argc < 1) return JS_NULL;
    std::string target = to_lower_ascii(DomBridge::js_to_std_string(ctx, argv[0]));
    JSValue hdrs = JS_GetPropertyStr(ctx, this_val, "_resHeaders");
    JSPropertyEnum* props = nullptr;
    uint32_t len = 0;
    JSValue ret = JS_NULL;
    if (JS_GetOwnPropertyNames(ctx, &props, &len, hdrs, JS_GPN_STRING_MASK | JS_GPN_ENUM_ONLY) == 0) {
        for (uint32_t i = 0; i < len; ++i) {
            const char* key = JS_AtomToCString(ctx, props[i].atom);
            if (key && to_lower_ascii(key) == target) {
                ret = JS_GetProperty(ctx, hdrs, props[i].atom);
                JS_FreeCString(ctx, key);
                break;
            }
            if (key) JS_FreeCString(ctx, key);
        }
        for (uint32_t i = 0; i < len; ++i) JS_FreeAtom(ctx, props[i].atom);
        js_free(ctx, props);
    }
    JS_FreeValue(ctx, hdrs);
    return ret;
}

JSValue js_xhr_send(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    auto* b = get_bridge(ctx);
    auto* op = b ? unwrap_cls<XhrOpaque>(ctx, this_val, b->cls_xhr()) : nullptr;
    if (!op || !b->page()) return JS_UNDEFINED;
    std::string body = (argc >= 1 && !JS_IsNull(argv[0]) && !JS_IsUndefined(argv[0]))
                           ? DomBridge::js_to_std_string(ctx, argv[0])
                           : "";
    std::string hdrs_json = "{";
    for (size_t i = 0; i < op->req_headers.size(); ++i) {
        if (i > 0) hdrs_json += ",";
        hdrs_json += json_escape(op->req_headers[i].first) + ":" + json_escape(op->req_headers[i].second);
    }
    hdrs_json += "}";

    if (op->async && b->page()->cb.fetch_async) {
        uint64_t req_id = b->page()->cb.fetch_async(
            b->page()->cb.user_data, op->method.c_str(), op->url.c_str(),
            hdrs_json.c_str(), body.data(), static_cast<int>(body.size()));
        PendingFetch pf;
        pf.req_id = req_id;
        pf.is_xhr = true;
        pf.xhr_obj = JS_DupValue(ctx, this_val);
        pf.req_url = op->url;
        b->add_pending_fetch(std::move(pf));
        return JS_UNDEFINED;
    }

    int status = 0, out_len = 0;
    char *out_body = nullptr, *out_url = nullptr, *out_hdrs = nullptr;
    if (b->page()->cb.fetch_sync) {
        b->page()->cb.fetch_sync(b->page()->cb.user_data, op->method.c_str(), op->url.c_str(),
                                 hdrs_json.c_str(), body.data(), static_cast<int>(body.size()),
                                 &status, &out_body, &out_len, &out_url, &out_hdrs);
    }
    std::string rbody = (out_body && out_len > 0) ? std::string(out_body, out_len) : "";
    std::string rhdrs = (out_hdrs && *out_hdrs) ? out_hdrs : "{}";
    if (b->page()->cb.free_host_buf) {
        if (out_body) b->page()->cb.free_host_buf(b->page()->cb.user_data, out_body);
        if (out_url) b->page()->cb.free_host_buf(b->page()->cb.user_data, out_url);
        if (out_hdrs) b->page()->cb.free_host_buf(b->page()->cb.user_data, out_hdrs);
    }
    JS_SetPropertyStr(ctx, this_val, "readyState", JS_NewInt32(ctx, 4));
    JS_SetPropertyStr(ctx, this_val, "status", JS_NewInt32(ctx, status));
    JS_SetPropertyStr(ctx, this_val, "responseText", JS_NewStringLen(ctx, rbody.data(), rbody.size()));
    JS_SetPropertyStr(ctx, this_val, "response", JS_NewStringLen(ctx, rbody.data(), rbody.size()));
    JSValue hm = JS_ParseJSON(ctx, rhdrs.c_str(), rhdrs.size(), "<xhr-headers>");
    if (!JS_IsException(hm) && JS_IsObject(hm)) JS_SetPropertyStr(ctx, this_val, "_resHeaders", hm);
    else JS_FreeValue(ctx, hm);

    for (const char* cbname : {"onreadystatechange", "onload"}) {
        JSValue fn = JS_GetPropertyStr(ctx, this_val, cbname);
        if (JS_IsFunction(ctx, fn)) {
            JSValue r = JS_Call(ctx, fn, this_val, 0, nullptr);
            if (JS_IsException(r)) b->report_exception();
            JS_FreeValue(ctx, r);
        }
        JS_FreeValue(ctx, fn);
    }
    return JS_UNDEFINED;
}

// =========================================================================
// CanvasRenderingContext2D bindings
// =========================================================================

JSValue js_canvas_get_set_fill(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    el_space_canvas* cv = unwrap_canvas(ctx, this_val, b);
    if (!cv) return JS_UNDEFINED;
    if (argc == 0) {
        return JS_NewString(ctx, magic == 0 ? "#000000" : cv->canvas().state.font.c_str());
    }
    std::string val = DomBridge::js_to_std_string(ctx, argv[0]);
    if (magic == 0) cv->canvas().set_fill_style(val.c_str());
    else if (magic == 1) cv->canvas().set_stroke_style(val.c_str());
    else if (magic == 2) cv->canvas().set_font(val.c_str());
    else if (magic == 3) {
        double lw = 1.0;
        JS_ToFloat64(ctx, &lw, argv[0]);
        cv->canvas().state.line_width = std::max(0.5, lw);
    } else if (magic == 4) {
        double a = 1.0;
        JS_ToFloat64(ctx, &a, argv[0]);
        cv->canvas().state.global_alpha = std::clamp(a, 0.0, 1.0);
    } else if (magic == 5) {
        cv->canvas().state.text_align = val;
    } else if (magic == 6) {
        cv->canvas().state.text_baseline = val;
    }
    return JS_UNDEFINED;
}

JSValue js_canvas_rect_op(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    el_space_canvas* cv = unwrap_canvas(ctx, this_val, b);
    if (!cv || argc < 4) return JS_UNDEFINED;
    double x = 0, y = 0, w = 0, h = 0;
    JS_ToFloat64(ctx, &x, argv[0]);
    JS_ToFloat64(ctx, &y, argv[1]);
    JS_ToFloat64(ctx, &w, argv[2]);
    JS_ToFloat64(ctx, &h, argv[3]);
    if (magic == 0) cv->canvas().clear_rect(x, y, w, h);
    else if (magic == 1) cv->canvas().fill_rect(x, y, w, h);
    else if (magic == 2) cv->canvas().stroke_rect(x, y, w, h);
    else if (magic == 3) cv->canvas().rect(x, y, w, h);
    if (magic != 3 && b->page()) b->page()->mark_dirty(false, true);
    return JS_UNDEFINED;
}

JSValue js_canvas_path_op(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    el_space_canvas* cv = unwrap_canvas(ctx, this_val, b);
    if (!cv) return JS_UNDEFINED;
    switch (magic) {
    case 0: cv->canvas().begin_path(); break;
    case 1: cv->canvas().close_path(); break;
    case 2: cv->canvas().save(); break;
    case 3: cv->canvas().restore(); break;
    case 4: cv->canvas().reset_transform(); break;
    case 5:
        cv->canvas().fill_path();
        if (b->page()) b->page()->mark_dirty(false, true);
        break;
    case 6:
        cv->canvas().stroke_path();
        if (b->page()) b->page()->mark_dirty(false, true);
        break;
    case 7: { // moveTo
        if (argc >= 2) {
            double x = 0, y = 0;
            JS_ToFloat64(ctx, &x, argv[0]);
            JS_ToFloat64(ctx, &y, argv[1]);
            cv->canvas().move_to(x, y);
        }
        break;
    }
    case 8: { // lineTo
        if (argc >= 2) {
            double x = 0, y = 0;
            JS_ToFloat64(ctx, &x, argv[0]);
            JS_ToFloat64(ctx, &y, argv[1]);
            cv->canvas().line_to(x, y);
        }
        break;
    }
    case 9: { // arc
        if (argc >= 5) {
            double cx = 0, cy = 0, r = 0, a0 = 0, a1 = 0;
            JS_ToFloat64(ctx, &cx, argv[0]);
            JS_ToFloat64(ctx, &cy, argv[1]);
            JS_ToFloat64(ctx, &r, argv[2]);
            JS_ToFloat64(ctx, &a0, argv[3]);
            JS_ToFloat64(ctx, &a1, argv[4]);
            bool ccw = (argc >= 6) ? JS_ToBool(ctx, argv[5]) != 0 : false;
            cv->canvas().arc(cx, cy, r, a0, a1, ccw);
        }
        break;
    }
    case 10: { // quadraticCurveTo
        if (argc >= 4) {
            double cpx = 0, cpy = 0, x = 0, y = 0;
            JS_ToFloat64(ctx, &cpx, argv[0]);
            JS_ToFloat64(ctx, &cpy, argv[1]);
            JS_ToFloat64(ctx, &x, argv[2]);
            JS_ToFloat64(ctx, &y, argv[3]);
            cv->canvas().quadratic_curve_to(cpx, cpy, x, y);
        }
        break;
    }
    case 11: { // bezierCurveTo
        if (argc >= 6) {
            double cp1x = 0, cp1y = 0, cp2x = 0, cp2y = 0, x = 0, y = 0;
            JS_ToFloat64(ctx, &cp1x, argv[0]);
            JS_ToFloat64(ctx, &cp1y, argv[1]);
            JS_ToFloat64(ctx, &cp2x, argv[2]);
            JS_ToFloat64(ctx, &cp2y, argv[3]);
            JS_ToFloat64(ctx, &x, argv[4]);
            JS_ToFloat64(ctx, &y, argv[5]);
            cv->canvas().bezier_curve_to(cp1x, cp1y, cp2x, cp2y, x, y);
        }
        break;
    }
    case 12: { // translate
        if (argc >= 2) {
            double x = 0, y = 0;
            JS_ToFloat64(ctx, &x, argv[0]);
            JS_ToFloat64(ctx, &y, argv[1]);
            cv->canvas().translate(x, y);
        }
        break;
    }
    case 13: { // scale
        if (argc >= 2) {
            double x = 1, y = 1;
            JS_ToFloat64(ctx, &x, argv[0]);
            JS_ToFloat64(ctx, &y, argv[1]);
            cv->canvas().scale(x, y);
        }
        break;
    }
    case 14: { // rotate
        if (argc >= 1) {
            double rad = 0;
            JS_ToFloat64(ctx, &rad, argv[0]);
            cv->canvas().rotate(rad);
        }
        break;
    }
    case 15:
    case 16: { // setTransform / transform
        if (argc >= 6) {
            double a = 1, b0 = 0, c = 0, d = 1, e = 0, f = 0;
            JS_ToFloat64(ctx, &a, argv[0]);
            JS_ToFloat64(ctx, &b0, argv[1]);
            JS_ToFloat64(ctx, &c, argv[2]);
            JS_ToFloat64(ctx, &d, argv[3]);
            JS_ToFloat64(ctx, &e, argv[4]);
            JS_ToFloat64(ctx, &f, argv[5]);
            if (magic == 15) cv->canvas().set_transform(a, b0, c, d, e, f);
            else cv->canvas().transform(a, b0, c, d, e, f);
        }
        break;
    }
    default:
        break;
    }
    return JS_UNDEFINED;
}

JSValue js_canvas_text_op(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int is_measure) {
    DomBridge* b = nullptr;
    el_space_canvas* cv = unwrap_canvas(ctx, this_val, b);
    if (!cv || argc < 1) return JS_UNDEFINED;
    std::string txt = DomBridge::js_to_std_string(ctx, argv[0]);
    if (is_measure) {
        double w = cv->canvas().measure_text(b->page() ? &b->page()->cb : nullptr, txt.c_str());
        JSValue obj = JS_NewObject(ctx);
        JS_SetPropertyStr(ctx, obj, "width", JS_NewFloat64(ctx, w));
        return obj;
    }
    if (argc >= 3) {
        double x = 0, y = 0;
        JS_ToFloat64(ctx, &x, argv[1]);
        JS_ToFloat64(ctx, &y, argv[2]);
        cv->canvas().fill_text(b->page() ? &b->page()->cb : nullptr, txt.c_str(), x, y);
        if (b->page()) b->page()->mark_dirty(false, true);
    }
    return JS_UNDEFINED;
}

// =========================================================================
// Node / Element / Style / ClassList
// =========================================================================

JSValue js_node_attr_op(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || argc < 1) return JS_UNDEFINED;
    std::string name = DomBridge::js_to_std_string(ctx, argv[0]);
    std::string lname = to_lower_ascii(name);
    if (magic == 0) { // getAttribute
        if (lname == "style") {
            if (auto* rec = b->find_record_by_el(el)) {
                if (!rec->inline_styles.empty()) {
                    return JS_NewString(ctx, serialize_style_map(rec->inline_styles).c_str());
                }
            }
        }
        const char* v = el->get_attr(lname.c_str());
        return v ? JS_NewString(ctx, v) : JS_NULL;
    }
    if (magic == 1 && argc >= 2) { // setAttribute
        std::string val = DomBridge::js_to_std_string(ctx, argv[1]);
        el->set_attr(lname.c_str(), val.c_str());
        if (lname == "style") {
            if (auto* rec = b->ensure_record(el)) {
                rec->inline_styles = parse_inline_style_attr(val.c_str());
            }
        }
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        return JS_UNDEFINED;
    }
    if (magic == 2) { // removeAttribute
        el->remove_attr(lname.c_str());
        if (lname == "style") {
            if (auto* rec = b->find_record_by_el(el)) rec->inline_styles.clear();
        }
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        return JS_UNDEFINED;
    }
    if (magic == 3) { // hasAttribute
        return JS_NewBool(ctx, el->get_attr(lname.c_str()) != nullptr);
    }
    return JS_UNDEFINED;
}

JSValue js_style_raw_get(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    DomBridge* b = get_bridge(ctx);
    if (!b || argc < 1) return JS_NewString(ctx, "");
    JSValue id_v = JS_GetPropertyStr(ctx, this_val, "__nid");
    uint64_t nid = 0;
    int64_t tmp = 0;
    JS_ToInt64(ctx, &tmp, id_v);
    JS_FreeValue(ctx, id_v);
    nid = static_cast<uint64_t>(tmp);
    auto el = b->find_node(nid);
    if (!el) return JS_NewString(ctx, "");
    std::string prop = DomBridge::js_to_std_string(ctx, argv[0]);
    auto* rec = b->ensure_record(el);
    if (prop == "cssText") return JS_NewString(ctx, serialize_style_map(rec->inline_styles).c_str());
    std::string k = DomBridge::normalize_css_prop(prop);
    auto it = rec->inline_styles.find(k);
    if (it != rec->inline_styles.end()) return JS_NewString(ctx, it->second.c_str());
    return JS_NewString(ctx, "");
}

JSValue js_style_raw_set(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    DomBridge* b = get_bridge(ctx);
    if (!b || argc < 2) return JS_UNDEFINED;
    JSValue id_v = JS_GetPropertyStr(ctx, this_val, "__nid");
    int64_t tmp = 0;
    JS_ToInt64(ctx, &tmp, id_v);
    JS_FreeValue(ctx, id_v);
    auto el = b->find_node(static_cast<uint64_t>(tmp));
    if (!el) return JS_UNDEFINED;
    std::string prop = DomBridge::js_to_std_string(ctx, argv[0]);
    std::string val = DomBridge::js_to_std_string(ctx, argv[1]);
    auto* rec = b->ensure_record(el);
    if (prop == "cssText") {
        rec->inline_styles = parse_inline_style_attr(val.c_str());
    } else {
        std::string k = DomBridge::normalize_css_prop(prop);
        if (val.empty()) rec->inline_styles.erase(k);
        else rec->inline_styles[k] = val;
    }
    b->sync_node_inline_style(el, *rec);
    return JS_UNDEFINED;
}

JSValue js_node_get_style(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/) {
    DomBridge* b = nullptr;
    uint64_t nid = 0;
    auto el = unwrap_node(ctx, this_val, b, &nid);
    if (!el || !b) return JS_UNDEFINED;
    JSValue raw = JS_NewObject(ctx);
    JS_SetPropertyStr(ctx, raw, "__nid", JS_NewInt64(ctx, static_cast<int64_t>(nid)));
    def_fn(ctx, raw, "__get", js_style_raw_get, 1);
    def_fn(ctx, raw, "__set", js_style_raw_set, 2);
    JSValue g = JS_GetGlobalObject(ctx);
    JSValue wrap = JS_GetPropertyStr(ctx, g, "__makeStyleProxy");
    JSValue ret = JS_Call(ctx, wrap, g, 1, &raw);
    JS_FreeValue(ctx, wrap);
    JS_FreeValue(ctx, g);
    JS_FreeValue(ctx, raw);
    return ret;
}

std::vector<std::string> split_classes(const char* raw) {
    std::vector<std::string> out;
    if (!raw) return out;
    std::istringstream iss(raw);
    std::string tok;
    while (iss >> tok) out.push_back(tok);
    return out;
}

std::string join_classes(const std::vector<std::string>& cls) {
    std::string out;
    for (size_t i = 0; i < cls.size(); ++i) {
        if (i > 0) out.push_back(' ');
        out += cls[i];
    }
    return out;
}

JSValue js_classlist_op(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = get_bridge(ctx);
    if (!b) return JS_UNDEFINED;
    JSValue id_v = JS_GetPropertyStr(ctx, this_val, "__nid");
    int64_t tmp = 0;
    JS_ToInt64(ctx, &tmp, id_v);
    JS_FreeValue(ctx, id_v);
    auto el = b->find_node(static_cast<uint64_t>(tmp));
    if (!el) return JS_UNDEFINED;
    auto cls = split_classes(el->get_attr("class"));

    if (magic == 0) { // contains
        if (argc < 1) return JS_FALSE;
        std::string target = DomBridge::js_to_std_string(ctx, argv[0]);
        return JS_NewBool(ctx, std::find(cls.begin(), cls.end(), target) != cls.end());
    }
    if (magic == 1) { // add
        for (int i = 0; i < argc; ++i) {
            std::string t = DomBridge::js_to_std_string(ctx, argv[i]);
            if (!t.empty() && std::find(cls.begin(), cls.end(), t) == cls.end()) cls.push_back(t);
        }
        el->set_attr("class", join_classes(cls).c_str());
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        return JS_UNDEFINED;
    }
    if (magic == 2) { // remove
        for (int i = 0; i < argc; ++i) {
            std::string t = DomBridge::js_to_std_string(ctx, argv[i]);
            cls.erase(std::remove(cls.begin(), cls.end(), t), cls.end());
        }
        el->set_attr("class", join_classes(cls).c_str());
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        return JS_UNDEFINED;
    }
    if (magic == 3) { // toggle
        if (argc < 1) return JS_FALSE;
        std::string t = DomBridge::js_to_std_string(ctx, argv[0]);
        auto it = std::find(cls.begin(), cls.end(), t);
        bool force_present = (argc >= 2) ? (JS_ToBool(ctx, argv[1]) != 0) : (it == cls.end());
        if (force_present && it == cls.end()) cls.push_back(t);
        else if (!force_present && it != cls.end()) cls.erase(it);
        el->set_attr("class", join_classes(cls).c_str());
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        return JS_NewBool(ctx, force_present);
    }
    if (magic == 4) { // toString
        return JS_NewString(ctx, join_classes(cls).c_str());
    }
    return JS_UNDEFINED;
}

JSValue js_node_get_classlist(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/) {
    DomBridge* b = nullptr;
    uint64_t nid = 0;
    auto el = unwrap_node(ctx, this_val, b, &nid);
    if (!el) return JS_UNDEFINED;
    JSValue cl = JS_NewObject(ctx);
    JS_SetPropertyStr(ctx, cl, "__nid", JS_NewInt64(ctx, static_cast<int64_t>(nid)));
    def_fn_magic(ctx, cl, "contains", js_classlist_op, 1, 0);
    def_fn_magic(ctx, cl, "add", js_classlist_op, 1, 1);
    def_fn_magic(ctx, cl, "remove", js_classlist_op, 1, 2);
    def_fn_magic(ctx, cl, "toggle", js_classlist_op, 1, 3);
    def_fn_magic(ctx, cl, "toString", js_classlist_op, 0, 4);
    return cl;
}

JSValue js_node_prop_str_get(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b) return JS_UNDEFINED;
    switch (magic) {
    case 0: return JS_NewString(ctx, (el->get_attr("id") ? el->get_attr("id") : ""));
    case 1: return JS_NewString(ctx, (el->get_attr("class") ? el->get_attr("class") : ""));
    case 2: return JS_NewString(ctx, b->serialize_element_html(el, false).c_str());
    case 3: return JS_NewString(ctx, b->serialize_element_html(el, true).c_str());
    case 4:
    case 5: return JS_NewString(ctx, b->serialize_text_content(el).c_str());
    case 6: { // value
        if (auto* inp = dynamic_cast<el_space_input*>(el.get())) return JS_NewString(ctx, inp->current_value().c_str());
        if (auto* ta = dynamic_cast<el_space_textarea*>(el.get())) return JS_NewString(ctx, ta->current_value().c_str());
        if (auto* sel = dynamic_cast<el_space_select*>(el.get())) return JS_NewString(ctx, sel->current_value().c_str());
        if (auto* btn = dynamic_cast<el_space_button*>(el.get())) return JS_NewString(ctx, btn->current_value().c_str());
        const char* v = el->get_attr("value");
        return JS_NewString(ctx, v ? v : "");
    }
    case 7: return JS_NewString(ctx, el->get_attr("src") ? el->get_attr("src") : "");
    case 8: return JS_NewString(ctx, el->get_attr("href") ? el->get_attr("href") : "");
    case 9: {
        std::string t = el->is_text() ? "#text" : (el->get_tagName() ? el->get_tagName() : "");
        for (char& ch : t) ch = static_cast<char>(std::toupper(static_cast<unsigned char>(ch)));
        return JS_NewString(ctx, t.c_str());
    }
    default: return JS_UNDEFINED;
    }
}

JSValue js_node_prop_str_set(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b || argc < 1) return JS_UNDEFINED;
    std::string val = DomBridge::js_to_std_string(ctx, argv[0]);
    switch (magic) {
    case 0:
        el->set_attr("id", val.c_str());
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        break;
    case 1:
        el->set_attr("class", val.c_str());
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        break;
    case 2: { // innerHTML
        if (b->page() && b->page()->doc && !el->is_text()) {
            b->page()->doc->append_children_from_string(*el, val.c_str(), true);
            trigger_restyle_relayout(b, true);
            // Execute any scripts inside inserted HTML
            std::vector<litehtml::element::ptr> stk;
            for (const auto& ch : el->children()) stk.push_back(ch);
            for (size_t i = 0; i < stk.size(); ++i) {
                auto cur = stk[i];
                if (!cur->is_text() && cur->get_tagName() && to_lower_ascii(cur->get_tagName()) == "script") {
                    std::string code;
                    cur->get_text(code);
                    if (!trim_ascii(code).empty()) b->run_script(code, "<innerHTML-script>");
                }
                for (const auto& gc : cur->children()) stk.push_back(gc);
            }
        }
        break;
    }
    case 4:
    case 5: { // textContent / innerText
        if (el->is_text()) {
            if (auto* txt = dynamic_cast<litehtml::el_text*>(el.get())) {
                txt->set_text(val.c_str());
            }
        } else {
            auto old_children = el->children();
            for (const auto& ch : old_children) el->removeChild(ch);
            if (!val.empty() && b->page() && b->page()->doc) {
                auto tn = std::make_shared<litehtml::el_text>(val.c_str(), b->page()->doc);
                el->appendChild(tn);
            }
        }
        trigger_restyle_relayout(b, true);
        break;
    }
    case 6: { // value
        if (auto* inp = dynamic_cast<el_space_input*>(el.get())) inp->set_current_value(val, true);
        else if (auto* ta = dynamic_cast<el_space_textarea*>(el.get())) ta->set_current_value(val);
        else if (auto* sel = dynamic_cast<el_space_select*>(el.get())) sel->set_current_value(val);
        else if (auto* btn = dynamic_cast<el_space_button*>(el.get())) btn->set_current_value(val);
        else el->set_attr("value", val.c_str());
        break;
    }
    case 7:
        el->set_attr("src", val.c_str());
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        break;
    case 8:
        el->set_attr("href", val.c_str());
        el->parse_attributes();
        trigger_restyle_relayout(b, true);
        break;
    default:
        break;
    }
    return JS_UNDEFINED;
}

JSValue js_node_checked_get(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el) return JS_FALSE;
    if (auto* inp = dynamic_cast<el_space_input*>(el.get())) return JS_NewBool(ctx, inp->is_checked());
    return JS_NewBool(ctx, el->get_attr("checked") != nullptr);
}

JSValue js_node_checked_set(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || argc < 1) return JS_UNDEFINED;
    bool c = JS_ToBool(ctx, argv[0]) != 0;
    if (auto* inp = dynamic_cast<el_space_input*>(el.get())) {
        inp->set_checked(c, true);
    } else if (c) el->set_attr("checked", "");
    else el->remove_attr("checked");
    return JS_UNDEFINED;
}

JSValue js_node_disabled_get(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    return JS_NewBool(ctx, el && el->get_attr("disabled") != nullptr);
}

JSValue js_node_disabled_set(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || argc < 1) return JS_UNDEFINED;
    if (JS_ToBool(ctx, argv[0])) el->set_attr("disabled", "");
    else el->remove_attr("disabled");
    if (b && b->page()) b->page()->mark_dirty(false, true);
    return JS_UNDEFINED;
}

JSValue js_node_selidx_get(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (auto* sel = dynamic_cast<el_space_select*>(el.get())) return JS_NewInt32(ctx, sel->selected_index());
    return JS_NewInt32(ctx, -1);
}

JSValue js_node_selidx_set(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (auto* sel = dynamic_cast<el_space_select*>(el.get())) {
        if (argc >= 1) {
            int idx = -1;
            JS_ToInt32(ctx, &idx, argv[0]);
            sel->set_selected_index(idx);
        }
    }
    return JS_UNDEFINED;
}

JSValue js_node_dim_get(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el) return JS_NewInt32(ctx, 0);
    if (auto* cv = dynamic_cast<el_space_canvas*>(el.get())) {
        if (magic == 0) return JS_NewInt32(ctx, cv->canvas().width);
        if (magic == 1) return JS_NewInt32(ctx, cv->canvas().height);
    }
    if (b && b->page()) b->page()->ensure_layout();
    auto p = el->get_placement();
    if (magic == 0 || magic == 2) return JS_NewInt32(ctx, static_cast<int>(p.width.value()));
    if (magic == 1 || magic == 3) return JS_NewInt32(ctx, static_cast<int>(p.height.value()));
    if (magic == 4) return JS_NewInt32(ctx, static_cast<int>(p.x.value()));
    if (magic == 5) return JS_NewInt32(ctx, static_cast<int>(p.y.value()));
    return JS_NewInt32(ctx, 0);
}

JSValue js_node_dim_set(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int is_height) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || argc < 1) return JS_UNDEFINED;
    int32_t v = 0;
    JS_ToInt32(ctx, &v, argv[0]);
    if (auto* cv = dynamic_cast<el_space_canvas*>(el.get())) {
        int nw = is_height ? cv->canvas().width : v;
        int nh = is_height ? v : cv->canvas().height;
        cv->resize_canvas(nw, nh);
        char buf[32];
        std::snprintf(buf, sizeof(buf), "%d", v);
        el->set_attr(is_height ? "height" : "width", buf);
    } else {
        char buf[32];
        std::snprintf(buf, sizeof(buf), "%d", v);
        el->set_attr(is_height ? "height" : "width", buf);
        trigger_restyle_relayout(b, true);
    }
    return JS_UNDEFINED;
}

JSValue js_node_rel_get(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b) return JS_NULL;
    switch (magic) {
    case 0: return b->wrap_node(el->parent());
    case 1: { // firstChild
        return el->children().empty() ? JS_NULL : b->wrap_node(el->children().front());
    }
    case 2: { // lastChild
        return el->children().empty() ? JS_NULL : b->wrap_node(el->children().back());
    }
    case 3: { // firstElementChild
        for (const auto& ch : el->children()) if (!ch->is_text()) return b->wrap_node(ch);
        return JS_NULL;
    }
    case 4: { // nextSibling / nextElementSibling
    case 5:
        auto par = el->parent();
        if (!par) return JS_NULL;
        bool found = false;
        for (const auto& sib : par->children()) {
            if (found) {
                if (magic == 4 || !sib->is_text()) return b->wrap_node(sib);
            }
            if (sib.get() == el.get()) found = true;
        }
        return JS_NULL;
    }
    case 6: { // previousSibling / previousElementSibling
    case 7:
        auto par = el->parent();
        if (!par) return JS_NULL;
        litehtml::element::ptr prev;
        for (const auto& sib : par->children()) {
            if (sib.get() == el.get()) return b->wrap_node(prev);
            if (magic == 6 || !sib->is_text()) prev = sib;
        }
        return JS_NULL;
    }
    case 8: { // children
        auto list = element_children_only(el);
        JSValue arr = JS_NewArray(ctx);
        for (size_t i = 0; i < list.size(); ++i) JS_SetPropertyUint32(ctx, arr, static_cast<uint32_t>(i), b->wrap_node(list[i]));
        return arr;
    }
    case 9: { // childNodes
        JSValue arr = JS_NewArray(ctx);
        uint32_t idx = 0;
        for (const auto& ch : el->children()) JS_SetPropertyUint32(ctx, arr, idx++, b->wrap_node(ch));
        return arr;
    }
    case 10: return JS_NewInt32(ctx, el->is_text() ? 3 : 1);
    default: return JS_NULL;
    }
}

JSValue js_node_tree_op(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b) return JS_UNDEFINED;

    if (magic == 0 && argc >= 1) { // appendChild
        DomBridge* b2 = nullptr;
        uint64_t ch_id = 0;
        auto ch = unwrap_node(ctx, argv[0], b2, &ch_id);
        if (ch && ch.get() != el.get()) {
            if (auto old_p = ch->parent()) old_p->removeChild(ch);
            el->appendChild(ch);
            ch->parent(el);
            if (auto* rec = b->find_record(ch_id)) rec->detached_pin.reset();
            trigger_restyle_relayout(b, true);
        }
        return JS_DupValue(ctx, argv[0]);
    }
    if (magic == 1 && argc >= 1) { // removeChild
        DomBridge* b2 = nullptr;
        uint64_t ch_id = 0;
        auto ch = unwrap_node(ctx, argv[0], b2, &ch_id);
        if (ch) {
            el->removeChild(ch);
            ch->parent(nullptr);
            if (auto* rec = b->find_record(ch_id)) {
                if (rec->js_wrappers > 0) rec->detached_pin = ch;
            }
            trigger_restyle_relayout(b, true);
        }
        return JS_DupValue(ctx, argv[0]);
    }
    if (magic == 2 && argc >= 1) { // insertBefore(newNode, refNode)
        DomBridge* b2 = nullptr;
        uint64_t nn_id = 0;
        auto nn = unwrap_node(ctx, argv[0], b2, &nn_id);
        auto ref = (argc >= 2 && !JS_IsNull(argv[1]) && !JS_IsUndefined(argv[1])) ? unwrap_node(ctx, argv[1], b2) : nullptr;
        if (nn && nn.get() != el.get()) {
            if (auto old_p = nn->parent()) old_p->removeChild(nn);
            if (!ref) {
                el->appendChild(nn);
            } else {
                std::vector<litehtml::element::ptr> tail;
                bool after = false;
                for (const auto& c : el->children()) {
                    if (c.get() == ref.get()) after = true;
                    if (after) tail.push_back(c);
                }
                for (const auto& t : tail) el->removeChild(t);
                el->appendChild(nn);
                for (const auto& t : tail) el->appendChild(t);
            }
            nn->parent(el);
            if (auto* rec = b->find_record(nn_id)) rec->detached_pin.reset();
            trigger_restyle_relayout(b, true);
        }
        return JS_DupValue(ctx, argv[0]);
    }
    if (magic == 3) { // remove()
        if (auto p = el->parent()) {
            p->removeChild(el);
            el->parent(nullptr);
            if (auto* rec = b->find_record_by_el(el)) {
                if (rec->js_wrappers > 0) rec->detached_pin = el;
            }
            trigger_restyle_relayout(b, true);
        }
        return JS_UNDEFINED;
    }
    if (magic == 4 && argc >= 2) { // replaceChild(newChild, oldChild)
        DomBridge* b2 = nullptr;
        uint64_t nn_id = 0;
        uint64_t oc_id = 0;
        auto nn = unwrap_node(ctx, argv[0], b2, &nn_id);
        auto oc = unwrap_node(ctx, argv[1], b2, &oc_id);
        if (nn && oc) {
            if (auto old_p = nn->parent()) old_p->removeChild(nn);
            std::vector<litehtml::element::ptr> tail;
            bool after = false;
            for (const auto& c : el->children()) {
                if (c.get() == oc.get()) {
                    after = true;
                    continue;
                }
                if (after) tail.push_back(c);
            }
            el->removeChild(oc);
            oc->parent(nullptr);
            if (auto* rec = b->find_record(oc_id)) {
                if (rec->js_wrappers > 0) rec->detached_pin = oc;
            }
            for (const auto& t : tail) el->removeChild(t);
            el->appendChild(nn);
            nn->parent(el);
            if (auto* rec = b->find_record(nn_id)) rec->detached_pin.reset();
            for (const auto& t : tail) el->appendChild(t);
            trigger_restyle_relayout(b, true);
        }
        return JS_DupValue(ctx, argv[1]);
    }
    if (magic == 5 && argc >= 1) { // contains
        DomBridge* b2 = nullptr;
        auto target = unwrap_node(ctx, argv[0], b2);
        auto cur = target;
        while (cur) {
            if (cur.get() == el.get()) return JS_TRUE;
            cur = cur->parent();
        }
        return JS_FALSE;
    }
    return JS_UNDEFINED;
}

JSValue js_node_query(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!b || argc < 1) return (magic == 0 || magic == 4) ? JS_NULL : JS_NewArray(ctx);
    litehtml::element::ptr scope = el ? el : (b->page() && b->page()->doc ? b->page()->doc->root() : nullptr);
    if (!scope) return (magic == 0 || magic == 4) ? JS_NULL : JS_NewArray(ctx);

    std::string q = DomBridge::js_to_std_string(ctx, argv[0]);
    std::vector<litehtml::element::ptr> found;
    if (magic == 0 || magic == 1) { // querySelector / querySelectorAll
        DomBridge::collect_elements_by_selector(scope, q, found, magic == 0);
        if (magic == 0) return found.empty() ? JS_NULL : b->wrap_node(found[0]);
    } else if (magic == 2) { // getElementsByTagName
        std::string ql = to_lower_ascii(q);
        std::vector<litehtml::element::ptr> stk = {scope};
        while (!stk.empty()) {
            auto cur = stk.back();
            stk.pop_back();
            if (cur.get() != scope.get() && !cur->is_text() && cur->get_tagName()) {
                if (ql == "*" || to_lower_ascii(cur->get_tagName()) == ql) found.push_back(cur);
            }
            for (auto it = cur->children().rbegin(); it != cur->children().rend(); ++it) stk.push_back(*it);
        }
    } else if (magic == 3) { // getElementsByClassName
        auto wanted = split_classes(q.c_str());
        std::vector<litehtml::element::ptr> stk = {scope};
        while (!stk.empty()) {
            auto cur = stk.back();
            stk.pop_back();
            if (cur.get() != scope.get() && !cur->is_text()) {
                auto have = split_classes(cur->get_attr("class"));
                bool ok = !wanted.empty();
                for (const auto& w : wanted) {
                    if (std::find(have.begin(), have.end(), w) == have.end()) { ok = false; break; }
                }
                if (ok) found.push_back(cur);
            }
            for (auto it = cur->children().rbegin(); it != cur->children().rend(); ++it) stk.push_back(*it);
        }
    } else if (magic == 4 && el) { // closest
        auto cur = el;
        while (cur) {
            auto sel_list = b->page()->doc->root()->select_all(q);
            for (const auto& m : sel_list) {
                if (m.get() == cur.get()) return b->wrap_node(cur);
            }
            cur = cur->parent();
        }
        return JS_NULL;
    }

    JSValue arr = JS_NewArray(ctx);
    for (size_t i = 0; i < found.size(); ++i) {
        JS_SetPropertyUint32(ctx, arr, static_cast<uint32_t>(i), b->wrap_node(found[i]));
    }
    return arr;
}

JSValue js_node_get_rect(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    JSValue obj = JS_NewObject(ctx);
    double x = 0, y = 0, w = 0, h = 0;
    if (el) {
        if (b && b->page()) b->page()->ensure_layout();
        auto p = el->get_placement();
        int sx = (b && b->page()) ? b->page()->scroll_x : 0;
        int sy = (b && b->page()) ? b->page()->scroll_y : 0;
        x = p.x.value() - sx;
        y = p.y.value() - sy;
        w = p.width.value();
        h = p.height.value();
    }
    JS_SetPropertyStr(ctx, obj, "x", JS_NewFloat64(ctx, x));
    JS_SetPropertyStr(ctx, obj, "y", JS_NewFloat64(ctx, y));
    JS_SetPropertyStr(ctx, obj, "left", JS_NewFloat64(ctx, x));
    JS_SetPropertyStr(ctx, obj, "top", JS_NewFloat64(ctx, y));
    JS_SetPropertyStr(ctx, obj, "width", JS_NewFloat64(ctx, w));
    JS_SetPropertyStr(ctx, obj, "height", JS_NewFloat64(ctx, h));
    JS_SetPropertyStr(ctx, obj, "right", JS_NewFloat64(ctx, x + w));
    JS_SetPropertyStr(ctx, obj, "bottom", JS_NewFloat64(ctx, y + h));
    return obj;
}

JSValue js_node_action(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b || !b->page()) return JS_UNDEFINED;
    if (magic == 0) { // focus
        auto prev = b->page()->focused_el.lock();
        if (prev && prev.get() != el.get()) b->dispatch_ui_event(prev, "blur", false, false);
        b->page()->focused_el = el;
        b->dispatch_ui_event(el, "focus", false, false);
        b->page()->mark_dirty(false, true);
    } else if (magic == 1) { // blur
        auto prev = b->page()->focused_el.lock();
        if (prev && prev.get() == el.get()) {
            b->page()->focused_el.reset();
            b->dispatch_ui_event(el, "blur", false, false);
            b->page()->mark_dirty(false, true);
        }
    } else if (magic == 2) { // click
        b->page()->dispatch_click_or_submit(el);
    }
    return JS_UNDEFINED;
}

JSValue js_node_get_context(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b) return JS_NULL;
    std::string kind = (argc >= 1) ? DomBridge::js_to_std_string(ctx, argv[0]) : "2d";
    if (kind != "2d" || !dynamic_cast<el_space_canvas*>(el.get())) return JS_NULL;
    return b->wrap_canvas_ctx(el);
}

JSValue js_node_listener_op(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b || argc < 2 || !JS_IsFunction(ctx, argv[1])) return JS_UNDEFINED;
    std::string type = DomBridge::js_to_std_string(ctx, argv[0]);
    auto* rec = b->ensure_record(el);
    if (magic == 0) { // addEventListener
        ListenerEntry le;
        le.type = type;
        le.func = JS_DupValue(ctx, argv[1]);
        if (argc >= 3 && JS_IsObject(argv[2])) {
            JSValue ov = JS_GetPropertyStr(ctx, argv[2], "once");
            le.once = JS_ToBool(ctx, ov) != 0;
            JS_FreeValue(ctx, ov);
        }
        rec->listeners.push_back(le);
    } else { // removeEventListener
        for (size_t i = 0; i < rec->listeners.size();) {
            if (rec->listeners[i].type == type &&
                JS_VALUE_GET_PTR(rec->listeners[i].func) == JS_VALUE_GET_PTR(argv[1])) {
                JS_FreeValue(ctx, rec->listeners[i].func);
                rec->listeners.erase(rec->listeners.begin() + i);
            } else {
                ++i;
            }
        }
    }
    return JS_UNDEFINED;
}

JSValue js_node_on_get(JSContext* ctx, JSValueConst this_val, int /*argc*/, JSValueConst* /*argv*/, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b || magic < 0 || magic >= 12) return JS_NULL;
    auto* rec = b->find_record_by_el(el);
    if (!rec || JS_IsUndefined(rec->on_props[magic])) return JS_NULL;
    return JS_DupValue(ctx, rec->on_props[magic]);
}

JSValue js_node_on_set(JSContext* ctx, JSValueConst this_val, int argc, JSValueConst* argv, int magic) {
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, this_val, b);
    if (!el || !b || magic < 0 || magic >= 12 || argc < 1) return JS_UNDEFINED;
    auto* rec = b->ensure_record(el);
    JS_FreeValue(ctx, rec->on_props[magic]);
    rec->on_props[magic] = JS_IsFunction(ctx, argv[0]) ? JS_DupValue(ctx, argv[0]) : JS_UNDEFINED;
    return JS_UNDEFINED;
}

JSValue js_doc_query(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv, int magic) {
    auto* b = get_bridge(ctx);
    if (!b || !b->page() || !b->page()->doc) return JS_NULL;
    auto root = b->page()->doc->root();
    if (!root) return (magic == 0 || magic == 1) ? JS_NULL : JS_NewArray(ctx);
    if (argc < 1) return (magic == 0 || magic == 1) ? JS_NULL : JS_NewArray(ctx);
    std::string q = DomBridge::js_to_std_string(ctx, argv[0]);
    if (magic == 0) { // getElementById
        std::vector<litehtml::element::ptr> stk = {root};
        while (!stk.empty()) {
            auto cur = stk.back();
            stk.pop_back();
            if (!cur->is_text()) {
                const char* id = cur->get_attr("id");
                if (id && q == id) return b->wrap_node(cur);
            }
            for (auto it = cur->children().rbegin(); it != cur->children().rend(); ++it) stk.push_back(*it);
        }
        return JS_NULL;
    }
    JSValue root_js = b->wrap_node(root);
    JSValue res = js_node_query(ctx, root_js, argc, argv, magic - 1);
    JS_FreeValue(ctx, root_js);
    return res;
}

JSValue js_doc_create_el(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv, int is_text) {
    auto* b = get_bridge(ctx);
    if (!b || !b->page() || !b->page()->doc || argc < 1) return JS_NULL;
    std::string s = DomBridge::js_to_std_string(ctx, argv[0]);
    if (is_text) {
        auto tn = std::make_shared<litehtml::el_text>(s.c_str(), b->page()->doc);
        return b->wrap_node(tn);
    }
    std::string tag = to_lower_ascii(s);
    litehtml::string_map empty_attrs;
    auto el = b->page()->doc->create_element(tag.c_str(), empty_attrs);
    return b->wrap_node(el);
}

JSValue js_doc_prop_get(JSContext* ctx, JSValueConst /*this_val*/, int /*argc*/, JSValueConst* /*argv*/, int magic) {
    auto* b = get_bridge(ctx);
    if (!b || !b->page()) return JS_NULL;
    auto doc = b->page()->doc;
    if (magic == 0) return b->wrap_node(find_doc_body_or_root(doc, "body"));
    if (magic == 1) return b->wrap_node(find_doc_body_or_root(doc, "head"));
    if (magic == 2) return b->wrap_node(doc ? doc->root() : nullptr);
    if (magic == 3) return JS_NewString(ctx, b->page()->title.c_str());
    if (magic == 4) return JS_NewString(ctx, b->page()->url.c_str());
    if (magic == 5) return b->wrap_node(b->page()->focused_el.lock());
    return JS_UNDEFINED;
}

JSValue js_doc_title_set(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv, int /*magic*/) {
    auto* b = get_bridge(ctx);
    if (b && b->page() && argc >= 1) {
        b->page()->title = DomBridge::js_to_std_string(ctx, argv[0]);
        if (b->page()->cb.set_title) b->page()->cb.set_title(b->page()->cb.user_data, b->page()->title.c_str());
    }
    return JS_UNDEFINED;
}

JSValue js_get_computed_style(JSContext* ctx, JSValueConst /*this_val*/, int argc, JSValueConst* argv) {
    if (argc < 1) return JS_NewObject(ctx);
    DomBridge* b = nullptr;
    auto el = unwrap_node(ctx, argv[0], b);
    JSValue obj = JS_NewObject(ctx);
    if (!el) return obj;
    if (b && b->page()) b->page()->ensure_restyle();
    const auto& css = el->css();
    char buf[64];
    std::snprintf(buf, sizeof(buf), "%dpx", static_cast<int>(std::round(css.get_font_size().value())));
    JS_SetPropertyStr(ctx, obj, "fontSize", JS_NewString(ctx, buf));
    auto col = css.get_color();
    std::snprintf(buf, sizeof(buf), "rgb(%d, %d, %d)", col.red, col.green, col.blue);
    JS_SetPropertyStr(ctx, obj, "color", JS_NewString(ctx, buf));
    auto bg = css.get_bg().m_color;
    std::snprintf(buf, sizeof(buf), "rgba(%d, %d, %d, %.2f)", bg.red, bg.green, bg.blue, bg.alpha / 255.0);
    JS_SetPropertyStr(ctx, obj, "backgroundColor", JS_NewString(ctx, buf));
    auto p = el->get_placement();
    std::snprintf(buf, sizeof(buf), "%dpx", static_cast<int>(p.width.value()));
    JS_SetPropertyStr(ctx, obj, "width", JS_NewString(ctx, buf));
    std::snprintf(buf, sizeof(buf), "%dpx", static_cast<int>(p.height.value()));
    JS_SetPropertyStr(ctx, obj, "height", JS_NewString(ctx, buf));
    const char* disp = "block";
    switch (css.get_display()) {
    case litehtml::display_none: disp = "none"; break;
    case litehtml::display_inline: disp = "inline"; break;
    case litehtml::display_inline_block: disp = "inline-block"; break;
    case litehtml::display_flex: disp = "flex"; break;
    case litehtml::display_inline_flex: disp = "inline-flex"; break;
    case litehtml::display_grid: disp = "grid"; break;
    default: break;
    }
    JS_SetPropertyStr(ctx, obj, "display", JS_NewString(ctx, disp));
    return obj;
}

} // namespace

// =========================================================================
// DomBridge Class Methods
// =========================================================================

DomBridge::DomBridge(SpacePage* page) : page_(page) {
    reset_context();
}

DomBridge::~DomBridge() {
    free_all_handles();
    if (ctx_) JS_FreeContext(ctx_);
    if (rt_) JS_FreeRuntime(rt_);
}

std::string DomBridge::js_to_std_string(JSContext* ctx, JSValueConst val) {
    const char* s = JS_ToCString(ctx, val);
    if (!s) return "";
    std::string out(s);
    JS_FreeCString(ctx, s);
    return out;
}

std::string DomBridge::normalize_css_prop(const std::string& name) {
    if (name.find('-') != std::string::npos) return to_lower_ascii(name);
    std::string out;
    for (unsigned char c : name) {
        if (std::isupper(c)) {
            out.push_back('-');
            out.push_back(static_cast<char>(std::tolower(c)));
        } else {
            out.push_back(static_cast<char>(c));
        }
    }
    return out;
}

void DomBridge::collect_elements_by_selector(const litehtml::element::ptr& root,
                                             const std::string& sel,
                                             std::vector<litehtml::element::ptr>& out,
                                             bool first_only) {
    if (!root || sel.empty()) return;
    if (first_only) {
        auto one = root->select_one(sel);
        if (one) out.push_back(one);
        return;
    }
    auto list = root->select_all(sel);
    for (const auto& item : list) out.push_back(item);
}

void DomBridge::begin_watchdog(double budget_ms) {
    watchdog_active_ = true;
    watchdog_deadline_ms_ = static_cast<double>(monotonic_ms()) + budget_ms;
}

bool DomBridge::check_watchdog_expired() const {
    return watchdog_active_ && static_cast<double>(monotonic_ms()) > watchdog_deadline_ms_;
}

void DomBridge::end_watchdog() {
    watchdog_active_ = false;
}

void DomBridge::notify_console(int level, const std::string& msg) {
    if (page_ && page_->cb.on_console) {
        page_->cb.on_console(page_->cb.user_data, level, msg.c_str());
    }
}

void DomBridge::report_exception() {
    if (!ctx_) return;
    JSValue exc = JS_GetException(ctx_);
    std::string msg = js_to_std_string(ctx_, exc);
    JSValue stk = JS_GetPropertyStr(ctx_, exc, "stack");
    if (!JS_IsUndefined(stk) && !JS_IsNull(stk)) {
        std::string st = js_to_std_string(ctx_, stk);
        if (!st.empty()) msg += "\n" + st;
    }
    JS_FreeValue(ctx_, stk);
    JS_FreeValue(ctx_, exc);
    notify_console(3, msg);
}

void DomBridge::free_all_handles() {
    if (!ctx_) return;
    for (auto& kv : nodes_) {
        for (auto& le : kv.second.listeners) JS_FreeValue(ctx_, le.func);
        for (auto& ov : kv.second.on_props) JS_FreeValue(ctx_, ov);
    }
    nodes_.clear();
    ptr_to_id_.clear();
    for (auto& le : window_listeners_) JS_FreeValue(ctx_, le.func);
    window_listeners_.clear();
    for (auto& le : document_listeners_) JS_FreeValue(ctx_, le.func);
    document_listeners_.clear();
    for (auto& t : timers_) {
        JS_FreeValue(ctx_, t.func);
        for (auto& a : t.args) JS_FreeValue(ctx_, a);
    }
    timers_.clear();
    for (auto& f : fetches_) {
        JS_FreeValue(ctx_, f.resolve_fn);
        JS_FreeValue(ctx_, f.reject_fn);
        JS_FreeValue(ctx_, f.xhr_obj);
    }
    fetches_.clear();
}

void DomBridge::reset_context() {
    free_all_handles();
    if (ctx_) { JS_FreeContext(ctx_); ctx_ = nullptr; }
    if (rt_) { JS_FreeRuntime(rt_); rt_ = nullptr; }

    rt_ = JS_NewRuntime();
    JS_SetMemoryLimit(rt_, 64 * 1024 * 1024);
    JS_SetMaxStackSize(rt_, 512 * 1024);
    JS_SetRuntimeOpaque(rt_, this);
    JS_SetInterruptHandler(rt_, js_interrupt_handler, this);

    ctx_ = JS_NewContext(rt_);

    JS_NewClassID(rt_, &cls_node_);
    JS_NewClassID(rt_, &cls_style_);
    JS_NewClassID(rt_, &cls_classlist_);
    JS_NewClassID(rt_, &cls_canvas_ctx_);
    JS_NewClassID(rt_, &cls_xhr_);

    JSClassDef node_def{};
    node_def.class_name = "SpaceDomNode";
    node_def.finalizer = [](JSRuntime* /*rt*/, JSValue val) {
        if (auto* p = static_cast<NodeOpaque*>(JS_GetOpaque(val, JS_GetClassID(val)))) {
            if (p->bridge) {
                if (auto* rec = p->bridge->find_record(p->id)) {
                    if (rec->js_wrappers > 0) --rec->js_wrappers;
                    if (rec->js_wrappers == 0) rec->detached_pin.reset();
                }
            }
            delete p;
        }
    };
    JSClassDef ctx2d_def{};
    ctx2d_def.class_name = "CanvasRenderingContext2D";
    ctx2d_def.finalizer = [](JSRuntime* /*rt*/, JSValue val) {
        auto* p = static_cast<CanvasOpaque*>(JS_GetOpaque(val, JS_GetClassID(val)));
        delete p;
    };
    JSClassDef xhr_def{};
    xhr_def.class_name = "XMLHttpRequest";
    xhr_def.finalizer = [](JSRuntime* /*rt*/, JSValue val) {
        auto* p = static_cast<XhrOpaque*>(JS_GetOpaque(val, JS_GetClassID(val)));
        delete p;
    };
    JS_NewClass(rt_, cls_node_, &node_def);
    JS_NewClass(rt_, cls_canvas_ctx_, &ctx2d_def);
    JS_NewClass(rt_, cls_xhr_, &xhr_def);

    install_globals();
    install_dom_apis();
    install_bootstrap_polyfills();
}

uint64_t DomBridge::intern_node(const litehtml::element::ptr& el) {
    if (!el) return 0;
    auto it = ptr_to_id_.find(el.get());
    if (it != ptr_to_id_.end()) {
        if (!nodes_[it->second].wp.expired()) return it->second;
    }
    uint64_t id = next_node_id_++;
    NodeRecord rec;
    rec.id = id;
    rec.wp = el;
    if (const char* st = el->get_attr("style")) {
        rec.inline_styles = parse_inline_style_attr(st);
    }
    nodes_[id] = std::move(rec);
    ptr_to_id_[el.get()] = id;
    return id;
}

litehtml::element::ptr DomBridge::find_node(uint64_t id) {
    auto it = nodes_.find(id);
    if (it == nodes_.end()) return nullptr;
    return it->second.wp.lock();
}

NodeRecord* DomBridge::ensure_record(const litehtml::element::ptr& el) {
    uint64_t id = intern_node(el);
    if (id == 0) return nullptr;
    return &nodes_[id];
}

NodeRecord* DomBridge::find_record(uint64_t id) {
    auto it = nodes_.find(id);
    return it == nodes_.end() ? nullptr : &it->second;
}

NodeRecord* DomBridge::find_record_by_el(const litehtml::element::ptr& el) {
    if (!el) return nullptr;
    auto it = ptr_to_id_.find(el.get());
    if (it == ptr_to_id_.end()) return nullptr;
    return find_record(it->second);
}

void DomBridge::sync_node_inline_style(const litehtml::element::ptr& el, NodeRecord& rec) {
    std::string css = serialize_style_map(rec.inline_styles);
    if (css.empty()) el->remove_attr("style");
    else el->set_attr("style", css.c_str());
    el->parse_attributes();
    trigger_restyle_relayout(this, true);
}

JSValue DomBridge::wrap_node(const litehtml::element::ptr& el) {
    if (!el) return JS_NULL;
    uint64_t id = intern_node(el);
    if (auto* rec = find_record(id)) {
        ++rec->js_wrappers;
        bool is_root = page_ && page_->doc && el == page_->doc->root();
        if (!is_root && !el->parent()) {
            rec->detached_pin = el;
        }
    }
    JSValue obj = JS_NewObjectClass(ctx_, cls_node_);
    JS_SetOpaque(obj, new NodeOpaque{this, id});
    return obj;
}

JSValue DomBridge::wrap_canvas_ctx(const litehtml::element::ptr& el) {
    if (!el) return JS_NULL;
    uint64_t id = intern_node(el);
    JSValue obj = JS_NewObjectClass(ctx_, cls_canvas_ctx_);
    JS_SetOpaque(obj, new CanvasOpaque{this, id});
    return obj;
}

uint64_t DomBridge::add_timer(TimerEntry&& entry) {
    entry.id = next_timer_id_++;
    timers_.push_back(std::move(entry));
    return timers_.back().id;
}

void DomBridge::cancel_timer(uint64_t id) {
    for (auto& t : timers_) {
        if (t.id == id) { t.cancelled = true; }
    }
}

void DomBridge::add_pending_fetch(PendingFetch&& pf) {
    fetches_.push_back(std::move(pf));
}

void DomBridge::install_globals() {
    JSValue global = JS_GetGlobalObject(ctx_);
    JS_SetPropertyStr(ctx_, global, "window", JS_DupValue(ctx_, global));
    JS_SetPropertyStr(ctx_, global, "self", JS_DupValue(ctx_, global));

    JSValue console = JS_NewObject(ctx_);
    def_fn_magic(ctx_, console, "log", js_console_msg, 1, 0);
    def_fn_magic(ctx_, console, "info", js_console_msg, 1, 1);
    def_fn_magic(ctx_, console, "warn", js_console_msg, 1, 2);
    def_fn_magic(ctx_, console, "error", js_console_msg, 1, 3);
    def_fn_magic(ctx_, console, "debug", js_console_msg, 1, 0);
    JS_SetPropertyStr(ctx_, global, "console", console);

    def_fn_magic(ctx_, global, "alert", js_alert_dialog, 1, 0);
    def_fn_magic(ctx_, global, "confirm", js_alert_dialog, 1, 1);
    def_fn_magic(ctx_, global, "prompt", js_alert_dialog, 2, 2);
    def_fn(ctx_, global, "btoa", js_btoa, 1);
    def_fn(ctx_, global, "atob", js_atob, 1);
    def_fn(ctx_, global, "fetch", js_fetch, 2);
    def_fn(ctx_, global, "getComputedStyle", js_get_computed_style, 1);

    def_fn_magic(ctx_, global, "setTimeout", js_set_timer, 2, 0);
    def_fn_magic(ctx_, global, "setInterval", js_set_timer, 2, 1);
    def_fn_magic(ctx_, global, "requestAnimationFrame", js_set_timer, 1, 2);
    def_fn(ctx_, global, "clearTimeout", js_clear_timer, 1);
    def_fn(ctx_, global, "clearInterval", js_clear_timer, 1);
    def_fn(ctx_, global, "cancelAnimationFrame", js_clear_timer, 1);
    def_fn(ctx_, global, "queueMicrotask", js_queue_microtask, 1);

    JSValue perf = JS_NewObject(ctx_);
    def_fn(ctx_, perf, "now", js_perf_now, 0);
    JS_SetPropertyStr(ctx_, global, "performance", perf);

    JSValue nav = JS_NewObject(ctx_);
    JS_SetPropertyStr(ctx_, nav, "userAgent",
                      JS_NewString(ctx_, "SpaceBrowser/1.0 SpaceOS-9 (X11; Linux x86_64) LiteHTML QuickJS-NG"));
    JS_SetPropertyStr(ctx_, nav, "platform", JS_NewString(ctx_, "SpaceOS-9"));
    JS_SetPropertyStr(ctx_, nav, "language", JS_NewString(ctx_, "zh-CN"));
    JS_SetPropertyStr(ctx_, nav, "onLine", JS_TRUE);
    JS_SetPropertyStr(ctx_, global, "navigator", nav);

    JSValue loc = JS_NewObject(ctx_);
    def_getset(ctx_, loc, "href", js_location_get_href, js_location_set_href);
    def_fn(ctx_, loc, "assign", js_location_set_href, 1);
    def_fn(ctx_, loc, "replace", js_location_replace, 1);
    def_fn(ctx_, loc, "reload", js_location_reload, 0);
    JS_SetPropertyStr(ctx_, global, "location", loc);

    JSValue hist = JS_NewObject(ctx_);
    def_fn_magic(ctx_, hist, "back", js_history_op, 0, 0);
    def_fn_magic(ctx_, hist, "forward", js_history_op, 0, 1);
    def_fn_magic(ctx_, hist, "go", js_history_op, 1, 2);
    def_fn_magic(ctx_, hist, "pushState", js_history_op, 3, 3);
    def_fn_magic(ctx_, hist, "replaceState", js_history_op, 3, 4);
    JS_SetPropertyStr(ctx_, global, "history", hist);

    auto make_storage = [&](bool is_session) {
        JSValue st = JS_NewObject(ctx_);
        JS_SetPropertyStr(ctx_, st, "__isSession", JS_NewBool(ctx_, is_session));
        def_fn_magic(ctx_, st, "getItem", js_storage_fn, 1, 0);
        def_fn_magic(ctx_, st, "setItem", js_storage_fn, 2, 1);
        def_fn_magic(ctx_, st, "removeItem", js_storage_fn, 1, 2);
        def_fn_magic(ctx_, st, "clear", js_storage_fn, 0, 3);
        def_fn_magic(ctx_, st, "key", js_storage_fn, 1, 4);
        def_getset_magic(ctx_, st, "length", 5, js_storage_fn);
        return st;
    };
    JS_SetPropertyStr(ctx_, global, "_rawLocalStorage", make_storage(false));
    JS_SetPropertyStr(ctx_, global, "_rawSessionStorage", make_storage(true));

    JSValue xhr_ctor = JS_NewCFunction2(ctx_, js_xhr_ctor, "XMLHttpRequest", 0, JS_CFUNC_constructor, 0);
    JSValue xhr_proto = JS_NewObject(ctx_);
    def_fn(ctx_, xhr_proto, "open", js_xhr_open, 3);
    def_fn(ctx_, xhr_proto, "setRequestHeader", js_xhr_set_header, 2);
    def_fn(ctx_, xhr_proto, "send", js_xhr_send, 1);
    def_fn(ctx_, xhr_proto, "getResponseHeader", js_xhr_get_header, 1);
    JS_SetClassProto(ctx_, cls_xhr_, xhr_proto);
    JS_SetPropertyStr(ctx_, global, "XMLHttpRequest", xhr_ctor);

    def_getset(ctx_, global, "innerWidth", [](JSContext* c, JSValueConst, int, JSValueConst*) {
        auto* br = get_bridge(c);
        return JS_NewInt32(c, (br && br->page()) ? br->page()->viewport_w : 800);
    });
    def_getset(ctx_, global, "innerHeight", [](JSContext* c, JSValueConst, int, JSValueConst*) {
        auto* br = get_bridge(c);
        return JS_NewInt32(c, (br && br->page()) ? br->page()->viewport_h : 600);
    });
    JS_SetPropertyStr(ctx_, global, "devicePixelRatio", JS_NewInt32(ctx_, 1));
    def_getset(ctx_, global, "scrollX", [](JSContext* c, JSValueConst, int, JSValueConst*) {
        auto* br = get_bridge(c);
        return JS_NewInt32(c, (br && br->page()) ? br->page()->scroll_x : 0);
    });
    def_getset(ctx_, global, "scrollY", [](JSContext* c, JSValueConst, int, JSValueConst*) {
        auto* br = get_bridge(c);
        return JS_NewInt32(c, (br && br->page()) ? br->page()->scroll_y : 0);
    });

    JS_FreeValue(ctx_, global);
}

void DomBridge::install_dom_apis() {
    // Node Prototype
    JSValue node_proto = JS_NewObject(ctx_);
    def_fn_magic(ctx_, node_proto, "getAttribute", js_node_attr_op, 1, 0);
    def_fn_magic(ctx_, node_proto, "setAttribute", js_node_attr_op, 2, 1);
    def_fn_magic(ctx_, node_proto, "removeAttribute", js_node_attr_op, 1, 2);
    def_fn_magic(ctx_, node_proto, "hasAttribute", js_node_attr_op, 1, 3);
    def_fn_magic(ctx_, node_proto, "appendChild", js_node_tree_op, 1, 0);
    def_fn_magic(ctx_, node_proto, "removeChild", js_node_tree_op, 1, 1);
    def_fn_magic(ctx_, node_proto, "insertBefore", js_node_tree_op, 2, 2);
    def_fn_magic(ctx_, node_proto, "remove", js_node_tree_op, 0, 3);
    def_fn_magic(ctx_, node_proto, "replaceChild", js_node_tree_op, 2, 4);
    def_fn_magic(ctx_, node_proto, "contains", js_node_tree_op, 1, 5);

    def_fn_magic(ctx_, node_proto, "querySelector", js_node_query, 1, 0);
    def_fn_magic(ctx_, node_proto, "querySelectorAll", js_node_query, 1, 1);
    def_fn_magic(ctx_, node_proto, "getElementsByTagName", js_node_query, 1, 2);
    def_fn_magic(ctx_, node_proto, "getElementsByClassName", js_node_query, 1, 3);
    def_fn_magic(ctx_, node_proto, "closest", js_node_query, 1, 4);

    def_fn(ctx_, node_proto, "getBoundingClientRect", js_node_get_rect, 0);
    def_fn_magic(ctx_, node_proto, "focus", js_node_action, 0, 0);
    def_fn_magic(ctx_, node_proto, "blur", js_node_action, 0, 1);
    def_fn_magic(ctx_, node_proto, "click", js_node_action, 0, 2);
    def_fn(ctx_, node_proto, "getContext", js_node_get_context, 1);
    def_fn_magic(ctx_, node_proto, "addEventListener", js_node_listener_op, 3, 0);
    def_fn_magic(ctx_, node_proto, "removeEventListener", js_node_listener_op, 2, 1);

    def_getset_magic(ctx_, node_proto, "id", 0, js_node_prop_str_get, js_node_prop_str_set);
    def_getset_magic(ctx_, node_proto, "className", 1, js_node_prop_str_get, js_node_prop_str_set);
    def_getset_magic(ctx_, node_proto, "innerHTML", 2, js_node_prop_str_get, js_node_prop_str_set);
    def_getset_magic(ctx_, node_proto, "outerHTML", 3, js_node_prop_str_get);
    def_getset_magic(ctx_, node_proto, "textContent", 4, js_node_prop_str_get, js_node_prop_str_set);
    def_getset_magic(ctx_, node_proto, "innerText", 5, js_node_prop_str_get, js_node_prop_str_set);
    def_getset_magic(ctx_, node_proto, "value", 6, js_node_prop_str_get, js_node_prop_str_set);
    def_getset_magic(ctx_, node_proto, "src", 7, js_node_prop_str_get, js_node_prop_str_set);
    def_getset_magic(ctx_, node_proto, "href", 8, js_node_prop_str_get, js_node_prop_str_set);
    def_getset_magic(ctx_, node_proto, "tagName", 9, js_node_prop_str_get);
    def_getset_magic(ctx_, node_proto, "nodeName", 9, js_node_prop_str_get);

    def_getset(ctx_, node_proto, "checked", js_node_checked_get, js_node_checked_set);
    def_getset(ctx_, node_proto, "disabled", js_node_disabled_get, js_node_disabled_set);
    def_getset(ctx_, node_proto, "selectedIndex", js_node_selidx_get, js_node_selidx_set);
    def_getset_magic(ctx_, node_proto, "width", 0, js_node_dim_get, js_node_dim_set);
    def_getset_magic(ctx_, node_proto, "height", 1, js_node_dim_get, js_node_dim_set);
    def_getset_magic(ctx_, node_proto, "offsetWidth", 2, js_node_dim_get);
    def_getset_magic(ctx_, node_proto, "offsetHeight", 3, js_node_dim_get);
    def_getset_magic(ctx_, node_proto, "clientWidth", 2, js_node_dim_get);
    def_getset_magic(ctx_, node_proto, "clientHeight", 3, js_node_dim_get);
    def_getset_magic(ctx_, node_proto, "offsetLeft", 4, js_node_dim_get);
    def_getset_magic(ctx_, node_proto, "offsetTop", 5, js_node_dim_get);

    def_getset(ctx_, node_proto, "style", js_node_get_style);
    def_getset(ctx_, node_proto, "classList", js_node_get_classlist);
    def_getset_magic(ctx_, node_proto, "parentNode", 0, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "parentElement", 0, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "firstChild", 1, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "lastChild", 2, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "firstElementChild", 3, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "nextSibling", 4, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "nextElementSibling", 5, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "previousSibling", 6, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "previousElementSibling", 7, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "children", 8, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "childNodes", 9, js_node_rel_get);
    def_getset_magic(ctx_, node_proto, "nodeType", 10, js_node_rel_get);

    for (int i = 0; i < 12; ++i) {
        std::string prop = std::string("on") + kOnEventNames[i];
        def_getset_magic(ctx_, node_proto, prop.c_str(), i, js_node_on_get, js_node_on_set);
    }
    JS_SetClassProto(ctx_, cls_node_, node_proto);

    // CanvasRenderingContext2D prototype
    JSValue c2d = JS_NewObject(ctx_);
    def_getset_magic(ctx_, c2d, "fillStyle", 0, js_canvas_get_set_fill, js_canvas_get_set_fill);
    def_getset_magic(ctx_, c2d, "strokeStyle", 1, js_canvas_get_set_fill, js_canvas_get_set_fill);
    def_getset_magic(ctx_, c2d, "font", 2, js_canvas_get_set_fill, js_canvas_get_set_fill);
    def_getset_magic(ctx_, c2d, "lineWidth", 3, js_canvas_get_set_fill, js_canvas_get_set_fill);
    def_getset_magic(ctx_, c2d, "globalAlpha", 4, js_canvas_get_set_fill, js_canvas_get_set_fill);
    def_getset_magic(ctx_, c2d, "textAlign", 5, js_canvas_get_set_fill, js_canvas_get_set_fill);
    def_getset_magic(ctx_, c2d, "textBaseline", 6, js_canvas_get_set_fill, js_canvas_get_set_fill);

    def_fn_magic(ctx_, c2d, "clearRect", js_canvas_rect_op, 4, 0);
    def_fn_magic(ctx_, c2d, "fillRect", js_canvas_rect_op, 4, 1);
    def_fn_magic(ctx_, c2d, "strokeRect", js_canvas_rect_op, 4, 2);
    def_fn_magic(ctx_, c2d, "rect", js_canvas_rect_op, 4, 3);

    def_fn_magic(ctx_, c2d, "beginPath", js_canvas_path_op, 0, 0);
    def_fn_magic(ctx_, c2d, "closePath", js_canvas_path_op, 0, 1);
    def_fn_magic(ctx_, c2d, "save", js_canvas_path_op, 0, 2);
    def_fn_magic(ctx_, c2d, "restore", js_canvas_path_op, 0, 3);
    def_fn_magic(ctx_, c2d, "resetTransform", js_canvas_path_op, 0, 4);
    def_fn_magic(ctx_, c2d, "fill", js_canvas_path_op, 0, 5);
    def_fn_magic(ctx_, c2d, "stroke", js_canvas_path_op, 0, 6);
    def_fn_magic(ctx_, c2d, "moveTo", js_canvas_path_op, 2, 7);
    def_fn_magic(ctx_, c2d, "lineTo", js_canvas_path_op, 2, 8);
    def_fn_magic(ctx_, c2d, "arc", js_canvas_path_op, 6, 9);
    def_fn_magic(ctx_, c2d, "quadraticCurveTo", js_canvas_path_op, 4, 10);
    def_fn_magic(ctx_, c2d, "bezierCurveTo", js_canvas_path_op, 6, 11);
    def_fn_magic(ctx_, c2d, "translate", js_canvas_path_op, 2, 12);
    def_fn_magic(ctx_, c2d, "scale", js_canvas_path_op, 2, 13);
    def_fn_magic(ctx_, c2d, "rotate", js_canvas_path_op, 1, 14);
    def_fn_magic(ctx_, c2d, "setTransform", js_canvas_path_op, 6, 15);
    def_fn_magic(ctx_, c2d, "transform", js_canvas_path_op, 6, 16);
    def_fn_magic(ctx_, c2d, "fillText", js_canvas_text_op, 3, 0);
    def_fn_magic(ctx_, c2d, "measureText", js_canvas_text_op, 1, 1);
    JS_SetClassProto(ctx_, cls_canvas_ctx_, c2d);

    // document
    JSValue global = JS_GetGlobalObject(ctx_);
    JSValue doc = JS_NewObject(ctx_);
    def_fn_magic(ctx_, doc, "getElementById", js_doc_query, 1, 0);
    def_fn_magic(ctx_, doc, "querySelector", js_doc_query, 1, 1);
    def_fn_magic(ctx_, doc, "querySelectorAll", js_doc_query, 1, 2);
    def_fn_magic(ctx_, doc, "getElementsByTagName", js_doc_query, 1, 3);
    def_fn_magic(ctx_, doc, "getElementsByClassName", js_doc_query, 1, 4);
    def_fn_magic(ctx_, doc, "createElement", js_doc_create_el, 1, 0);
    def_fn_magic(ctx_, doc, "createTextNode", js_doc_create_el, 1, 1);
    def_getset_magic(ctx_, doc, "body", 0, js_doc_prop_get);
    def_getset_magic(ctx_, doc, "head", 1, js_doc_prop_get);
    def_getset_magic(ctx_, doc, "documentElement", 2, js_doc_prop_get);
    def_getset_magic(ctx_, doc, "title", 3, js_doc_prop_get, js_doc_title_set);
    def_getset_magic(ctx_, doc, "URL", 4, js_doc_prop_get);
    def_getset_magic(ctx_, doc, "activeElement", 5, js_doc_prop_get);
    def_getset(ctx_, doc, "cookie", js_cookie_get, js_cookie_set);
    JS_SetPropertyStr(ctx_, doc, "readyState", JS_NewString(ctx_, "complete"));
    JS_SetPropertyStr(ctx_, global, "document", doc);
    JS_FreeValue(ctx_, global);
}

void DomBridge::install_bootstrap_polyfills() {
    static const char* kBootstrap = R"JS(
(function(g){
  g.__makeStyleProxy = function(raw){
    return new Proxy(raw, {
      get(t, k){
        if (typeof k !== 'string') return undefined;
        if (k === 'setProperty') return function(n,v){ t.__set(String(n), v==null?'':String(v)); };
        if (k === 'getPropertyValue') return function(n){ return t.__get(String(n)); };
        if (k === 'removeProperty') return function(n){ t.__set(String(n), ''); };
        return t.__get(k);
      },
      set(t, k, v){
        if (typeof k === 'string') t.__set(k, v==null?'':String(v));
        return true;
      }
    });
  };
  function makeStorageProxy(raw){
    return new Proxy(raw, {
      get(t, k){
        if (k in t) {
          var v = t[k];
          return (typeof v === 'function') ? v.bind(t) : v;
        }
        if (typeof k === 'string') return t.getItem(k);
        return undefined;
      },
      set(t, k, v){
        if (typeof k === 'string') t.setItem(k, String(v));
        return true;
      },
      deleteProperty(t, k){
        if (typeof k === 'string') t.removeItem(k);
        return true;
      }
    });
  }
  g.localStorage = makeStorageProxy(g._rawLocalStorage);
  g.sessionStorage = makeStorageProxy(g._rawSessionStorage);

  class Event {
    constructor(type, init){
      this.type = String(type);
      this.bubbles = !!(init && init.bubbles);
      this.cancelable = !!(init && init.cancelable);
      this.defaultPrevented = false;
      this.cancelBubble = false;
      this._stopImm = false;
      this.target = null;
      this.currentTarget = null;
      this.detail = init && init.detail !== undefined ? init.detail : null;
    }
    preventDefault(){ if (this.cancelable) this.defaultPrevented = true; }
    stopPropagation(){ this.cancelBubble = true; }
    stopImmediatePropagation(){ this.cancelBubble = true; this._stopImm = true; }
  }
  class CustomEvent extends Event {
    constructor(type, init){ super(type, init); }
  }
  g.Event = Event;
  g.CustomEvent = CustomEvent;

  class URLSearchParams {
    constructor(init){
      this._entries = [];
      if (typeof init === 'string') {
        var s = init.startsWith('?') ? init.slice(1) : init;
        if (s) for (var part of s.split('&')) {
          var eq = part.indexOf('=');
          var k = decodeURIComponent((eq<0?part:part.slice(0,eq)).replace(/\+/g,' '));
          var v = eq<0 ? '' : decodeURIComponent(part.slice(eq+1).replace(/\+/g,' '));
          this._entries.push([k, v]);
        }
      } else if (init && typeof init === 'object') {
        for (var k of Object.keys(init)) this._entries.push([k, String(init[k])]);
      }
    }
    append(k,v){ this._entries.push([String(k), String(v)]); }
    set(k,v){ k=String(k); this.delete(k); this._entries.push([k, String(v)]); }
    get(k){ k=String(k); for (var e of this._entries) if (e[0]===k) return e[1]; return null; }
    has(k){ return this.get(k)!==null; }
    delete(k){ k=String(k); this._entries=this._entries.filter(e=>e[0]!==k); }
    toString(){
      return this._entries.map(e=>encodeURIComponent(e[0])+'='+encodeURIComponent(e[1])).join('&');
    }
  }
  class URL {
    constructor(input, base){
      var s = String(input);
      if (base && !/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(s)) {
        var b = String(base);
        var m0 = b.match(/^([a-zA-Z][a-zA-Z0-9+.-]*:\/\/[^/?#]*)/);
        if (s.startsWith('/')) s = (m0 ? m0[1] : '') + s;
        else {
          var clean = b.split('#')[0].split('?')[0];
          var idx = clean.lastIndexOf('/');
          s = (idx >= 0 ? clean.slice(0, idx+1) : clean + '/') + s;
        }
      }
      var m = s.match(/^([a-zA-Z][a-zA-Z0-9+.-]*):(?:\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/);
      if (!m) throw new TypeError('Invalid URL: ' + s);
      this.protocol = m[1].toLowerCase() + ':';
      this.host = m[2] || '';
      var hp = this.host.split(':');
      this.hostname = hp[0] || '';
      this.port = hp[1] || '';
      this.pathname = m[3] || '/';
      this.search = m[4] || '';
      this.hash = m[5] || '';
      this.origin = this.host ? (this.protocol + '//' + this.host) : 'null';
      this.searchParams = new URLSearchParams(this.search);
    }
    get href(){
      var q = this.searchParams.toString();
      var qs = q ? ('?' + q) : '';
      return this.protocol + (this.host ? '//' + this.host : '//') + this.pathname + qs + this.hash;
    }
    toString(){ return this.href; }
  }
  g.URL = URL;
  g.URLSearchParams = URLSearchParams;

  var winListeners = {};
  var docListeners = {};
  function attachET(target, map){
    target.addEventListener = function(t, fn){ (map[t]=map[t]||[]).push(fn); };
    target.removeEventListener = function(t, fn){ if(map[t]) map[t]=map[t].filter(f=>f!==fn); };
    target.dispatchEvent = function(ev){
      if(!ev.target) ev.target = target;
      ev.currentTarget = target;
      for (var fn of (map[ev.type]||[]).slice()) { fn.call(target, ev); if (ev._stopImm) break; }
      return !ev.defaultPrevented;
    };
  }
  attachET(g, winListeners);
  attachET(g.document, docListeners);

  Object.defineProperty(g.Node ? g.Node : g, 'UNUSED', {value:0});
})(globalThis);
)JS";
    JSValue res = JS_Eval(ctx_, kBootstrap, std::strlen(kBootstrap), "<bootstrap>", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(res)) report_exception();
    JS_FreeValue(ctx_, res);
}

void DomBridge::flush_jobs() {
    if (!rt_) return;
    JSContext* jctx = nullptr;
    int budget = 512;
    while (budget-- > 0 && JS_IsJobPending(rt_)) {
        int r = JS_ExecutePendingJob(rt_, &jctx);
        if (r < 0) {
            report_exception();
            break;
        }
        if (check_watchdog_expired()) break;
    }
}

void DomBridge::run_script(const std::string& code, const std::string& filename) {
    if (!ctx_ || code.empty()) return;
    WatchdogScope wd(this, 1500.0);
    JSValue v = JS_Eval(ctx_, code.c_str(), code.size(), filename.c_str(), JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(v)) report_exception();
    JS_FreeValue(ctx_, v);
    flush_jobs();
}

std::string DomBridge::eval_repl(const std::string& code) {
    if (!ctx_ || code.empty()) return "";
    WatchdogScope wd(this, 1500.0);
    JSValue v = JS_Eval(ctx_, code.c_str(), code.size(), "<repl>", JS_EVAL_TYPE_GLOBAL);
    std::string out;
    if (JS_IsException(v)) {
        JSValue exc = JS_GetException(ctx_);
        out = "Error: " + js_to_std_string(ctx_, exc);
        JS_FreeValue(ctx_, exc);
    } else if (!JS_IsUndefined(v)) {
        if (JS_IsObject(v) && !JS_IsNull(v) && !JS_IsFunction(ctx_, v)) {
            JSValue j = JS_JSONStringify(ctx_, v, JS_UNDEFINED, JS_UNDEFINED);
            if (!JS_IsException(j) && JS_IsString(j)) {
                out = js_to_std_string(ctx_, j);
                JS_FreeValue(ctx_, j);
            } else {
                JS_FreeValue(ctx_, j);
                out = js_to_std_string(ctx_, v);
            }
        } else {
            out = js_to_std_string(ctx_, v);
        }
    } else {
        out = "undefined";
    }
    JS_FreeValue(ctx_, v);
    flush_jobs();
    return out;
}

void DomBridge::run_scripts_in_subtree(const litehtml::element::ptr& root, bool only_once_attr) {
    if (!root || !page_) return;
    std::vector<litehtml::element::ptr> scripts;
    std::vector<litehtml::element::ptr> stk = {root};
    while (!stk.empty()) {
        auto cur = stk.back();
        stk.pop_back();
        if (!cur->is_text() && cur->get_tagName() && to_lower_ascii(cur->get_tagName()) == "script") {
            if (!only_once_attr || !cur->get_attr("data-space-executed")) {
                scripts.push_back(cur);
            }
        }
        for (auto it = cur->children().rbegin(); it != cur->children().rend(); ++it) stk.push_back(*it);
    }

    if (page_->cb.prefetch_urls && scripts.size() > 1) {
        std::string urls_nl;
        for (const auto& s : scripts) {
            const char* type_attr = s->get_attr("type");
            if (type_attr && *type_attr) {
                std::string tl = to_lower_ascii(type_attr);
                if (tl != "text/javascript" && tl != "application/javascript" && tl != "module") {
                    continue;
                }
            }
            const char* src = s->get_attr("src");
            if (src && *src) {
                if (!urls_nl.empty()) urls_nl.push_back('\n');
                urls_nl += resolve_url(page_->base_url, src);
            }
        }
        if (!urls_nl.empty()) {
            page_->cb.prefetch_urls(page_->cb.user_data, urls_nl.c_str());
        }
    }

    for (const auto& s : scripts) {
        if (only_once_attr) s->set_attr("data-space-executed", "1");
        const char* type_attr = s->get_attr("type");
        if (type_attr && *type_attr) {
            std::string tl = to_lower_ascii(type_attr);
            if (tl != "text/javascript" && tl != "application/javascript" && tl != "module") {
                continue;
            }
        }
        const char* src = s->get_attr("src");
        if (src && *src && page_->cb.fetch_sync) {
            std::string resolved = resolve_url(page_->base_url, src);
            int status = 0, out_len = 0;
            char *out_body = nullptr, *out_url = nullptr, *out_hdrs = nullptr;
            page_->cb.fetch_sync(page_->cb.user_data, "GET", resolved.c_str(), nullptr, nullptr, 0,
                                 &status, &out_body, &out_len, &out_url, &out_hdrs);
            if (status >= 200 && status < 400 && out_body && out_len > 0) {
                std::string code(out_body, out_len);
                run_script(code, resolved);
            }
            if (page_->cb.free_host_buf) {
                if (out_body) page_->cb.free_host_buf(page_->cb.user_data, out_body);
                if (out_url) page_->cb.free_host_buf(page_->cb.user_data, out_url);
                if (out_hdrs) page_->cb.free_host_buf(page_->cb.user_data, out_hdrs);
            }
        } else {
            std::string code;
            s->get_text(code);
            if (!trim_ascii(code).empty()) {
                run_script(code, page_->url);
            }
        }
    }
}

void DomBridge::execute_document_scripts() {
    if (!page_ || !page_->doc || !page_->doc->root()) return;
    run_scripts_in_subtree(page_->doc->root(), true);
    dispatch_window_event("DOMContentLoaded");
    dispatch_window_event("load");
}

void DomBridge::dispatch_window_event(const std::string& type) {
    if (!ctx_) return;
    WatchdogScope wd(this, 1500.0);
    std::string js = "(function(){var e=new Event('" + type + "');document.dispatchEvent(e);window.dispatchEvent(e);})()";
    JSValue v = JS_Eval(ctx_, js.c_str(), js.size(), "<lifecycle>", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(v)) report_exception();
    JS_FreeValue(ctx_, v);
    flush_jobs();
}

bool DomBridge::dispatch_ui_event(const litehtml::element::ptr& target, const std::string& type,
                                    bool bubbles, bool cancelable, int x, int y, int button,
                                    int key_code, const std::string& key, bool ctrl, bool shift, bool alt) {
    if (!ctx_ || !target) return false;
    WatchdogScope wd(this, 1500.0);

    JSValue global = JS_GetGlobalObject(ctx_);
    JSValue ev_ctor = JS_GetPropertyStr(ctx_, global, "Event");
    JSValue init = JS_NewObject(ctx_);
    JS_SetPropertyStr(ctx_, init, "bubbles", JS_NewBool(ctx_, bubbles));
    JS_SetPropertyStr(ctx_, init, "cancelable", JS_NewBool(ctx_, cancelable));
    JSValue type_s = JS_NewString(ctx_, type.c_str());
    JSValue ctor_args[2] = {type_s, init};
    JSValue ev = JS_CallConstructor(ctx_, ev_ctor, 2, ctor_args);
    JS_FreeValue(ctx_, type_s);
    JS_FreeValue(ctx_, init);
    JS_FreeValue(ctx_, ev_ctor);
    JS_FreeValue(ctx_, global);

    if (JS_IsException(ev)) {
        report_exception();
        JS_FreeValue(ctx_, ev);
        return false;
    }

    JSValue target_js = wrap_node(target);
    JS_SetPropertyStr(ctx_, ev, "target", JS_DupValue(ctx_, target_js));
    JS_SetPropertyStr(ctx_, ev, "clientX", JS_NewInt32(ctx_, x));
    JS_SetPropertyStr(ctx_, ev, "clientY", JS_NewInt32(ctx_, y));
    JS_SetPropertyStr(ctx_, ev, "button", JS_NewInt32(ctx_, button));
    JS_SetPropertyStr(ctx_, ev, "keyCode", JS_NewInt32(ctx_, key_code));
    JS_SetPropertyStr(ctx_, ev, "which", JS_NewInt32(ctx_, key_code));
    JS_SetPropertyStr(ctx_, ev, "key", JS_NewString(ctx_, key.c_str()));
    JS_SetPropertyStr(ctx_, ev, "ctrlKey", JS_NewBool(ctx_, ctrl));
    JS_SetPropertyStr(ctx_, ev, "shiftKey", JS_NewBool(ctx_, shift));
    JS_SetPropertyStr(ctx_, ev, "altKey", JS_NewBool(ctx_, alt));

    std::vector<litehtml::element::ptr> path;
    auto cur = target;
    while (cur) {
        path.push_back(cur);
        if (!bubbles) break;
        cur = cur->parent();
    }

    int on_idx = -1;
    for (int i = 0; i < 12; ++i) {
        if (type == kOnEventNames[i]) { on_idx = i; break; }
    }

    for (const auto& node : path) {
        JSValue cb_flag = JS_GetPropertyStr(ctx_, ev, "cancelBubble");
        bool stopped = JS_ToBool(ctx_, cb_flag) != 0;
        JS_FreeValue(ctx_, cb_flag);
        if (stopped) break;

        JSValue node_js = wrap_node(node);
        JS_SetPropertyStr(ctx_, ev, "currentTarget", JS_DupValue(ctx_, node_js));

        // 1) Inline HTML attribute on*="..."
        std::string on_attr = "on" + type;
        if (const char* code = node->get_attr(on_attr.c_str())) {
            std::string wrapped = "(function(event){" + std::string(code) + "})";
            JSValue fn = JS_Eval(ctx_, wrapped.c_str(), wrapped.size(), "<onattr>", JS_EVAL_TYPE_GLOBAL);
            if (!JS_IsException(fn)) {
                JSValue r = JS_Call(ctx_, fn, node_js, 1, &ev);
                if (JS_IsException(r)) report_exception();
                if (JS_IsBool(r) && !JS_ToBool(ctx_, r) && cancelable) {
                    JS_SetPropertyStr(ctx_, ev, "defaultPrevented", JS_TRUE);
                }
                JS_FreeValue(ctx_, r);
            } else {
                report_exception();
            }
            JS_FreeValue(ctx_, fn);
        }

        // 2) JS el.on* property
        if (on_idx >= 0) {
            if (auto* rec = find_record_by_el(node)) {
                if (JS_IsFunction(ctx_, rec->on_props[on_idx])) {
                    JSValue fn = JS_DupValue(ctx_, rec->on_props[on_idx]);
                    JSValue r = JS_Call(ctx_, fn, node_js, 1, &ev);
                    if (JS_IsException(r)) report_exception();
                    if (JS_IsBool(r) && !JS_ToBool(ctx_, r) && cancelable) {
                        JS_SetPropertyStr(ctx_, ev, "defaultPrevented", JS_TRUE);
                    }
                    JS_FreeValue(ctx_, r);
                    JS_FreeValue(ctx_, fn);
                }
            }
        }

        // 3) addEventListener handlers
        if (auto* rec = find_record_by_el(node)) {
            std::vector<JSValue> calls;
            for (size_t i = 0; i < rec->listeners.size();) {
                if (rec->listeners[i].type == type) {
                    calls.push_back(JS_DupValue(ctx_, rec->listeners[i].func));
                    if (rec->listeners[i].once) {
                        JS_FreeValue(ctx_, rec->listeners[i].func);
                        rec->listeners.erase(rec->listeners.begin() + i);
                        continue;
                    }
                }
                ++i;
            }
            for (auto& fn : calls) {
                JSValue si = JS_GetPropertyStr(ctx_, ev, "_stopImm");
                bool imm = JS_ToBool(ctx_, si) != 0;
                JS_FreeValue(ctx_, si);
                if (!imm) {
                    JSValue r = JS_Call(ctx_, fn, node_js, 1, &ev);
                    if (JS_IsException(r)) report_exception();
                    JS_FreeValue(ctx_, r);
                }
                JS_FreeValue(ctx_, fn);
            }
        }

        JS_FreeValue(ctx_, node_js);
    }

    JSValue dp = JS_GetPropertyStr(ctx_, ev, "defaultPrevented");
    bool prevented = JS_ToBool(ctx_, dp) != 0;
    JS_FreeValue(ctx_, dp);
    JS_FreeValue(ctx_, target_js);
    JS_FreeValue(ctx_, ev);
    flush_jobs();
    return prevented;
}

void DomBridge::tick(double now_ms) {
    if (!ctx_ || timers_.empty()) {
        flush_jobs();
        return;
    }
    WatchdogScope wd(this, 1500.0);
    std::vector<size_t> due_indices;
    size_t n = timers_.size();
    for (size_t i = 0; i < n; ++i) {
        if (!timers_[i].cancelled && timers_[i].fire_at_ms <= now_ms) {
            due_indices.push_back(i);
        }
    }
    for (size_t idx : due_indices) {
        if (idx >= timers_.size() || timers_[idx].cancelled) continue;
        JSValue fn = JS_DupValue(ctx_, timers_[idx].func);
        bool is_raf = timers_[idx].is_raf;
        std::vector<JSValue> call_args;
        if (is_raf) {
            call_args.push_back(JS_NewFloat64(ctx_, now_ms));
        } else {
            for (const auto& a : timers_[idx].args) call_args.push_back(JS_DupValue(ctx_, a));
        }
        if (timers_[idx].is_interval) {
            timers_[idx].fire_at_ms = now_ms + std::max(4.0, timers_[idx].interval_ms);
        } else {
            timers_[idx].cancelled = true;
        }
        JSValue global = JS_GetGlobalObject(ctx_);
        JSValue r = JS_Call(ctx_, fn, global, static_cast<int>(call_args.size()), call_args.data());
        if (JS_IsException(r)) report_exception();
        JS_FreeValue(ctx_, r);
        JS_FreeValue(ctx_, global);
        JS_FreeValue(ctx_, fn);
        for (auto& a : call_args) JS_FreeValue(ctx_, a);
        if (check_watchdog_expired()) break;
    }

    // Sweep cancelled timers
    for (size_t i = 0; i < timers_.size();) {
        if (timers_[i].cancelled) {
            JS_FreeValue(ctx_, timers_[i].func);
            for (auto& a : timers_[i].args) JS_FreeValue(ctx_, a);
            timers_.erase(timers_.begin() + i);
        } else {
            ++i;
        }
    }
    flush_jobs();
}

void DomBridge::resolve_fetch(uint64_t req_id, int status, const char* resolved_url,
                              const char* headers_json, const char* body, int body_len,
                              const char* err) {
    if (!ctx_) return;
    WatchdogScope wd(this, 1500.0);
    for (size_t i = 0; i < fetches_.size(); ++i) {
        if (fetches_[i].req_id != req_id) continue;
        PendingFetch pf = std::move(fetches_[i]);
        fetches_.erase(fetches_.begin() + i);

        std::string rbody = (body && body_len > 0) ? std::string(body, body_len) : "";
        std::string rurl = (resolved_url && *resolved_url) ? resolved_url : pf.req_url;
        std::string rhdrs = (headers_json && *headers_json) ? headers_json : "{}";

        if (!pf.is_xhr) {
            if (err && *err) {
                JSValue e = JS_NewString(ctx_, err);
                JSValue r = JS_Call(ctx_, pf.reject_fn, JS_UNDEFINED, 1, &e);
                if (JS_IsException(r)) report_exception();
                JS_FreeValue(ctx_, r);
                JS_FreeValue(ctx_, e);
            } else {
                JSValue resp = make_response_object(ctx_, status, rurl, rhdrs, rbody);
                JSValue r = JS_Call(ctx_, pf.resolve_fn, JS_UNDEFINED, 1, &resp);
                if (JS_IsException(r)) report_exception();
                JS_FreeValue(ctx_, r);
                JS_FreeValue(ctx_, resp);
            }
            JS_FreeValue(ctx_, pf.resolve_fn);
            JS_FreeValue(ctx_, pf.reject_fn);
        } else {
            JS_SetPropertyStr(ctx_, pf.xhr_obj, "readyState", JS_NewInt32(ctx_, 4));
            JS_SetPropertyStr(ctx_, pf.xhr_obj, "status", JS_NewInt32(ctx_, status));
            JS_SetPropertyStr(ctx_, pf.xhr_obj, "responseText", JS_NewStringLen(ctx_, rbody.data(), rbody.size()));
            JS_SetPropertyStr(ctx_, pf.xhr_obj, "response", JS_NewStringLen(ctx_, rbody.data(), rbody.size()));
            JSValue hm = JS_ParseJSON(ctx_, rhdrs.c_str(), rhdrs.size(), "<xhr-headers>");
            if (!JS_IsException(hm) && JS_IsObject(hm)) JS_SetPropertyStr(ctx_, pf.xhr_obj, "_resHeaders", hm);
            else JS_FreeValue(ctx_, hm);

            const char* ev1 = "onreadystatechange";
            const char* ev2 = (err && *err) ? "onerror" : "onload";
            for (const char* cbname : {ev1, ev2}) {
                JSValue fn = JS_GetPropertyStr(ctx_, pf.xhr_obj, cbname);
                if (JS_IsFunction(ctx_, fn)) {
                    JSValue r = JS_Call(ctx_, fn, pf.xhr_obj, 0, nullptr);
                    if (JS_IsException(r)) report_exception();
                    JS_FreeValue(ctx_, r);
                }
                JS_FreeValue(ctx_, fn);
            }
            JS_FreeValue(ctx_, pf.xhr_obj);
        }
        flush_jobs();
        return;
    }
}

std::string DomBridge::serialize_text_content(const litehtml::element::ptr& el) {
    if (!el) return "";
    if (el->is_text()) {
        std::string t;
        el->get_text(t);
        return t;
    }
    std::string out;
    for (const auto& ch : el->children()) {
        out += serialize_text_content(ch);
    }
    return out;
}

std::string DomBridge::serialize_element_html(const litehtml::element::ptr& el, bool outer, int depth) {
    if (!el || depth > 64) return "";
    if (el->is_text()) {
        std::string t;
        el->get_text(t);
        return t;
    }
    std::string tag = el->get_tagName() ? to_lower_ascii(el->get_tagName()) : "div";
    if (tag.rfind("__tag_", 0) == 0) return "";
    std::string out;
    if (outer) {
        out += "<" + tag;
        if (const char* id = el->get_attr("id")) out += " id=\"" + std::string(id) + "\"";
        if (const char* cl = el->get_attr("class")) out += " class=\"" + std::string(cl) + "\"";
        if (const char* href = el->get_attr("href")) out += " href=\"" + std::string(href) + "\"";
        if (const char* src = el->get_attr("src")) out += " src=\"" + std::string(src) + "\"";
        out += ">";
    }
    for (const auto& ch : el->children()) {
        out += serialize_element_html(ch, true, depth + 1);
    }
    if (outer && tag != "br" && tag != "hr" && tag != "img" && tag != "input" && tag != "meta" && tag != "link") {
        out += "</" + tag + ">";
    }
    return out;
}

std::string DomBridge::dump_dom_summary() {
    if (!page_ || !page_->doc || !page_->doc->root()) return "";
    std::string out;
    std::function<void(const litehtml::element::ptr&, int)> walk = [&](const litehtml::element::ptr& el, int d) {
        if (!el || d > 24 || out.size() > 16000) return;
        if (el->is_text()) {
            std::string t;
            el->get_text(t);
            t = trim_ascii(t);
            if (!t.empty()) {
                out.append(d * 2, ' ');
                out += "\"" + t.substr(0, 60) + "\"\n";
            }
            return;
        }
        std::string tag = el->get_tagName() ? el->get_tagName() : "?";
        if (tag.rfind("__tag_", 0) == 0) return;
        std::string ltag = to_lower_ascii(tag);
        if (ltag == "style" || ltag == "script") return;
        out.append(d * 2, ' ');
        out += "<" + tag;
        if (const char* id = el->get_attr("id")) out += " #" + std::string(id);
        if (const char* cl = el->get_attr("class")) out += " ." + std::string(cl);
        auto p = el->get_placement();
        char dim[48];
        std::snprintf(dim, sizeof(dim), " [%dx%d@%d,%d]",
                      static_cast<int>(p.width.value()), static_cast<int>(p.height.value()),
                      static_cast<int>(p.x.value()), static_cast<int>(p.y.value()));
        out += std::string(dim) + ">\n";

        std::string text_run;
        auto flush_text = [&]() {
            std::string t = trim_ascii(text_run);
            text_run.clear();
            if (!t.empty()) {
                out.append((d + 1) * 2, ' ');
                out += "\"" + t.substr(0, 80) + "\"\n";
            }
        };
        for (const auto& ch : el->children()) {
            if (!ch) continue;
            if (ch->is_text()) {
                std::string t;
                ch->get_text(t);
                text_run += t;
            } else {
                flush_text();
                walk(ch, d + 1);
            }
        }
        flush_text();
    };
    walk(page_->doc->root(), 0);
    return out;
}

} // namespace space
