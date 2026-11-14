/*
 *
 *       user/desktop/src/sound.rs
 *       SPUI 4.0 ALSA hardware mixer controller and system sound effect engine
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#![allow(dead_code)]

use std::fs::{self, OpenOptions};
use std::mem::{size_of, zeroed};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uxconf::DesktopConfig;

const SOUND_DIR: &str = "/usr/share/sounds/spui";
const WAVPLAY_BIN: &str = "/bin/wavplay";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemSound {
    Startup,
    Click,
    Notify,
    Alert,
    Volume,
}

impl SystemSound {
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Startup => "startup.wav",
            Self::Click => "click.wav",
            Self::Notify => "notify.wav",
            Self::Alert => "alert.wav",
            Self::Volume => "volume.wav",
        }
    }

    pub fn asset_path(self) -> String {
        format!("{SOUND_DIR}/{}", self.file_name())
    }

    pub fn is_enabled(self, cfg: &DesktopConfig) -> bool {
        if !cfg.sound_enabled || cfg.sound_muted || cfg.sound_master_volume <= 0 {
            return false;
        }
        match self {
            Self::Startup => cfg.sound_event_startup,
            Self::Click => cfg.sound_event_ui,
            Self::Notify => cfg.sound_event_notify,
            Self::Alert => cfg.sound_event_alert,
            Self::Volume => cfg.sound_event_volume,
        }
    }

    const fn cooldown(self) -> Duration {
        match self {
            Self::Startup => Duration::from_millis(500),
            Self::Click => Duration::from_millis(38),
            Self::Notify => Duration::from_millis(90),
            Self::Alert => Duration::from_millis(90),
            Self::Volume => Duration::from_millis(60),
        }
    }
}

struct PlaybackGuard {
    active: Option<Child>,
    last_played: [(SystemSound, Option<Instant>); 5],
}

impl PlaybackGuard {
    const fn new() -> Self {
        Self {
            active: None,
            last_played: [
                (SystemSound::Startup, None),
                (SystemSound::Click, None),
                (SystemSound::Notify, None),
                (SystemSound::Alert, None),
                (SystemSound::Volume, None),
            ],
        }
    }

    fn slot_mut(&mut self, kind: SystemSound) -> &mut Option<Instant> {
        let idx = match kind {
            SystemSound::Startup => 0,
            SystemSound::Click => 1,
            SystemSound::Notify => 2,
            SystemSound::Alert => 3,
            SystemSound::Volume => 4,
        };
        &mut self.last_played[idx].1
    }
}

static PLAYER_STATE: Mutex<PlaybackGuard> = Mutex::new(PlaybackGuard::new());

fn spawn_wav(kind: SystemSound, volume: i32, bypass_cooldown: bool) -> bool {
    let path = kind.asset_path();
    if !Path::new(WAVPLAY_BIN).exists() || !Path::new(&path).exists() {
        return false;
    }
    let now = Instant::now();
    let Ok(mut state) = PLAYER_STATE.lock() else {
        return false;
    };
    if let Some(child) = state.active.as_mut() {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => state.active = None,
            Ok(None) => {
                if !bypass_cooldown {
                    return false;
                }
            }
        }
    }
    let last_slot = state.slot_mut(kind);
    if !bypass_cooldown
        && last_slot.is_some_and(|prev| now.saturating_duration_since(prev) < kind.cooldown())
    {
        return false;
    }
    *last_slot = Some(now);

    let scale_str = volume.clamp(1, 100).to_string();
    if let Ok(child) = Command::new(WAVPLAY_BIN)
        .args(["--scale", &scale_str, &path])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        state.active = Some(child);
        true
    } else {
        false
    }
}

/// Non-blocking playback honoring user mute, master volume, global SFX enable & per-event flag.
pub fn play(cfg: &DesktopConfig, kind: SystemSound) {
    if !kind.is_enabled(cfg) {
        return;
    }
    let _ = spawn_wav(kind, cfg.sound_master_volume, false);
}

/// Explicit preview playback from Settings ("试听") honoring mute state.
pub fn preview(cfg: &DesktopConfig, kind: SystemSound) -> bool {
    if cfg.sound_muted || cfg.sound_master_volume <= 0 {
        return false;
    }
    spawn_wav(kind, cfg.sound_master_volume, true)
}

// ---- ALSA Control ABI (/dev/snd/controlC<n>) --------------------------------

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn alsa_iowr<T>(nr: u8) -> u32 {
    ((IOC_READ | IOC_WRITE) << IOC_DIRSHIFT)
        | ((b'U' as u32) << IOC_TYPESHIFT)
        | ((nr as u32) << IOC_NRSHIFT)
        | ((size_of::<T>() as u32) << IOC_SIZESHIFT)
}

const SNDRV_CTL_ELEM_TYPE_BOOLEAN: u32 = 1;
const SNDRV_CTL_ELEM_TYPE_INTEGER: u32 = 2;
const SNDRV_CTL_ELEM_IFACE_MIXER: u32 = 2;
const SNDRV_CTL_ELEM_ID_NAME_MAXLEN: usize = 44;

const SNDRV_CTL_IOCTL_ELEM_LIST: u32 = alsa_iowr::<SndCtlElemList>(0x10);
const SNDRV_CTL_IOCTL_ELEM_INFO: u32 = alsa_iowr::<SndCtlElemInfo>(0x11);
const SNDRV_CTL_IOCTL_ELEM_READ: u32 = alsa_iowr::<SndCtlElemValue>(0x12);
const SNDRV_CTL_IOCTL_ELEM_WRITE: u32 = alsa_iowr::<SndCtlElemValue>(0x13);

#[repr(C)]
#[derive(Clone, Copy)]
struct SndCtlElemId {
    numid: u32,
    iface: u32,
    device: u32,
    subdevice: u32,
    name: [u8; SNDRV_CTL_ELEM_ID_NAME_MAXLEN],
    index: u32,
}

impl SndCtlElemId {
    fn from_mixer_name(name: &str) -> Self {
        let mut id: Self = unsafe { zeroed() };
        id.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
        let bytes = name.as_bytes();
        let copy_len = bytes.len().min(SNDRV_CTL_ELEM_ID_NAME_MAXLEN - 1);
        id.name[..copy_len].copy_from_slice(&bytes[..copy_len]);
        id
    }

    fn name_str(&self) -> &str {
        let len = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.name.len());
        std::str::from_utf8(&self.name[..len]).unwrap_or("")
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SndCtlElemList {
    offset: u32,
    space: u32,
    used: u32,
    count: u32,
    pids: u64,
    reserved: [u8; 50],
    _pad: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SndCtlElemInfoInt {
    min: i64,
    max: i64,
    step: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
union SndCtlElemInfoVal {
    integer: SndCtlElemInfoInt,
    raw: [u8; 128],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SndCtlElemInfo {
    id: SndCtlElemId,
    kind: u32,
    access: u32,
    count: u32,
    owner: i32,
    value: SndCtlElemInfoVal,
    reserved: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
union SndCtlElemValData {
    integer: [i64; 128],
    raw: [u8; 1024],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SndCtlElemValue {
    id: SndCtlElemId,
    indirect: u32,
    _pad0: u32,
    value: SndCtlElemValData,
    reserved: [u8; 128],
}

fn parse_cards() -> Vec<(u32, String)> {
    let Ok(text) = fs::read_to_string("/proc/asound/cards") else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let digits_len = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits_len == 0 {
            continue;
        }
        let Ok(num) = trimmed[..digits_len].parse::<u32>() else {
            continue;
        };
        let label = trimmed[digits_len..].trim().to_string();
        cards.push((num, label));
    }
    cards
}

/// Return a human-readable label for the primary ALSA playback card.
pub fn detected_card_name() -> String {
    let cards = parse_cards();
    for (num, label) in &cards {
        if label.contains("HDA") {
            return format!("声卡 {num}: Intel HDA 高保真音频");
        }
    }
    for (num, label) in &cards {
        if label.contains("SB16") {
            return format!("声卡 {num}: Sound Blaster 16");
        }
    }
    if let Some((num, label)) = cards.first() {
        return format!(
            "声卡 {num}: {}",
            if label.is_empty() { "ALSA PCM" } else { label }
        );
    }
    "未检测到 ALSA 声卡（静音占位）".to_string()
}

fn set_elem_integers(fd: libc::c_int, id: SndCtlElemId, raw_value: i64, clamp_to_info: bool) {
    // SAFETY: `SndCtlElemInfo` is zero-initialized POD and passed directly to SpaceKernel SNDRV_CTL_IOCTL_ELEM_INFO.
    let mut info: SndCtlElemInfo = unsafe { zeroed() };
    info.id = id;
    let res = unsafe { libc::ioctl(fd, SNDRV_CTL_IOCTL_ELEM_INFO as _, &mut info) };
    if res < 0 {
        return;
    }

    let val = if clamp_to_info && info.kind == SNDRV_CTL_ELEM_TYPE_INTEGER {
        // SAFETY: `info.kind == INTEGER` means the `integer` union member is populated.
        let range = unsafe { info.value.integer };
        raw_value.clamp(range.min, range.max)
    } else if info.kind == SNDRV_CTL_ELEM_TYPE_BOOLEAN {
        i64::from(raw_value != 0)
    } else {
        raw_value
    };

    // SAFETY: `SndCtlElemValue` is zero-initialized POD with `integer` variant matching kernel ABI.
    let mut ctl_val: SndCtlElemValue = unsafe { zeroed() };
    ctl_val.id = info.id;
    let count = (info.count as usize).clamp(1, 8);
    unsafe {
        let _ = libc::ioctl(fd, SNDRV_CTL_IOCTL_ELEM_READ as _, &mut ctl_val);
        for slot in &mut ctl_val.value.integer[..count] {
            *slot = val;
        }
        let _ = libc::ioctl(fd, SNDRV_CTL_IOCTL_ELEM_WRITE as _, &mut ctl_val);
    }
}

fn apply_card_mixer(card_num: u32, effective_vol: i32, unmuted: bool) {
    let ctl_path = format!("/dev/snd/controlC{card_num}");
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&ctl_path) else {
        return;
    };
    let fd = file.as_raw_fd();

    // Query control list so we discover both Master Playback Volume and Switch.
    let mut ids = [SndCtlElemId::from_mixer_name(""); 32];
    // SAFETY: `SndCtlElemList` is zero-initialized POD and `pids` points to valid `ids` buffer of length 32.
    let mut list: SndCtlElemList = unsafe { zeroed() };
    list.space = ids.len() as u32;
    list.pids = ids.as_mut_ptr() as u64;
    let list_ok = unsafe { libc::ioctl(fd, SNDRV_CTL_IOCTL_ELEM_LIST as _, &mut list) } >= 0;

    let mut applied_vol = false;
    if list_ok {
        let used = (list.used as usize).min(ids.len());
        for id in &ids[..used] {
            let name = id.name_str();
            if name == "Master Playback Volume" {
                set_elem_integers(fd, *id, i64::from(effective_vol), true);
                applied_vol = true;
            } else if name == "Master Playback Switch" {
                set_elem_integers(fd, *id, i64::from(unmuted), false);
            }
        }
    }

    if !applied_vol {
        set_elem_integers(
            fd,
            SndCtlElemId::from_mixer_name("Master Playback Volume"),
            i64::from(effective_vol),
            true,
        );
        set_elem_integers(
            fd,
            SndCtlElemId::from_mixer_name("Master Playback Switch"),
            i64::from(unmuted),
            false,
        );
    }
}

/// Apply master volume (0..=100) and mute state to all active ALSA sound cards.
pub fn apply_mixer_volume(volume: i32, muted: bool) {
    let unmuted = !muted && volume > 0;
    let effective = if unmuted { volume.clamp(0, 100) } else { 0 };
    let cards = parse_cards();
    if cards.is_empty() {
        for num in 0..=1 {
            apply_card_mixer(num, effective, unmuted);
        }
    } else {
        for (num, _) in cards {
            apply_card_mixer(num, effective, unmuted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alsa_ctl_abi_sizes_match_kernel() {
        assert_eq!(size_of::<SndCtlElemId>(), 64);
        assert_eq!(size_of::<SndCtlElemList>(), 80);
        assert_eq!(size_of::<SndCtlElemInfo>(), 272);
        assert_eq!(size_of::<SndCtlElemValue>(), 1224);
        assert_eq!(SNDRV_CTL_IOCTL_ELEM_LIST, 0xc050_5510);
        assert_eq!(SNDRV_CTL_IOCTL_ELEM_INFO, 0xc110_5511);
        assert_eq!(SNDRV_CTL_IOCTL_ELEM_READ, 0xc4c8_5512);
        assert_eq!(SNDRV_CTL_IOCTL_ELEM_WRITE, 0xc4c8_5513);
    }

    #[test]
    fn sound_config_gating_respects_mute_and_event_flags() {
        let mut cfg = DesktopConfig::default();
        assert_eq!(cfg.sound_master_volume, 80);
        assert!(!cfg.sound_muted);
        assert!(SystemSound::Startup.is_enabled(&cfg));
        assert!(SystemSound::Click.is_enabled(&cfg));
        assert!(SystemSound::Notify.is_enabled(&cfg));
        assert!(SystemSound::Alert.is_enabled(&cfg));
        assert!(SystemSound::Volume.is_enabled(&cfg));

        cfg.sound_muted = true;
        assert!(!SystemSound::Click.is_enabled(&cfg));

        cfg.sound_muted = false;
        cfg.sound_event_ui = false;
        assert!(!SystemSound::Click.is_enabled(&cfg));
        assert!(SystemSound::Notify.is_enabled(&cfg));

        cfg.sound_enabled = false;
        assert!(!SystemSound::Notify.is_enabled(&cfg));
    }
}
