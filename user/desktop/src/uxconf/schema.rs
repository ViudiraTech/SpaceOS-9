/*
 *
 *       user/desktop/src/uxconf/schema.rs
 *       Built-in channel schemas, default values and type constraints
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Built-in channel definitions for SpaceOS-9 / SPUI. Known properties enforce
//! their declared `ValueKind` while arbitrary user/third-party channels and
//! custom properties remain freely writable, matching `xfconfd` semantics.

use crate::{ConfError, Value, ValueKind};

pub const BUILTIN_CHANNELS: [&str; 12] = [
    "appearance",
    "desktop",
    "panel",
    "wm",
    "display",
    "input",
    "sound",
    "terminal",
    "files",
    "editor",
    "datetime",
    "network",
];

/// Default ordered Win8/8.1 Start Screen tile list (`id:size`, comma-separated).
pub const DEFAULT_START_TILES: &str = "status-clock:wide,status-net:wide,status-audio:wide,status-sys:wide,app-settings:wide,app-files:medium,app-term:medium,app-monitor:medium,app-music:medium,app-edit:small,app-calc:small,app-mines:small,app-view:small,app-help:small";

/// Return all default `(property, Value)` entries for a built-in `channel`.
pub fn channel_defaults(channel: &str) -> Vec<(&'static str, Value)> {
    match channel {
        "appearance" => vec![
            ("/theme/id", Value::String("spui-4".to_string())),
            ("/theme/style", Value::String("spui".to_string())),
            ("/theme/mode", Value::String("dark".to_string())),
            ("/theme/accent", Value::String("#3b82f6".to_string())),
            ("/font/scale", Value::Int(100)),
        ],
        "desktop" => vec![
            (
                "/backdrop/wallpaper",
                Value::String("alpine-peaks.jpg".to_string()),
            ),
            ("/backdrop/style", Value::String("cover".to_string())),
            (
                "/backdrop/solid-color",
                Value::String("#1e293b".to_string()),
            ),
            ("/icons/visible", Value::Bool(true)),
            ("/icons/size", Value::Int(40)),
        ],
        "panel" => vec![
            ("/position", Value::String("bottom".to_string())),
            ("/size", Value::Int(46)),
            ("/show-pinned", Value::Bool(true)),
            ("/start-fullscreen", Value::Bool(true)),
            (
                "/start-tiles",
                Value::String(DEFAULT_START_TILES.to_string()),
            ),
        ],
        "wm" => vec![
            ("/button-layout", Value::String("right".to_string())),
            (
                "/double-click-action",
                Value::String("maximize".to_string()),
            ),
            ("/focus-mode", Value::String("click".to_string())),
            ("/auto-raise", Value::Bool(false)),
            ("/placement", Value::String("cascade".to_string())),
        ],
        "display" => vec![
            ("/resolution", Value::String("1280x800".to_string())),
            ("/blank-timeout-sec", Value::Int(0)),
        ],
        "input" => vec![
            ("/mouse/left-handed", Value::Bool(false)),
            ("/mouse/speed", Value::Int(3)),
            ("/mouse/double-click-ms", Value::Int(380)),
            ("/mouse/natural-scroll", Value::Bool(false)),
            ("/keyboard/repeat-enabled", Value::Bool(true)),
            ("/keyboard/repeat-delay-ms", Value::Int(400)),
            ("/keyboard/repeat-rate-hz", Value::Int(30)),
        ],
        "sound" => vec![
            ("/master-volume", Value::Int(80)),
            ("/muted", Value::Bool(false)),
            ("/system-sounds-enabled", Value::Bool(true)),
            ("/event-startup", Value::Bool(true)),
            ("/event-ui", Value::Bool(true)),
            ("/event-notify", Value::Bool(true)),
            ("/event-alert", Value::Bool(true)),
            ("/event-volume", Value::Bool(true)),
            ("/theme", Value::String("modern-glass".to_string())),
        ],
        "terminal" => vec![
            ("/font-size", Value::Int(14)),
            ("/cursor-style", Value::String("block".to_string())),
            ("/scrollback-lines", Value::Int(1000)),
        ],
        "files" => vec![
            ("/show-hidden", Value::Bool(false)),
            ("/folders-first", Value::Bool(true)),
            ("/single-click-open", Value::Bool(false)),
        ],
        "editor" => vec![
            ("/font-size", Value::Int(14)),
            ("/show-line-numbers", Value::Bool(true)),
            ("/highlight-active-line", Value::Bool(true)),
            ("/tab-width", Value::Int(4)),
        ],
        "datetime" => vec![
            ("/clock-24h", Value::Bool(true)),
            ("/clock-seconds", Value::Bool(false)),
            ("/clock-show-date", Value::Bool(true)),
            ("/timezone", Value::String("UTC".to_string())),
        ],
        "network" => vec![
            ("/address", Value::String("10.0.2.15".to_string())),
            ("/apply-serial", Value::Int(0)),
            ("/dns", Value::String("10.0.2.3".to_string())),
            ("/enabled", Value::Bool(true)),
            ("/gateway", Value::String("10.0.2.2".to_string())),
            ("/mode", Value::String("dhcp".to_string())),
            ("/prefixlen", Value::Int(24)),
        ],
        _ => Vec::new(),
    }
}

/// Look up the compiled default value for `(channel, property)` if defined in the built-in schema.
pub fn schema_default(channel: &str, property: &str) -> Option<Value> {
    channel_defaults(channel)
        .into_iter()
        .find_map(|(k, v)| (k == property).then_some(v))
}

/// Expected `ValueKind` for a built-in schema property, if any.
pub fn schema_kind(channel: &str, property: &str) -> Option<ValueKind> {
    schema_default(channel, property).map(|v| v.kind())
}

/// Check that `value` matches the built-in schema's `ValueKind` when `(channel, property)` is known.
pub fn validate_schema_type(channel: &str, property: &str, value: &Value) -> Result<(), ConfError> {
    if let Some(expected) = schema_kind(channel, property) {
        let actual = value.kind();
        if actual != expected {
            return Err(ConfError::TypeMismatch { expected, actual });
        }
    }
    Ok(())
}
