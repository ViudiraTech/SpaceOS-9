/*
 *
 *       user/desktop/src/apps/session_ui.rs
 *       Full-screen DDM greeter/OOBE primitives and greeter protocol client
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The greeter and OOBE wizard are ordinary unprivileged X clients.  They share
//! this module for the full-screen window, the wallpaper cache and the bounded
//! socket client used to talk to the privileged DDM daemon.

use crate::ddm::wire::{self, ClientKind, Opcode, Reader, Writer};
use crate::draw::{Canvas, Rect};
use crate::icons;
use crate::text::TextRenderer;
use crate::theme::Theme;
use crate::x11::{Result, X};
use std::io;
use std::os::unix::io::RawFd;
use std::path::Path;

const MAX_FRAME: usize = 128 * 1024;

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct UserInfo {
    pub name: String,
    pub display: String,
}

#[derive(Clone, Debug)]
pub struct UiConfig {
    pub mode: ClientKind,
    pub hostname: String,
    pub last_user: String,
    pub last_session: String,
    pub allow_power: bool,
    pub sessions: Vec<SessionInfo>,
    pub users: Vec<UserInfo>,
    pub locale: String,
    pub timezone: String,
    pub keyboard: String,
    pub version: String,
    pub suggested_user: String,
    pub wallpaper: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum Message {
    Config(UiConfig),
    LoginOk {
        user: String,
        session: String,
    },
    OobeOk {
        user: String,
    },
    PowerOk,
    Pong,
    Error {
        code: u16,
        message: String,
        retry_ms: u32,
    },
}

pub struct Backend {
    fd: RawFd,
    buffer: Vec<u8>,
    pub config: UiConfig,
}

impl Backend {
    pub fn connect(socket: &str, kind: ClientKind) -> Result<Self> {
        let fd = connect_retry(socket)?;
        let mut backend = Self {
            fd,
            buffer: Vec::new(),
            config: UiConfig {
                mode: kind,
                hostname: String::new(),
                last_user: String::new(),
                last_session: String::new(),
                allow_power: false,
                sessions: Vec::new(),
                users: Vec::new(),
                locale: "zh_CN".to_string(),
                timezone: "UTC".to_string(),
                keyboard: "us".to_string(),
                version: String::new(),
                suggested_user: "user".to_string(),
                wallpaper: "/usr/share/spui/wallpapers/alpine-peaks.jpg".to_string(),
            },
        };
        let mut hello = Writer::new(Opcode::Hello);
        hello.u16(wire::PROTOCOL_VERSION).u8(kind as u8);
        backend.send(&hello.finish())?;
        // The daemon sends ServerConfig immediately after Hello.  Wait for it
        // with a bounded timeout rather than assuming a scheduling order.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match backend.read_available()? {
                Some(Message::Config(config)) => {
                    backend.config = config;
                    return Ok(backend);
                }
                Some(Message::Error { message, .. }) => {
                    return Err(format!("daemon rejected greeter: {message}").into())
                }
                Some(_) => {}
                None => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        Err("timed out waiting for DDM configuration".into())
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }

    pub fn send(&self, payload: &[u8]) -> Result<()> {
        let frame = wire::encode_frame(payload)?;
        let mut offset = 0;
        while offset < frame.len() {
            let written = unsafe {
                libc::send(
                    self.fd,
                    frame[offset..].as_ptr() as *const libc::c_void,
                    frame.len() - offset,
                    libc::MSG_NOSIGNAL,
                )
            };
            if written < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if written == 0 {
                return Err("DDM socket closed".into());
            }
            offset += written as usize;
        }
        Ok(())
    }

    pub fn login(&self, user: &str, password: &[u8], session: &str) -> Result<()> {
        let mut writer = Writer::new(Opcode::Login);
        writer.string(user).bytes(password).string(session);
        self.send(&writer.finish())
    }

    pub fn oobe_commit(
        &self,
        user: &str,
        full_name: &str,
        password: &[u8],
        timezone: &str,
        locale: &str,
        keyboard: &str,
        hostname: &str,
        auto_login: bool,
    ) -> Result<()> {
        let mut writer = Writer::new(Opcode::OobeCommit);
        writer
            .string(user)
            .string(full_name)
            .bytes(password)
            .string(timezone)
            .string(locale)
            .string(keyboard)
            .string(hostname)
            .bool(auto_login);
        self.send(&writer.finish())
    }

    pub fn power(&self, action: crate::ddm::wire::PowerAction) -> Result<()> {
        let mut writer = Writer::new(Opcode::Power);
        writer.u8(action as u8);
        self.send(&writer.finish())
    }

    /// Non-blocking read of one complete frame.  `None` means the socket has
    /// no complete frame yet; an error means the daemon closed or the stream
    /// violated the framing contract.
    pub fn read_available(&mut self) -> Result<Option<Message>> {
        // Frames already buffered must be consumed before recv(2): after one
        // recv returns several frames the socket is no longer readable, but
        // the remaining frames are still valid protocol input.
        if let Some(payload) = wire::parse_frame(&mut self.buffer)? {
            return decode_frame(&payload).map(Some);
        }
        let mut chunk = [0u8; 8192];
        let count = unsafe {
            libc::recv(
                self.fd,
                chunk.as_mut_ptr() as *mut libc::c_void,
                chunk.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if count == 0 {
            return Err("DDM closed the greeter socket".into());
        }
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error.into());
        }
        self.buffer.extend_from_slice(&chunk[..count as usize]);
        if self.buffer.len() > MAX_FRAME {
            return Err("DDM frame buffer overflow".into());
        }
        let Some(payload) = wire::parse_frame(&mut self.buffer)? else {
            return Ok(None);
        };
        decode_frame(&payload).map(Some)
    }
}

fn decode_frame(payload: &[u8]) -> Result<Message> {
    let mut reader = Reader::new(payload);
    let opcode = reader.opcode()?;
    let message = match opcode {
        Opcode::ServerConfig => Message::Config(parse_config(&mut reader)?),
        Opcode::LoginOk => Message::LoginOk {
            user: reader.string()?,
            session: reader.string()?,
        },
        Opcode::OobeOk => Message::OobeOk {
            user: reader.string()?,
        },
        Opcode::PowerOk => Message::PowerOk,
        Opcode::Pong => Message::Pong,
        Opcode::Error => Message::Error {
            code: reader.u16()?,
            message: reader.string()?,
            retry_ms: reader.u32()?,
        },
        other => return Err(format!("unexpected DDM opcode {other:?}").into()),
    };
    reader.finish()?;
    Ok(message)
}

impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

fn parse_config(reader: &mut Reader<'_>) -> Result<UiConfig> {
    let mode = ClientKind::from_u8(reader.u8()?).ok_or("invalid client mode")?;
    let hostname = reader.string()?;
    let last_user = reader.string()?;
    let last_session = reader.string()?;
    let allow_power = reader.bool()?;
    let mut sessions = Vec::new();
    for _ in 0..reader.u16()? {
        sessions.push(SessionInfo {
            id: reader.string()?,
            name: reader.string()?,
        });
    }
    let mut users = Vec::new();
    for _ in 0..reader.u16()? {
        users.push(UserInfo {
            name: reader.string()?,
            display: reader.string()?,
        });
    }
    let locale = reader.string()?;
    let timezone = reader.string()?;
    let keyboard = reader.string()?;
    let version = reader.string()?;
    let suggested_user = reader.string()?;
    let wallpaper = reader.string()?;
    Ok(UiConfig {
        mode,
        hostname,
        last_user,
        last_session,
        allow_power,
        sessions,
        users,
        locale,
        timezone,
        keyboard,
        version,
        suggested_user,
        wallpaper,
    })
}

fn connect_retry(path: &str) -> Result<RawFd> {
    let mut last_error = None;
    for _ in 0..100 {
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_bytes();
        if bytes.len() >= address.sun_path.len() {
            unsafe {
                libc::close(fd);
            }
            return Err("DDM socket path is too long".into());
        }
        for (slot, byte) in address.sun_path.iter_mut().zip(bytes.iter()) {
            *slot = *byte as libc::c_char;
        }
        let result = unsafe {
            libc::connect(
                fd,
                &address as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if result == 0 {
            return Ok(fd);
        }
        last_error = Some(io::Error::last_os_error());
        unsafe {
            libc::close(fd);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "cannot connect to DDM".to_string())
        .into())
}

pub struct Screen {
    pub x: X,
    pub window: u32,
    pub width: i32,
    pub height: i32,
    pub canvas: Canvas,
    backing: u32,
    pub text: TextRenderer,
    pub theme: Theme,
    background: Vec<u32>,
    wallpaper_path: String,
    pub needs_redraw: bool,
}

impl Screen {
    pub fn new(wallpaper_path: &str) -> Result<Self> {
        let x = X::connect()?;
        let width = i32::from(x.width);
        let height = i32::from(x.height);
        let win = x.generate_id()?;
        x.create_window(
            win,
            x.root,
            0,
            0,
            width as u16,
            height as u16,
            true,
            x11_event_mask(),
            0x101014,
        )?;
        x.set_title(win, "SpaceOS Login")?;
        x.set_class(win, "spui", "SpuiGreeter")?;
        x.set_wm_type(win, x.atoms.net_wm_type_normal)?;
        x.map(win)?;
        x.raise(win)?;
        let _ = x.focus(win);
        let _ = x.apply_screensaver_timeout(0);
        x.flush()?;

        let mut text =
            TextRenderer::load().map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        text.set_ui_scale(1.0);
        let canvas = x.create_canvas(width as usize, height as usize, 0x101014);
        let backing = x.create_backing(win, width as u16, height as u16)?;
        x.put_canvas(backing, &canvas, canvas.bounds())?;
        x.set_background_pixmap(win, backing)?;
        x.flush()?;
        let theme = Theme::dark_with_accent(0x3b82f6);
        let background = wallpaper(wallpaper_path, width as usize, height as usize);
        Ok(Self {
            x,
            window: win,
            width,
            height,
            canvas,
            backing,
            text,
            theme,
            background,
            wallpaper_path: wallpaper_path.to_string(),
            needs_redraw: true,
        })
    }

    pub fn draw_background(&mut self) {
        self.canvas.copy_background(&self.background);
    }

    pub fn present(&mut self) -> Result<()> {
        let dirty = self.canvas.take_dirty();
        let area = if dirty.is_empty() {
            Rect::new(0, 0, self.width, self.height)
        } else {
            dirty
        };
        self.x.put_canvas(self.backing, &self.canvas, area)?;
        self.x.clear_area(self.window, area)?;
        self.x.flush()?;
        self.needs_redraw = false;
        Ok(())
    }

    pub fn handle_event(&mut self, event: &x11rb::protocol::Event) -> bool {
        use x11rb::protocol::Event;
        match event {
            Event::Expose(expose) if expose.window == self.window => {
                self.needs_redraw = true;
                true
            }
            Event::ConfigureNotify(configure) if configure.window == self.window => {
                if i32::from(configure.width) != self.width
                    || i32::from(configure.height) != self.height
                {
                    self.resize(i32::from(configure.width), i32::from(configure.height));
                }
                true
            }
            _ => false,
        }
    }

    fn resize(&mut self, width: i32, height: i32) {
        let width = width.max(1);
        let height = height.max(1);
        let _ = self.x.free_pixmap(self.backing);
        let _ = self.x.release_canvas(&self.canvas);
        self.width = width;
        self.height = height;
        self.canvas = self
            .x
            .create_canvas(width as usize, height as usize, 0x101014);
        if let Ok(backing) = self
            .x
            .create_backing(self.window, width as u16, height as u16)
        {
            self.backing = backing;
            let _ = self.x.set_background_pixmap(self.window, backing);
        }
        self.background = wallpaper(&self.wallpaper_path, width as usize, height as usize);
        self.needs_redraw = true;
    }

    pub fn wait(&self, backend_fd: RawFd, timeout_ms: i32) -> (bool, bool) {
        let (x_ready, _, backend_ready) = self.x.wait_fds(None, Some(backend_fd), timeout_ms);
        (x_ready, backend_ready)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.x.free_pixmap(self.backing);
        let _ = self.x.release_canvas(&self.canvas);
        let _ = self.x.destroy(self.window);
        let _ = self.x.flush();
    }
}

fn x11_event_mask() -> x11rb::protocol::xproto::EventMask {
    use x11rb::protocol::xproto::EventMask;
    EventMask::EXPOSURE
        | EventMask::STRUCTURE_NOTIFY
        | EventMask::KEY_PRESS
        | EventMask::KEY_RELEASE
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::POINTER_MOTION
        | EventMask::LEAVE_WINDOW
        | EventMask::FOCUS_CHANGE
}

fn wallpaper(path: &str, width: usize, height: usize) -> Vec<u32> {
    let raw = icons::decode_jpeg(Path::new(path));
    let mut buffer = if let Some(sprite) = raw {
        sprite.scale_cover_opaque(width, height)
    } else {
        gradient(width, height)
    };
    // Keep the right-hand login card legible without hiding the wallpaper.
    for y in 0..height {
        for x in 0..width {
            let horizontal = if width > 1 {
                x as f32 / (width - 1) as f32
            } else {
                0.0
            };
            let vertical = if height > 1 {
                y as f32 / (height - 1) as f32
            } else {
                0.0
            };
            let darken = (0.16 + horizontal * 0.30 + vertical * 0.10).clamp(0.0, 0.62);
            let pixel = buffer[y * width + x];
            buffer[y * width + x] = crate::draw::mix(pixel, 0x08080c, darken);
        }
    }
    buffer
}

fn gradient(width: usize, height: usize) -> Vec<u32> {
    let mut buffer = vec![0u32; width * height];
    for y in 0..height {
        let t = if height > 1 {
            y as f32 / (height - 1) as f32
        } else {
            0.0
        };
        let color = crate::draw::mix(0x141a24, 0x2a3442, t);
        for x in 0..width {
            buffer[y * width + x] = color;
        }
    }
    buffer
}

pub fn draw_logo(screen: &mut Screen, x: i32, y: i32, size: u32) {
    icons::draw(&mut screen.canvas, icons::Icon::SpaceLogo, x, y, size, None);
}

pub fn draw_error(screen: &mut Screen, rect: Rect, message: &str) {
    if message.is_empty() {
        return;
    }
    let shown = screen
        .text
        .truncate(message, rect.w as f32, 11.5, crate::text::Face::Ui);
    screen.text.draw(
        &mut screen.canvas,
        &shown,
        rect.x as f32,
        (rect.y + 12) as f32,
        11.5,
        screen.theme.danger,
        crate::text::Face::Ui,
    );
}

pub fn user_initial(display: &str, user: &str) -> String {
    display
        .chars()
        .find(|c| !c.is_whitespace())
        .or_else(|| user.chars().next())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}
