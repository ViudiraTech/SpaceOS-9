/*
 *
 *       user/desktop/src/ime/xim.rs
 *       Pure Rust X11 XIM Protocol Server (`@im=spime`) using `x11rb`.
 *       Supports `_XIM_SERVERS`, `@transport=X/`, `COMPOUND_TEXT` (UTF-8 `\x1b%G`) &
 *       `UTF8_STRING` encoding negotiation, synchronous `XIM_FORWARD_EVENT`, and `spotLocation`.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use spui_ui::x11::X;
use std::collections::HashMap;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageData, ClientMessageEvent,
    ConnectionExt, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    SelectionRequestEvent, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as WrapperExt;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME};

// Standard XIM Major Opcodes (X11R6 XIM Specification).
const XIM_CONNECT: u8 = 1;
const XIM_CONNECT_REPLY: u8 = 2;
const XIM_DISCONNECT: u8 = 3;
const XIM_DISCONNECT_REPLY: u8 = 4;
const XIM_OPEN: u8 = 30;
const XIM_OPEN_REPLY: u8 = 31;
const XIM_CLOSE: u8 = 32;
const XIM_CLOSE_REPLY: u8 = 33;
const XIM_ENCODING_NEGOTIATION: u8 = 38;
const XIM_ENCODING_NEGOTIATION_REPLY: u8 = 39;
const XIM_QUERY_EXTENSION: u8 = 40;
const XIM_QUERY_EXTENSION_REPLY: u8 = 41;
const XIM_GET_IM_VALUES: u8 = 44;
const XIM_GET_IM_VALUES_REPLY: u8 = 45;
const XIM_SET_EVENT_MASK: u8 = 47;
const XIM_CREATE_IC: u8 = 50;
const XIM_CREATE_IC_REPLY: u8 = 51;
const XIM_DESTROY_IC: u8 = 52;
const XIM_DESTROY_IC_REPLY: u8 = 53;
const XIM_SET_IC_VALUES: u8 = 54;
const XIM_SET_IC_VALUES_REPLY: u8 = 55;
const XIM_GET_IC_VALUES: u8 = 56;
const XIM_GET_IC_VALUES_REPLY: u8 = 57;
const XIM_SET_IC_FOCUS: u8 = 58;
const XIM_UNSET_IC_FOCUS: u8 = 59;
const XIM_FORWARD_EVENT: u8 = 60;
const XIM_RESET_IC: u8 = 62;
const XIM_RESET_IC_REPLY: u8 = 63;
const XIM_SYNC: u8 = 70;
const XIM_SYNC_REPLY: u8 = 71;
const XIM_COMMIT: u8 = 75;

const XIM_FLAG_SYNCHRONOUS: u16 = 0x0001;
const XIM_FLAG_LOOKUP_CHARS: u16 = 0x0002;

// XIC Attribute IDs advertised in XIM_OPEN_REPLY
const ATTR_INPUT_STYLE: u16 = 0;
const ATTR_CLIENT_WINDOW: u16 = 1;
const ATTR_FOCUS_WINDOW: u16 = 2;
const ATTR_FILTER_EVENTS: u16 = 3;
const ATTR_PREEDIT_ATTRS: u16 = 4;
const ATTR_SPOT_LOCATION: u16 = 5;
const ATTR_AREA: u16 = 6;
const ATTR_FONT_SET: u16 = 7;

// XIMAttr Type IDs
const XIM_TYPE_WINDOW: u16 = 4;
const XIM_TYPE_CARD32: u16 = 5;
const XIM_TYPE_XRECTANGLE: u16 = 8;
const XIM_TYPE_XPOINT: u16 = 9;
const XIM_TYPE_XFONTSET: u16 = 11;
const XIM_TYPE_XIMSTYLES: u16 = 10;
const XIM_TYPE_NESTEDLIST: u16 = 12;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct XimIc {
    pub ic_id: u16,
    pub im_id: u16,
    pub client_window: Window,
    pub focus_window: Window,
    pub spot_x: i16,
    pub spot_y: i16,
    pub has_spot: bool,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct XimConn {
    pub server_comm: Window,
    pub client_comm: Window,
    pub little_endian: bool,
    pub im_id: u16,
    pub use_compound_text: bool,
    pub ics: HashMap<u16, XimIc>,
    next_ic: u16,
}

impl XimConn {
    fn new(server_comm: Window, client_comm: Window) -> Self {
        Self {
            server_comm,
            client_comm,
            little_endian: true,
            im_id: 1,
            use_compound_text: true,
            ics: HashMap::new(),
            next_ic: 1,
        }
    }
}

/// Event emitted to `spimed` from XIM transport.
pub enum XimAction {
    FocusIn {
        conn_idx: Window,
        ic_id: u16,
        spot: (i32, i32, i32),
    },
    FocusOut {
        conn_idx: Window,
        ic_id: u16,
    },
    SpotUpdated {
        conn_idx: Window,
        ic_id: u16,
        spot: (i32, i32, i32),
    },
    ForwardKey {
        conn_idx: Window,
        im_id: u16,
        ic_id: u16,
        sync_flag: u16,
        serial: u16,
        wire_event: [u8; 32],
        is_press: bool,
        keycode: u8,
        state: u16,
    },
    #[allow(dead_code)]
    ResetIc {
        conn_idx: Window,
        ic_id: u16,
    },
}

pub struct XimServer {
    pub owner: Window,
    atom_server: Atom,
    #[allow(dead_code)]
    atom_xim_servers: Atom,
    atom_locales: Atom,
    atom_transport: Atom,
    atom_xim_xconnect: Atom,
    atom_xim_protocol: Atom,
    atom_xim_moredata: Atom,
    atom_spime_data: Atom,
    #[allow(dead_code)]
    atom_compound_text: Atom,
    pub conns: HashMap<Window, XimConn>,
    more_buf: HashMap<Window, Vec<u8>>,
}

impl XimServer {
    pub fn start(x11: &X) -> Result<Self, Box<dyn std::error::Error>> {
        let intern = |name: &str| -> Result<Atom, Box<dyn std::error::Error>> {
            Ok(x11.conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        let atom_server = intern("@server=spime")?;
        let atom_xim_servers = intern("_XIM_SERVERS")?;
        let atom_locales = intern("LOCALES")?;
        let atom_transport = intern("TRANSPORT")?;
        let atom_xim_xconnect = intern("_XIM_XCONNECT")?;
        let atom_xim_protocol = intern("_XIM_PROTOCOL")?;
        let atom_xim_moredata = intern("_XIM_MOREDATA")?;
        let atom_spime_data = intern("_SPIME_XIM_DATA")?;
        let atom_compound_text = intern("COMPOUND_TEXT")?;

        let owner = x11.generate_id()?;
        x11.conn
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                owner,
                x11.root,
                -10,
                -10,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                COPY_FROM_PARENT,
                &CreateWindowAux::new().override_redirect(1).event_mask(
                    EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY,
                ),
            )?
            .check()?;

        x11.conn
            .set_selection_owner(owner, atom_server, CURRENT_TIME)?
            .check()?;

        // Register `@server=spime` inside `_XIM_SERVERS` on root window.
        let mut servers: Vec<Atom> = x11
            .conn
            .get_property(false, x11.root, atom_xim_servers, AtomEnum::ATOM, 0, 32)?
            .reply()
            .ok()
            .and_then(|r| {
                if r.format == 32 {
                    Some(
                        r.value32()
                            .map(|iter| iter.collect())
                            .unwrap_or_default(),
                    )
                } else {
                    None
                }
            })
            .unwrap_or_default();
        if !servers.contains(&atom_server) {
            servers.insert(0, atom_server);
        }
        x11.conn
            .change_property32(
                PropMode::REPLACE,
                x11.root,
                atom_xim_servers,
                AtomEnum::ATOM,
                &servers,
            )?
            .check()?;
        x11.flush()?;

        Ok(Self {
            owner,
            atom_server,
            atom_xim_servers,
            atom_locales,
            atom_transport,
            atom_xim_xconnect,
            atom_xim_protocol,
            atom_xim_moredata,
            atom_spime_data,
            atom_compound_text,
            conns: HashMap::new(),
            more_buf: HashMap::new(),
        })
    }

    pub fn handle_event(&mut self, x11: &X, ev: &Event) -> Option<XimAction> {
        match ev {
            Event::SelectionRequest(req) => {
                self.handle_selection_request(x11, req);
                None
            }
            Event::DestroyNotify(dn) => {
                if self.conns.remove(&dn.window).is_some() {
                    self.more_buf.remove(&dn.window);
                }
                let dead_keys: Vec<Window> = self
                    .conns
                    .iter()
                    .filter(|(_, c)| c.client_comm == dn.window)
                    .map(|(&k, _)| k)
                    .collect();
                for k in dead_keys {
                    self.conns.remove(&k);
                    self.more_buf.remove(&k);
                }
                None
            }
            Event::ClientMessage(cm) => {
                if cm.window == self.owner && cm.type_ == self.atom_xim_xconnect && cm.format == 32 {
                    let client_comm = cm.data.as_data32()[0];
                    self.accept_xconnect(x11, client_comm);
                    return None;
                }
                if self.conns.contains_key(&cm.window) {
                    if cm.type_ == self.atom_xim_moredata && cm.format == 32 {
                        let d32 = cm.data.as_data32();
                        let len = d32[0] as usize;
                        let prop = d32[1];
                        if let Some(chunk) = read_prop_bytes(x11, cm.window, prop, len) {
                            self.more_buf.entry(cm.window).or_default().extend_from_slice(&chunk);
                        }
                        return None;
                    }
                    if cm.type_ == self.atom_xim_protocol {
                        let pkt = if cm.format == 8 {
                            cm.data.as_data8().to_vec()
                        } else if cm.format == 32 {
                            let d32 = cm.data.as_data32();
                            let len = d32[0] as usize;
                            let prop = d32[1];
                            let tail = read_prop_bytes(x11, cm.window, prop, len)?;
                            if let Some(mut acc) = self.more_buf.remove(&cm.window) {
                                acc.extend_from_slice(&tail);
                                acc
                            } else {
                                tail
                            }
                        } else {
                            return None;
                        };
                        return self.dispatch_packet(x11, cm.window, &pkt);
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn handle_selection_request(&self, x11: &X, req: &SelectionRequestEvent) {
        if req.selection != self.atom_server {
            return;
        }
        let prop = if req.property == 0 {
            req.target
        } else {
            req.property
        };
        let mut ok = true;
        if req.target == self.atom_locales {
            let val = b"@locale=zh_CN.UTF-8,en_US.UTF-8,C";
            let _ = x11.conn.change_property8(
                PropMode::REPLACE,
                req.requestor,
                prop,
                self.atom_locales,
                val,
            );
        } else if req.target == self.atom_transport {
            let val = b"@transport=X/";
            let _ = x11.conn.change_property8(
                PropMode::REPLACE,
                req.requestor,
                prop,
                self.atom_transport,
                val,
            );
        } else {
            ok = false;
        }
        let reply = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property: if ok { prop } else { 0 },
        };
        let _ = x11.conn.send_event(false, req.requestor, EventMask::NO_EVENT, reply);
        let _ = x11.flush();
    }

    fn accept_xconnect(&mut self, x11: &X, client_comm: Window) {
        if client_comm == 0 {
            return;
        }
        let Ok(server_comm) = x11.generate_id() else {
            return;
        };
        if x11
            .conn
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                server_comm,
                x11.root,
                -10,
                -10,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                COPY_FROM_PARENT,
                &CreateWindowAux::new().override_redirect(1).event_mask(
                    EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY,
                ),
            )
            .is_err()
        {
            return;
        }
        // Watch client_comm destroy so we reclaim connection resources immediately on exit.
        let _ = x11.conn.change_window_attributes(
            client_comm,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        );
        self.conns
            .insert(server_comm, XimConn::new(server_comm, client_comm));
        let cm = ClientMessageEvent {
            response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: client_comm,
            type_: self.atom_xim_xconnect,
            data: ClientMessageData::from([server_comm, 0, 2, 20, 0]),
        };
        let _ = x11.conn.send_event(false, client_comm, EventMask::NO_EVENT, cm);
        let _ = x11.flush();
    }

    fn send_packet(&self, x11: &X, server_comm: Window, major: u8, minor: u8, body: &[u8]) {
        let Some(conn) = self.conns.get(&server_comm) else {
            return;
        };
        let pkt = encode_xim_packet(major, minor, body, conn.little_endian);
        if pkt.len() <= 20 {
            let mut raw = [0u8; 20];
            raw[..pkt.len()].copy_from_slice(&pkt);
            let cm = ClientMessageEvent {
                response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
                format: 8,
                sequence: 0,
                window: conn.client_comm,
                type_: self.atom_xim_protocol,
                data: ClientMessageData::from(raw),
            };
            let _ = x11
                .conn
                .send_event(false, conn.client_comm, EventMask::NO_EVENT, cm);
        } else {
            let _ = x11.conn.change_property8(
                PropMode::APPEND,
                conn.client_comm,
                self.atom_spime_data,
                AtomEnum::STRING,
                &pkt,
            );
            let cm = ClientMessageEvent {
                response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
                format: 32,
                sequence: 0,
                window: conn.client_comm,
                type_: self.atom_xim_protocol,
                data: ClientMessageData::from([
                    pkt.len() as u32,
                    self.atom_spime_data,
                    0,
                    0,
                    0,
                ]),
            };
            let _ = x11
                .conn
                .send_event(false, conn.client_comm, EventMask::NO_EVENT, cm);
        }
        let _ = x11.flush();
    }

    fn dispatch_packet(&mut self, x11: &X, server_comm: Window, pkt: &[u8]) -> Option<XimAction> {
        if pkt.len() < 4 {
            return None;
        }
        let major = pkt[0];
        let body = &pkt[4..];
        let mut conn = self.conns.get(&server_comm)?.clone();
        let le = if major == XIM_CONNECT && !body.is_empty() {
            body[0] != b'B'
        } else {
            conn.little_endian
        };

        let mut action = None;
        match major {
            XIM_CONNECT => {
                conn.little_endian = le;
                self.conns.insert(server_comm, conn);
                let mut rep = Vec::with_capacity(4);
                push_u16(&mut rep, 1, le);
                push_u16(&mut rep, 0, le);
                self.send_packet(x11, server_comm, XIM_CONNECT_REPLY, 0, &rep);
            }
            XIM_DISCONNECT => {
                self.send_packet(x11, server_comm, XIM_DISCONNECT_REPLY, 0, &[]);
                let _ = x11.conn.destroy_window(server_comm);
                self.conns.remove(&server_comm);
                self.more_buf.remove(&server_comm);
                let _ = x11.flush();
            }
            XIM_OPEN => {
                let rep = build_open_reply(conn.im_id, le);
                self.send_packet(x11, server_comm, XIM_OPEN_REPLY, 0, &rep);
            }
            XIM_CLOSE => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let mut rep = Vec::with_capacity(4);
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, 0, le);
                self.send_packet(x11, server_comm, XIM_CLOSE_REPLY, 0, &rep);
            }
            XIM_QUERY_EXTENSION => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let mut rep = Vec::with_capacity(4);
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, 0, le);
                self.send_packet(x11, server_comm, XIM_QUERY_EXTENSION_REPLY, 0, &rep);
            }
            XIM_ENCODING_NEGOTIATION => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let list_len = get_u16(body, 2, le).unwrap_or(0) as usize;
                let enc_slice = body.get(4..4 + list_len).unwrap_or(&[]);
                let (chosen_idx, use_ct) = negotiate_encoding(enc_slice);
                conn.use_compound_text = use_ct;
                self.conns.insert(server_comm, conn);
                let mut rep = Vec::with_capacity(8);
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, 0, le); // category = name
                push_u16(&mut rep, chosen_idx as u16, le);
                push_u16(&mut rep, 0, le);
                self.send_packet(x11, server_comm, XIM_ENCODING_NEGOTIATION_REPLY, 0, &rep);
            }
            XIM_GET_IM_VALUES => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let rep = build_get_im_values_reply(im_id, le);
                self.send_packet(x11, server_comm, XIM_GET_IM_VALUES_REPLY, 0, &rep);
            }
            XIM_CREATE_IC => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let attrs_len = get_u16(body, 2, le).unwrap_or(0) as usize;
                let ic_id = conn.next_ic.max(1);
                conn.next_ic = ic_id.wrapping_add(1).max(1);
                let mut ic = XimIc {
                    ic_id,
                    im_id,
                    client_window: 0,
                    focus_window: 0,
                    spot_x: 0,
                    spot_y: 0,
                    has_spot: false,
                };
                if let Some(slice) = body.get(4..4 + attrs_len) {
                    apply_ic_attrs(slice, le, &mut ic);
                }
                conn.ics.insert(ic_id, ic);
                self.conns.insert(server_comm, conn);

                // Tell libX11 to synchronously forward KeyPress (0x01) + KeyRelease (0x02).
                let mut mask_msg = Vec::with_capacity(12);
                push_u16(&mut mask_msg, im_id, le);
                push_u16(&mut mask_msg, ic_id, le);
                push_u32(&mut mask_msg, 0x0000_0003, le);
                push_u32(&mut mask_msg, 0x0000_0003, le);
                self.send_packet(x11, server_comm, XIM_SET_EVENT_MASK, 0, &mask_msg);

                let mut rep = Vec::with_capacity(4);
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, ic_id, le);
                self.send_packet(x11, server_comm, XIM_CREATE_IC_REPLY, 0, &rep);
            }
            XIM_DESTROY_IC => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let ic_id = get_u16(body, 2, le).unwrap_or(0);
                conn.ics.remove(&ic_id);
                self.conns.insert(server_comm, conn);
                let mut rep = Vec::with_capacity(4);
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, ic_id, le);
                self.send_packet(x11, server_comm, XIM_DESTROY_IC_REPLY, 0, &rep);
            }
            XIM_SET_IC_VALUES => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let ic_id = get_u16(body, 2, le).unwrap_or(0);
                let attrs_len = get_u16(body, 4, le).unwrap_or(0) as usize;
                let mut spot_changed = false;
                if let Some(ic) = conn.ics.get_mut(&ic_id) {
                    let prev = (ic.spot_x, ic.spot_y, ic.has_spot);
                    if let Some(slice) = body.get(8..8 + attrs_len) {
                        apply_ic_attrs(slice, le, ic);
                    }
                    spot_changed = (ic.spot_x, ic.spot_y, ic.has_spot) != prev;
                    if spot_changed {
                        let spot = resolve_ic_spot(x11, ic);
                        action = Some(XimAction::SpotUpdated {
                            conn_idx: server_comm,
                            ic_id,
                            spot,
                        });
                    }
                }
                self.conns.insert(server_comm, conn);
                let mut rep = Vec::with_capacity(4);
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, ic_id, le);
                self.send_packet(x11, server_comm, XIM_SET_IC_VALUES_REPLY, 0, &rep);
                let _ = spot_changed;
            }
            XIM_GET_IC_VALUES => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let ic_id = get_u16(body, 2, le).unwrap_or(0);
                let cnt = get_u16(body, 4, le).unwrap_or(0) as usize;
                let mut asked_filter = false;
                for i in 0..cnt {
                    if get_u16(body, 6 + i * 2, le) == Some(ATTR_FILTER_EVENTS) {
                        asked_filter = true;
                    }
                }
                let mut attrs = Vec::new();
                if asked_filter {
                    push_u16(&mut attrs, ATTR_FILTER_EVENTS, le);
                    push_u16(&mut attrs, 4, le);
                    push_u32(&mut attrs, 0x0000_0003, le);
                }
                let mut rep = Vec::with_capacity(8 + attrs.len());
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, ic_id, le);
                push_u16(&mut rep, attrs.len() as u16, le);
                push_u16(&mut rep, 0, le);
                rep.extend_from_slice(&attrs);
                self.send_packet(x11, server_comm, XIM_GET_IC_VALUES_REPLY, 0, &rep);
            }
            XIM_SET_IC_FOCUS => {
                let ic_id = get_u16(body, 2, le).unwrap_or(0);
                if let Some(ic) = conn.ics.get(&ic_id) {
                    let spot = resolve_ic_spot(x11, ic);
                    action = Some(XimAction::FocusIn {
                        conn_idx: server_comm,
                        ic_id,
                        spot,
                    });
                }
            }
            XIM_UNSET_IC_FOCUS => {
                let ic_id = get_u16(body, 2, le).unwrap_or(0);
                action = Some(XimAction::FocusOut {
                    conn_idx: server_comm,
                    ic_id,
                });
            }
            XIM_RESET_IC => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let ic_id = get_u16(body, 2, le).unwrap_or(0);
                action = Some(XimAction::ResetIc {
                    conn_idx: server_comm,
                    ic_id,
                });
                let mut rep = Vec::with_capacity(8);
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, ic_id, le);
                push_u16(&mut rep, 0, le);
                push_u16(&mut rep, 0, le);
                self.send_packet(x11, server_comm, XIM_RESET_IC_REPLY, 0, &rep);
            }
            XIM_FORWARD_EVENT => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let ic_id = get_u16(body, 2, le).unwrap_or(0);
                let flag = get_u16(body, 4, le).unwrap_or(0);
                let serial = get_u16(body, 6, le).unwrap_or(0);
                if let Some(ev_bytes) = body.get(8..40) {
                    let mut wire = [0u8; 32];
                    wire.copy_from_slice(ev_bytes);
                    let rtype = wire[0] & 0x7f;
                    let keycode = wire[1];
                    let state = u16::from_le_bytes([wire[28], wire[29]]);
                    if rtype == 2 || rtype == 3 {
                        action = Some(XimAction::ForwardKey {
                            conn_idx: server_comm,
                            im_id,
                            ic_id,
                            sync_flag: flag & XIM_FLAG_SYNCHRONOUS,
                            serial,
                            wire_event: wire,
                            is_press: rtype == 2,
                            keycode,
                            state,
                        });
                    } else {
                        self.forward_unconsumed(x11, server_comm, im_id, ic_id, flag & 1, serial, &wire);
                    }
                }
            }
            XIM_SYNC => {
                let im_id = get_u16(body, 0, le).unwrap_or(conn.im_id);
                let ic_id = get_u16(body, 2, le).unwrap_or(0);
                let mut rep = Vec::with_capacity(4);
                push_u16(&mut rep, im_id, le);
                push_u16(&mut rep, ic_id, le);
                self.send_packet(x11, server_comm, XIM_SYNC_REPLY, 0, &rep);
            }
            _ => {}
        }
        action
    }

    pub fn forward_unconsumed(
        &self,
        x11: &X,
        server_comm: Window,
        im_id: u16,
        ic_id: u16,
        sync_flag: u16,
        serial: u16,
        wire: &[u8; 32],
    ) {
        let Some(conn) = self.conns.get(&server_comm) else {
            return;
        };
        let le = conn.little_endian;
        let mut msg = Vec::with_capacity(40);
        push_u16(&mut msg, im_id, le);
        push_u16(&mut msg, ic_id, le);
        push_u16(&mut msg, sync_flag & XIM_FLAG_SYNCHRONOUS, le);
        push_u16(&mut msg, serial, le);
        msg.extend_from_slice(wire);
        self.send_packet(x11, server_comm, XIM_FORWARD_EVENT, 0, &msg);
    }

    pub fn send_sync_reply(&self, x11: &X, server_comm: Window, im_id: u16, ic_id: u16) {
        let Some(conn) = self.conns.get(&server_comm) else {
            return;
        };
        let le = conn.little_endian;
        let mut rep = Vec::with_capacity(4);
        push_u16(&mut rep, im_id, le);
        push_u16(&mut rep, ic_id, le);
        self.send_packet(x11, server_comm, XIM_SYNC_REPLY, 0, &rep);
    }

    pub fn commit_text(
        &self,
        x11: &X,
        server_comm: Window,
        im_id: u16,
        ic_id: u16,
        sync_flag: u16,
        text: &str,
    ) {
        let Some(conn) = self.conns.get(&server_comm) else {
            return;
        };
        let le = conn.little_endian;
        let encoded = if conn.use_compound_text {
            encode_compound_text_utf8(text)
        } else {
            text.as_bytes().to_vec()
        };
        let flag = XIM_FLAG_LOOKUP_CHARS | (sync_flag & XIM_FLAG_SYNCHRONOUS);
        let mut msg = Vec::with_capacity(8 + encoded.len() + 4);
        push_u16(&mut msg, im_id, le);
        push_u16(&mut msg, ic_id, le);
        push_u16(&mut msg, flag, le);
        push_u16(&mut msg, encoded.len() as u16, le);
        msg.extend_from_slice(&encoded);
        while msg.len() % 4 != 0 {
            msg.push(0);
        }
        self.send_packet(x11, server_comm, XIM_COMMIT, 0, &msg);
    }
}

fn read_prop_bytes(x11: &X, window: Window, prop: Atom, expected_len: usize) -> Option<Vec<u8>> {
    if prop == 0 || expected_len == 0 {
        return None;
    }
    let words = ((expected_len + 3) / 4).max(1) as u32;
    let reply = x11
        .conn
        .get_property(true, window, prop, AtomEnum::ANY, 0, words)
        .ok()?
        .reply()
        .ok()?;
    let mut val = reply.value;
    if val.len() > expected_len {
        val.truncate(expected_len);
    }
    Some(val)
}

fn resolve_ic_spot(x11: &X, ic: &XimIc) -> (i32, i32, i32) {
    let win = if ic.focus_window != 0 {
        ic.focus_window
    } else {
        ic.client_window
    };
    if win == 0 {
        return (120, i32::from(x11.height) - 140, 22);
    }
    let (wx, wy, _, wh, _, _, _) = x11.get_window_info(win).unwrap_or((0, 0, 0, 0, 0, false, false));
    let trans = x11
        .conn
        .translate_coordinates(win, x11.root, 0, 0)
        .ok()
        .and_then(|c| c.reply().ok());
    let (rx, ry) = trans.map_or((i32::from(wx), i32::from(wy)), |t| {
        (i32::from(t.dst_x), i32::from(t.dst_y))
    });
    if ic.has_spot {
        (rx + i32::from(ic.spot_x), ry + i32::from(ic.spot_y), 20)
    } else {
        (rx + 24, (ry + i32::from(wh) - 42).max(ry + 36), 22)
    }
}

/// Encode UTF-8 string into ISO-2022 Compound Text UTF-8 designation (`ESC % G <utf8> ESC % @`).
pub fn encode_compound_text_utf8(s: &str) -> Vec<u8> {
    if s.is_ascii() {
        return s.as_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(s.len() + 6);
    out.extend_from_slice(b"\x1b%G");
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(b"\x1b%@");
    out
}

fn negotiate_encoding(raw: &[u8]) -> (i16, bool) {
    let mut pos = 0usize;
    let mut idx = 0i16;
    let mut compound_idx = None;
    let mut utf8_idx = None;
    while pos < raw.len() {
        let len = usize::from(raw[pos]);
        pos += 1;
        if pos + len > raw.len() {
            break;
        }
        if let Ok(name) = std::str::from_utf8(&raw[pos..pos + len]) {
            if name.eq_ignore_ascii_case("UTF8_STRING") {
                utf8_idx = Some(idx);
            } else if name.eq_ignore_ascii_case("COMPOUND_TEXT") {
                compound_idx = Some(idx);
            }
        }
        pos += len;
        idx += 1;
    }
    if let Some(i) = utf8_idx {
        (i, false)
    } else if let Some(i) = compound_idx {
        (i, true)
    } else {
        (0, true)
    }
}

fn encode_xim_packet(major: u8, minor: u8, body: &[u8], le: bool) -> Vec<u8> {
    let padded = (body.len() + 3) & !3;
    let len_words = (padded / 4) as u16;
    let mut pkt = Vec::with_capacity(4 + padded);
    pkt.push(major);
    pkt.push(minor);
    push_u16(&mut pkt, len_words, le);
    pkt.extend_from_slice(body);
    pkt.resize(4 + padded, 0);
    pkt
}

fn encode_attr_desc(id: u16, kind: u16, name: &[u8], le: bool, out: &mut Vec<u8>) {
    push_u16(out, id, le);
    push_u16(out, kind, le);
    push_u16(out, name.len() as u16, le);
    out.extend_from_slice(name);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

fn build_open_reply(im_id: u16, le: bool) -> Vec<u8> {
    let mut im_attrs = Vec::new();
    encode_attr_desc(0, XIM_TYPE_XIMSTYLES, b"queryInputStyle", le, &mut im_attrs);

    let mut ic_attrs = Vec::new();
    encode_attr_desc(ATTR_INPUT_STYLE, XIM_TYPE_CARD32, b"inputStyle", le, &mut ic_attrs);
    encode_attr_desc(ATTR_CLIENT_WINDOW, XIM_TYPE_WINDOW, b"clientWindow", le, &mut ic_attrs);
    encode_attr_desc(ATTR_FOCUS_WINDOW, XIM_TYPE_WINDOW, b"focusWindow", le, &mut ic_attrs);
    encode_attr_desc(ATTR_FILTER_EVENTS, XIM_TYPE_CARD32, b"filterEvents", le, &mut ic_attrs);
    encode_attr_desc(
        ATTR_PREEDIT_ATTRS,
        XIM_TYPE_NESTEDLIST,
        b"preeditAttributes",
        le,
        &mut ic_attrs,
    );
    encode_attr_desc(ATTR_SPOT_LOCATION, XIM_TYPE_XPOINT, b"spotLocation", le, &mut ic_attrs);
    encode_attr_desc(ATTR_AREA, XIM_TYPE_XRECTANGLE, b"area", le, &mut ic_attrs);
    encode_attr_desc(ATTR_FONT_SET, XIM_TYPE_XFONTSET, b"fontSet", le, &mut ic_attrs);

    let mut out = Vec::with_capacity(8 + im_attrs.len() + ic_attrs.len());
    push_u16(&mut out, im_id, le);
    push_u16(&mut out, im_attrs.len() as u16, le);
    out.extend_from_slice(&im_attrs);
    push_u16(&mut out, ic_attrs.len() as u16, le);
    push_u16(&mut out, 0, le);
    out.extend_from_slice(&ic_attrs);
    out
}

fn build_get_im_values_reply(im_id: u16, le: bool) -> Vec<u8> {
    // 4 supported styles: PreeditPosition|StatusNothing (0x0404), PreeditNothing|StatusNothing (0x0408),
    // PreeditPosition|StatusNone (0x0804), PreeditNothing|StatusNone (0x0808).
    let styles: [u32; 4] = [0x0404, 0x0408, 0x0804, 0x0808];
    let mut style_val = Vec::with_capacity(4 + styles.len() * 4);
    push_u16(&mut style_val, styles.len() as u16, le);
    push_u16(&mut style_val, 0, le);
    for st in styles {
        push_u32(&mut style_val, st, le);
    }

    let mut attr_item = Vec::with_capacity(4 + style_val.len());
    push_u16(&mut attr_item, 0, le);
    push_u16(&mut attr_item, style_val.len() as u16, le);
    attr_item.extend_from_slice(&style_val);

    let mut out = Vec::with_capacity(4 + attr_item.len());
    push_u16(&mut out, im_id, le);
    push_u16(&mut out, attr_item.len() as u16, le);
    out.extend_from_slice(&attr_item);
    out
}

fn apply_ic_attrs(slice: &[u8], le: bool, ic: &mut XimIc) {
    let mut pos = 0usize;
    while pos + 4 <= slice.len() {
        let Some(attr_id) = get_u16(slice, pos, le) else {
            break;
        };
        let Some(vlen) = get_u16(slice, pos + 2, le).map(usize::from) else {
            break;
        };
        pos += 4;
        if pos + vlen > slice.len() {
            break;
        }
        let val = &slice[pos..pos + vlen];
        match attr_id {
            ATTR_CLIENT_WINDOW => {
                if let Some(w) = get_u32(val, 0, le) {
                    ic.client_window = w;
                }
            }
            ATTR_FOCUS_WINDOW => {
                if let Some(w) = get_u32(val, 0, le) {
                    ic.focus_window = w;
                }
            }
            ATTR_SPOT_LOCATION => {
                if let (Some(sx), Some(sy)) = (get_u16(val, 0, le), get_u16(val, 2, le)) {
                    ic.spot_x = sx as i16;
                    ic.spot_y = sy as i16;
                    ic.has_spot = true;
                }
            }
            ATTR_PREEDIT_ATTRS => apply_ic_attrs(val, le, ic),
            _ => {}
        }
        let step = (vlen + 3) & !3;
        pos += step;
    }
}

#[inline]
fn push_u16(out: &mut Vec<u8>, v: u16, le: bool) {
    out.extend_from_slice(&if le { v.to_le_bytes() } else { v.to_be_bytes() });
}

#[inline]
fn push_u32(out: &mut Vec<u8>, v: u32, le: bool) {
    out.extend_from_slice(&if le { v.to_le_bytes() } else { v.to_be_bytes() });
}

#[inline]
fn get_u16(b: &[u8], off: usize, le: bool) -> Option<u16> {
    let s: [u8; 2] = b.get(off..off + 2)?.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(s)
    } else {
        u16::from_be_bytes(s)
    })
}

#[inline]
fn get_u32(b: &[u8], off: usize, le: bool) -> Option<u32> {
    let s: [u8; 4] = b.get(off..off + 4)?.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(s)
    } else {
        u32::from_be_bytes(s)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_text_wraps_utf8_cleanly() {
        assert_eq!(encode_compound_text_utf8("abc"), b"abc");
        let ct = encode_compound_text_utf8("你好");
        assert!(ct.starts_with(b"\x1b%G"));
        assert!(ct.ends_with(b"\x1b%@"));
        assert!(ct.windows(6).any(|w| w == "你好".as_bytes()));
    }

    #[test]
    fn xim_open_and_nested_spot_parse_cleanly() {
        let open = build_open_reply(1, true);
        assert_eq!(get_u16(&open, 0, true), Some(1));
        let im_len = get_u16(&open, 2, true).unwrap() as usize;
        assert!(im_len > 0 && im_len % 4 == 0);
        let ic_len = get_u16(&open, 4 + im_len, true).unwrap() as usize;
        assert!(ic_len > 0 && ic_len % 4 == 0);

        // Nested preeditAttributes holding spotLocation (120, 340)
        let mut inner = Vec::new();
        push_u16(&mut inner, ATTR_SPOT_LOCATION, true);
        push_u16(&mut inner, 4, true);
        push_u16(&mut inner, 120, true);
        push_u16(&mut inner, 340, true);

        let mut outer = Vec::new();
        push_u16(&mut outer, ATTR_PREEDIT_ATTRS, true);
        push_u16(&mut outer, inner.len() as u16, true);
        outer.extend_from_slice(&inner);

        let mut ic = XimIc {
            ic_id: 1,
            im_id: 1,
            client_window: 0,
            focus_window: 0,
            spot_x: 0,
            spot_y: 0,
            has_spot: false,
        };
        apply_ic_attrs(&outer, true, &mut ic);
        assert!(ic.has_spot);
        assert_eq!((ic.spot_x, ic.spot_y), (120, 340));
    }
}
