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
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::thread;
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

const MAX_CONCURRENT_SFX: usize = 4;

struct PlaybackGuard {
    last_played: [(SystemSound, Option<Instant>); 5],
}

impl PlaybackGuard {
    const fn new() -> Self {
        Self {
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
static SFX_SENDER: OnceLock<SyncSender<(SystemSound, bool)>> = OnceLock::new();

fn sfx_sender() -> &'static SyncSender<(SystemSound, bool)> {
    SFX_SENDER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<(SystemSound, bool)>(4);
        let _ = thread::Builder::new()
            .name("spui-sfx".to_string())
            .spawn(move || {
                let mut active: Vec<Child> = Vec::new();
                while let Ok((kind, bypass_cooldown)) = rx.recv() {
                    active.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
                    if active.len() >= MAX_CONCURRENT_SFX {
                        if !bypass_cooldown {
                            continue;
                        }
                        let mut oldest = active.remove(0);
                        let _ = oldest.kill();
                        let _ = oldest.wait();
                    }
                    let path = kind.asset_path();
                    if let Ok(child) = Command::new(WAVPLAY_BIN)
                        .args(["--scale", "100", &path])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                    {
                        active.push(child);
                    }
                }
            });
        tx
    })
}

fn spawn_wav(kind: SystemSound, bypass_cooldown: bool) -> bool {
    let path = kind.asset_path();
    if !Path::new(WAVPLAY_BIN).exists() || !Path::new(&path).exists() {
        return false;
    }
    let now = Instant::now();
    let Ok(mut state) = PLAYER_STATE.lock() else {
        return false;
    };
    let last_slot = state.slot_mut(kind);
    if !bypass_cooldown
        && last_slot.is_some_and(|prev| now.saturating_duration_since(prev) < kind.cooldown())
    {
        return false;
    }
    *last_slot = Some(now);
    drop(state);

    // Never block the caller (UI thread) on child spawning or reaping.
    sfx_sender().try_send((kind, bypass_cooldown)).is_ok()
}

/// Non-blocking playback honoring user mute, master volume, global SFX enable & per-event flag.
pub fn play(cfg: &DesktopConfig, kind: SystemSound) {
    if !kind.is_enabled(cfg) {
        return;
    }
    let _ = spawn_wav(kind, false);
}

/// Explicit preview playback from Settings ("试听") honoring mute state.
pub fn preview(cfg: &DesktopConfig, kind: SystemSound) -> bool {
    if cfg.sound_muted || cfg.sound_master_volume <= 0 {
        return false;
    }
    spawn_wav(kind, true)
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

/// One `Master Playback {Volume,Switch}` control discovered on a card.
#[derive(Clone, Copy)]
struct CtlElem {
    id: SndCtlElemId,
    kind: u32,
    count: usize,
    min: i64,
    max: i64,
}

impl CtlElem {
    fn query(fd: libc::c_int, id: SndCtlElemId) -> Option<Self> {
        // SAFETY: `SndCtlElemInfo` is zero-initialized POD passed to SNDRV_CTL_IOCTL_ELEM_INFO.
        let mut info: SndCtlElemInfo = unsafe { zeroed() };
        info.id = id;
        if unsafe { libc::ioctl(fd, SNDRV_CTL_IOCTL_ELEM_INFO as _, &mut info) } < 0 {
            return None;
        }
        let (min, max) = if info.kind == SNDRV_CTL_ELEM_TYPE_INTEGER {
            // SAFETY: `kind == INTEGER` means the `integer` union member is populated.
            let range = unsafe { info.value.integer };
            (range.min, range.max)
        } else {
            (0, 1)
        };
        Some(Self {
            id: info.id,
            kind: info.kind,
            count: (info.count as usize).clamp(1, 8),
            min,
            max,
        })
    }

    /// Write `raw` to every channel with one `ELEM_WRITE` (no read-modify-write:
    /// all channels get the same value, as `amixer sset Master N%` does).
    fn write(&self, fd: libc::c_int, raw: i64) -> bool {
        let value = if self.kind == SNDRV_CTL_ELEM_TYPE_BOOLEAN {
            i64::from(raw != 0)
        } else {
            raw.clamp(self.min, self.max)
        };
        // SAFETY: zero-initialized POD with the `integer` variant matching the kernel ABI.
        let mut ctl_val: SndCtlElemValue = unsafe { zeroed() };
        ctl_val.id = self.id;
        unsafe {
            for slot in &mut ctl_val.value.integer[..self.count] {
                *slot = value;
            }
            libc::ioctl(fd, SNDRV_CTL_IOCTL_ELEM_WRITE as _, &mut ctl_val) >= 0
        }
    }
}

/// An open `/dev/snd/controlC<n>` with its master controls resolved once.
struct CardMixer {
    file: fs::File,
    volume: Option<CtlElem>,
    switch: Option<CtlElem>,
    applied: Option<(i32, bool)>,
}

impl CardMixer {
    fn open(card_num: u32) -> Option<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/dev/snd/controlC{card_num}"))
            .ok()?;
        let fd = file.as_raw_fd();
        let mut volume = None;
        let mut switch = None;

        let mut ids = [SndCtlElemId::from_mixer_name(""); 32];
        // SAFETY: zero-initialized POD; `pids` points at `ids` with room for `space` entries.
        let mut list: SndCtlElemList = unsafe { zeroed() };
        list.space = ids.len() as u32;
        list.pids = ids.as_mut_ptr() as u64;
        if unsafe { libc::ioctl(fd, SNDRV_CTL_IOCTL_ELEM_LIST as _, &mut list) } >= 0 {
            let used = (list.used as usize).min(ids.len());
            for id in &ids[..used] {
                match id.name_str() {
                    "Master Playback Volume" => volume = CtlElem::query(fd, *id),
                    "Master Playback Switch" => switch = CtlElem::query(fd, *id),
                    _ => {}
                }
            }
        }
        if volume.is_none() {
            volume = CtlElem::query(fd, SndCtlElemId::from_mixer_name("Master Playback Volume"));
        }
        if switch.is_none() {
            switch = CtlElem::query(fd, SndCtlElemId::from_mixer_name("Master Playback Switch"));
        }
        if volume.is_none() && switch.is_none() {
            return None;
        }
        Some(Self {
            file,
            volume,
            switch,
            applied: None,
        })
    }

    /// Returns `false` if the device rejected a write (card removed / reset).
    fn apply(&mut self, effective_vol: i32, unmuted: bool) -> bool {
        let fd = self.file.as_raw_fd();
        let prev = self.applied;
        let mut ok = true;
        if let Some(switch) = self.switch {
            if prev.is_none_or(|(_, was_unmuted)| was_unmuted != unmuted) {
                ok &= switch.write(fd, i64::from(unmuted));
            }
        }
        if let Some(volume) = self.volume {
            if prev.is_none_or(|(was_vol, _)| was_vol != effective_vol) {
                ok &= volume.write(fd, i64::from(effective_vol));
            }
        }
        self.applied = ok.then_some((effective_vol, unmuted));
        ok
    }
}

struct MixerCache {
    cards: Vec<(u32, CardMixer)>,
    scanned: Option<Instant>,
}

/// Control handles stay open across calls: a volume-slider drag issues one
/// `ELEM_WRITE` per changed value instead of re-parsing `/proc/asound/cards`,
/// reopening the control node and re-running LIST/INFO/READ for every motion
/// event, which used to make dragging the slider stutter.
static MIXER_CACHE: Mutex<MixerCache> = Mutex::new(MixerCache {
    cards: Vec::new(),
    scanned: None,
});

const MIXER_RESCAN_INTERVAL: Duration = Duration::from_secs(5);

fn rescan_cards(cache: &mut MixerCache) {
    let mut numbers: Vec<u32> = parse_cards().into_iter().map(|(num, _)| num).collect();
    if numbers.is_empty() {
        numbers = vec![0, 1];
    }
    cache.cards = numbers
        .into_iter()
        .filter_map(|num| CardMixer::open(num).map(|mixer| (num, mixer)))
        .collect();
    cache.scanned = Some(Instant::now());
}

/// Compute hardware mixer `(effective_volume, unmuted)` tuple cleanly.
pub(crate) fn effective_mixer_params(volume: i32, muted: bool) -> (i32, bool) {
    let unmuted = !muted && volume > 0;
    let effective = if unmuted { volume.clamp(0, 100) } else { 0 };
    (effective, unmuted)
}

/// Apply master volume (0..=100) and mute state to all active ALSA sound cards.
pub fn apply_mixer_volume(volume: i32, muted: bool) {
    let (effective, unmuted) = effective_mixer_params(volume, muted);
    let Ok(mut cache) = MIXER_CACHE.lock() else {
        return;
    };
    let stale = cache
        .scanned
        .is_none_or(|at| cache.cards.is_empty() && at.elapsed() >= MIXER_RESCAN_INTERVAL);
    if stale {
        rescan_cards(&mut cache);
    }
    let mut failed = false;
    for (_, card) in cache.cards.iter_mut() {
        failed |= !card.apply(effective, unmuted);
    }
    if failed {
        // A card went away or was re-probed: reopen once and retry.
        rescan_cards(&mut cache);
        for (_, card) in cache.cards.iter_mut() {
            let _ = card.apply(effective, unmuted);
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

    #[test]
    fn effective_mixer_params_clamps_and_mutes() {
        assert_eq!(effective_mixer_params(80, false), (80, true));
        assert_eq!(effective_mixer_params(150, false), (100, true));
        assert_eq!(effective_mixer_params(0, false), (0, false));
        assert_eq!(effective_mixer_params(60, true), (0, false));
        assert_eq!(effective_mixer_params(-10, false), (0, false));
    }
}
