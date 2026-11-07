/*
 *
 *       user/desktop/src/netmgr.rs
 *       SPUI client re-exports for SpaceOS NetworkManager
 *
 *       2026/11/06 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Re-exports the shared `uxconf::net` NetworkManager status, NETLINK_ROUTE
//! event watcher, control IPC, and ICMP probe client APIs for SPUI Shell and
//! System Settings. The NetworkManager daemon itself runs as an independent
//! `systemg` service (`/usr/bin/NetworkManager` via `NetworkManager.service`).

pub use uxconf::net::{
    drain_events, open_event_socket, parse_prefix_or_netmask, ping_once, query_status,
    request_apply, request_connect, request_renew, NetStatus,
};
