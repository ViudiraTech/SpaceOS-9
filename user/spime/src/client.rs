/*
 *
 *       user/spime/src/client.rs
 *       Low-latency non-blocking-safe Unix domain socket IPC client for `spimed`.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use crate::proto::{read_frame, socket_path, write_frame, ClientRequest, DaemonReply};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_millis(150);

fn is_idempotent_after_write(req: &ClientRequest) -> bool {
    !matches!(
        req,
        ClientRequest::KeyEvent { .. } | ClientRequest::ToggleMode { .. }
    )
}

/// Reconnecting IPC client embedded in SPUI `App`, `space-browser`, and `Shell`.
pub struct SpimeClient {
    client_id: u32,
    stream: Option<UnixStream>,
    last_connect_attempt: Option<Instant>,
    last_spot: Option<(i16, i16, i16)>,
    pub chinese_mode: bool,
}

impl SpimeClient {
    pub fn new(client_id: u32) -> Self {
        Self {
            client_id,
            stream: None,
            last_connect_attempt: None,
            last_spot: None,
            chinese_mode: false,
        }
    }

    fn ensure_connected(&mut self) -> bool {
        if self.stream.is_some() {
            return true;
        }
        let now = Instant::now();
        if let Some(prev) = self.last_connect_attempt {
            if now.duration_since(prev) < CONNECT_RETRY_INTERVAL {
                return false;
            }
        }
        self.last_connect_attempt = Some(now);
        let path = socket_path();
        let Ok(stream) = UnixStream::connect(&path) else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        self.stream = Some(stream);
        self.last_spot = None;
        true
    }

    fn roundtrip(&mut self, req: &ClientRequest) -> Option<DaemonReply> {
        if !self.ensure_connected() {
            return None;
        }
        let payload = req.encode();
        for attempt in 0..2 {
            let Some(ref mut stream) = self.stream else {
                if attempt == 0 && self.ensure_connected() {
                    continue;
                }
                return None;
            };
            if write_frame(stream, &payload).is_ok() {
                if let Ok(raw) = read_frame(stream) {
                    if let Ok(reply) = DaemonReply::decode(&raw) {
                        self.chinese_mode = reply.chinese_mode;
                        return Some(reply);
                    }
                }
                // Drop the socket after a read timeout/EOF so future calls reconnect cleanly,
                // and never resend non-idempotent requests (e.g. ToggleMode) that the daemon
                // may have already processed.
                self.stream = None;
                self.last_spot = None;
                if !is_idempotent_after_write(req) {
                    return None;
                }
            } else {
                self.stream = None;
                self.last_spot = None;
            }
            if attempt == 0 {
                self.last_connect_attempt = None;
                let _ = self.ensure_connected();
            }
        }
        None
    }

    pub fn focus_in(&mut self, window_id: u32) {
        self.last_spot = None;
        let _ = self.roundtrip(&ClientRequest::FocusIn {
            client_id: self.client_id,
            window_id,
        });
    }

    pub fn focus_out(&mut self, window_id: u32) {
        self.last_spot = None;
        let _ = self.roundtrip(&ClientRequest::FocusOut {
            client_id: self.client_id,
            window_id,
        });
    }

    pub fn set_spot(&mut self, x: i32, y: i32, h: i32) {
        let sx = x.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let sy = y.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let sh = h.clamp(1, i16::MAX as i32) as i16;
        if self.last_spot == Some((sx, sy, sh)) {
            return;
        }
        self.last_spot = Some((sx, sy, sh));
        let _ = self.roundtrip(&ClientRequest::SetSpot {
            client_id: self.client_id,
            x: sx,
            y: sy,
            h: sh,
        });
    }

    pub fn key_event(
        &mut self,
        window_id: u32,
        is_press: bool,
        keysym: u32,
        state: u16,
        keycode: u16,
    ) -> Option<DaemonReply> {
        self.roundtrip(&ClientRequest::KeyEvent {
            client_id: self.client_id,
            window_id,
            is_press,
            keysym,
            state,
            keycode,
        })
    }

    pub fn reset(&mut self) {
        let _ = self.roundtrip(&ClientRequest::Reset {
            client_id: self.client_id,
        });
    }

    pub fn toggle_mode(&mut self) -> Option<bool> {
        self.roundtrip(&ClientRequest::ToggleMode {
            client_id: self.client_id,
        })
        .map(|r| r.chinese_mode)
    }

    pub fn set_chinese_mode(&mut self, chinese: bool) -> Option<bool> {
        self.roundtrip(&ClientRequest::SetChineseMode {
            client_id: self.client_id,
            chinese,
        })
        .map(|r| r.chinese_mode)
    }

    pub fn query_chinese_mode(&mut self) -> Option<bool> {
        self.roundtrip(&ClientRequest::QueryStatus)
            .map(|r| r.chinese_mode)
    }

    pub fn poll_pending(&mut self) -> Option<DaemonReply> {
        self.roundtrip(&ClientRequest::PollPending {
            client_id: self.client_id,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn toggle_mode_is_not_retried_after_write_when_read_fails() {
        let sock = std::env::temp_dir().join(format!("spime-client-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_bg = Arc::clone(&hits);
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut s, _)) = listener.accept() {
                    let _ = read_frame(&mut s);
                    hits_bg.fetch_add(1, Ordering::SeqCst);
                    // Close immediately without replying to simulate EOF after write.
                    drop(s);
                    break;
                }
            }
        });

        let mut client = SpimeClient::new(1);
        let stream = UnixStream::connect(&sock).unwrap();
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        client.stream = Some(stream);
        assert_eq!(client.toggle_mode(), None);
        assert!(!client.is_connected());
        let _ = handle.join();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_file(&sock);
    }
}
