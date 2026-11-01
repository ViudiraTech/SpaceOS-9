/*
 *
 *       src/tty/vt.rs
 *       Linux virtual terminal and keyboard mode ioctls
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The interface display servers use to take over the console, per
//! `include/uapi/linux/vt.h` and `kd.h`: graphics mode stops text rendering,
//! `K_OFF` detaches the keyboard from the line discipline, and `VT_PROCESS`
//! gives the owner a release and acquire handshake around VT switches.

use super::console;
use crate::{sync::SpinLock, user::uaccess, vfs::VfsError};
use core::sync::atomic::{AtomicBool, Ordering};

const KIOCSOUND: u64 = 0x4B2F;
const KDMKTONE: u64 = 0x4B30;
const KDGETLED: u64 = 0x4B31;
const KDSETLED: u64 = 0x4B32;
const KDGKBTYPE: u64 = 0x4B33;
const KDSETMODE: u64 = 0x4B3A;
const KDGETMODE: u64 = 0x4B3B;
const KDGKBMODE: u64 = 0x4B44;
const KDSKBMODE: u64 = 0x4B45;
const KDGKBMETA: u64 = 0x4B62;
const KDSKBMETA: u64 = 0x4B63;
const KDGKBLED: u64 = 0x4B64;
const KDSKBLED: u64 = 0x4B65;
const VT_OPENQRY: u64 = 0x5600;
const VT_GETMODE: u64 = 0x5601;
const VT_SETMODE: u64 = 0x5602;
const VT_GETSTATE: u64 = 0x5603;
const VT_RELDISP: u64 = 0x5605;
const VT_ACTIVATE: u64 = 0x5606;
const VT_WAITACTIVE: u64 = 0x5607;
const VT_DISALLOCATE: u64 = 0x5608;
const VT_RESIZE: u64 = 0x5609;
const VT_LOCKSWITCH: u64 = 0x560B;
const VT_UNLOCKSWITCH: u64 = 0x560C;

const KD_TEXT: u32 = 0;
const KD_GRAPHICS: u32 = 1;
const K_RAW: u32 = 0;
const K_XLATE: u32 = 1;
const K_MEDIUMRAW: u32 = 2;
const K_UNICODE: u32 = 3;
const K_OFF: u32 = 4;
const KB_101: u8 = 2;
const VT_AUTO: u8 = 0;
const VT_PROCESS: u8 = 1;
const VT_ACKACQ: u64 = 2;

#[derive(Clone, Copy)]
struct VtMode {
    mode: u8,
    waitv: u8,
    relsig: i16,
    acqsig: i16,
    frsig: i16,
    owner: u64,
}

const VT_COUNT: usize = 8;

struct State {
    modes: [VtMode; VT_COUNT],
    display_mode: [u32; VT_COUNT],
    keyboard_mode: [u32; VT_COUNT],
    leds: u8,
    switch_locked: bool,
    /// Target of a switch waiting for the current owner's `VT_RELDISP`.
    pending_target: Option<usize>,
}

static STATE: SpinLock<State> = SpinLock::new(State {
    modes: [VtMode {
        mode: VT_AUTO,
        waitv: 0,
        relsig: 0,
        acqsig: 0,
        frsig: 0,
        owner: 0,
    }; VT_COUNT],
    display_mode: [KD_TEXT; VT_COUNT],
    keyboard_mode: [K_UNICODE; VT_COUNT],
    leds: 0,
    switch_locked: false,
    pending_target: None,
});

/// True while the active VT's keyboard input belongs to the line discipline.
static KEYBOARD_TO_TTY: AtomicBool = AtomicBool::new(true);
/// True while a display server owns the screen and text must not be drawn.
static GRAPHICS_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn keyboard_to_tty() -> bool {
    KEYBOARD_TO_TTY.load(Ordering::Acquire)
}

pub fn graphics_active() -> bool {
    GRAPHICS_ACTIVE.load(Ordering::Acquire)
}

fn active_vt() -> usize {
    console::status().active_vt
}

fn publish(state: &State, active: usize) {
    let index = active.saturating_sub(1).min(VT_COUNT - 1);
    KEYBOARD_TO_TTY.store(state.keyboard_mode[index] != K_OFF, Ordering::Release);
    GRAPHICS_ACTIVE.store(state.display_mode[index] == KD_GRAPHICS, Ordering::Release);
}

fn send_signal(owner: u64, signal: i16) {
    if owner != 0 && signal > 0 {
        crate::signal::send_to_process(owner, crate::signal::Signal::kernel(signal as u32));
    }
}

/// Finish switching to `target` and notify a `VT_PROCESS` owner.
fn complete_switch(state: &mut State, target: usize) {
    console::switch_to(target);
    publish(state, target);
    let mode = state.modes[target - 1];
    if mode.mode == VT_PROCESS {
        send_signal(mode.owner, mode.acqsig);
    }
    crate::ipc::wake_pollers();
}

fn begin_switch(target: usize) -> Result<(), VfsError> {
    if !(1..=VT_COUNT).contains(&target) {
        return Err(VfsError::NoSuchDeviceOrAddress);
    }
    let mut state = STATE.lock();
    if state.switch_locked {
        return Err(VfsError::Busy);
    }
    let current = active_vt();
    if current == target {
        return Ok(());
    }
    let mode = state.modes[current - 1];
    if mode.mode == VT_PROCESS {
        state.pending_target = Some(target);
        send_signal(mode.owner, mode.relsig);
        return Ok(());
    }
    complete_switch(&mut state, target);
    Ok(())
}

/// VT and KD ioctls. `vt` is the terminal this descriptor names, 0 for the
/// foreground one. Returns `None` for commands outside this family.
pub fn ioctl(vt: usize, command: u64, arg: u64) -> Option<Result<u64, VfsError>> {
    let index = if vt == 0 { active_vt() } else { vt }
        .saturating_sub(1)
        .min(VT_COUNT - 1);
    let result: Result<u64, VfsError> = match command {
        KIOCSOUND | KDMKTONE => Ok(0),
        KDGKBTYPE => uaccess::write_value(arg, &KB_101).map(|()| 0),
        KDGETLED | KDGKBLED => {
            let leds = STATE.lock().leds;
            uaccess::write_value(arg, &leds).map(|()| 0)
        }
        KDSETLED | KDSKBLED => {
            STATE.lock().leds = arg as u8 & 7;
            Ok(0)
        }
        KDGKBMETA => uaccess::write_value(arg, &0x03u32).map(|()| 0),
        KDSKBMETA => Ok(0),
        KDGETMODE => {
            let mode = STATE.lock().display_mode[index];
            uaccess::write_value(arg, &mode).map(|()| 0)
        }
        KDSETMODE => match arg as u32 {
            KD_TEXT | KD_GRAPHICS => {
                let mut state = STATE.lock();
                let changed = state.display_mode[index] != arg as u32;
                state.display_mode[index] = arg as u32;
                let active = active_vt();
                publish(&state, active);
                drop(state);
                if changed && arg as u32 == KD_TEXT && index + 1 == active {
                    console::repaint(active);
                }
                Ok(0)
            }
            _ => Err(VfsError::InvalidArgument),
        },
        KDGKBMODE => {
            let mode = STATE.lock().keyboard_mode[index];
            uaccess::write_value(arg, &mode).map(|()| 0)
        }
        KDSKBMODE => match arg as u32 {
            K_RAW | K_XLATE | K_MEDIUMRAW | K_UNICODE | K_OFF => {
                let mut state = STATE.lock();
                state.keyboard_mode[index] = arg as u32;
                publish(&state, active_vt());
                Ok(0)
            }
            _ => Err(VfsError::InvalidArgument),
        },
        VT_OPENQRY => {
            let state = STATE.lock();
            let active = active_vt();
            // Prefer a VT no process has claimed for itself.
            let free = (1..=VT_COUNT)
                .rev()
                .find(|&number| {
                    number != active
                        && state.modes[number - 1].mode == VT_AUTO
                        && state.display_mode[number - 1] == KD_TEXT
                })
                .map_or(-1, |number| number as i32);
            drop(state);
            uaccess::write_value(arg, &free).map(|()| 0)
        }
        VT_GETSTATE => {
            let raw: [u16; 3] = [
                active_vt() as u16,
                0,
                0xFFFF & (((1u32 << VT_COUNT) - 1) << 1) as u16,
            ];
            uaccess::write_value(arg, &raw).map(|()| 0)
        }
        VT_GETMODE => {
            let mode = STATE.lock().modes[index];
            let mut raw = [0u8; 8];
            raw[0] = mode.mode;
            raw[1] = mode.waitv;
            raw[2..4].copy_from_slice(&mode.relsig.to_le_bytes());
            raw[4..6].copy_from_slice(&mode.acqsig.to_le_bytes());
            raw[6..8].copy_from_slice(&mode.frsig.to_le_bytes());
            uaccess::write_value(arg, &raw).map(|()| 0)
        }
        VT_SETMODE => match uaccess::read_value::<[u8; 8]>(arg) {
            Ok(raw) => {
                if raw[0] > VT_PROCESS {
                    Err(VfsError::InvalidArgument)
                } else {
                    let owner = crate::sched::current().map_or(0, |task| task.stats().tgid);
                    STATE.lock().modes[index] = VtMode {
                        mode: raw[0],
                        waitv: raw[1],
                        relsig: i16::from_le_bytes([raw[2], raw[3]]),
                        acqsig: i16::from_le_bytes([raw[4], raw[5]]),
                        frsig: i16::from_le_bytes([raw[6], raw[7]]),
                        owner,
                    };
                    Ok(0)
                }
            }
            Err(error) => Err(error),
        },
        VT_RELDISP => {
            let mut state = STATE.lock();
            if arg == VT_ACKACQ {
                Ok(0)
            } else if let Some(target) = state.pending_target.take() {
                if arg != 0 {
                    complete_switch(&mut state, target);
                }
                Ok(0)
            } else {
                Err(VfsError::InvalidArgument)
            }
        }
        VT_ACTIVATE => begin_switch(arg as usize).map(|()| 0),
        VT_WAITACTIVE => {
            if !(1..=VT_COUNT).contains(&(arg as usize)) {
                Err(VfsError::NoSuchDeviceOrAddress)
            } else {
                loop {
                    if active_vt() == arg as usize {
                        break Ok(0);
                    }
                    if crate::signal::pending_for_current() {
                        break Err(VfsError::Interrupted);
                    }
                    crate::ipc::block_briefly();
                }
            }
        }
        VT_DISALLOCATE => {
            let mut state = STATE.lock();
            let target = arg as usize;
            if target != 0 && target != active_vt() && target <= VT_COUNT {
                state.modes[target - 1].mode = VT_AUTO;
                state.display_mode[target - 1] = KD_TEXT;
                state.keyboard_mode[target - 1] = K_UNICODE;
            }
            Ok(0)
        }
        VT_RESIZE => Ok(0),
        VT_LOCKSWITCH => {
            STATE.lock().switch_locked = true;
            Ok(0)
        }
        VT_UNLOCKSWITCH => {
            STATE.lock().switch_locked = false;
            Ok(0)
        }
        _ => return None,
    };
    Some(result)
}

/// Clear a VT's display-server claims when its owner died without cleaning up.
pub fn release_owner(tgid: u64) {
    let mut state = STATE.lock();
    let active = active_vt();
    let mut changed = false;
    for index in 0..VT_COUNT {
        if state.modes[index].mode == VT_PROCESS && state.modes[index].owner == tgid {
            state.modes[index].mode = VT_AUTO;
            state.modes[index].owner = 0;
            state.display_mode[index] = KD_TEXT;
            state.keyboard_mode[index] = K_UNICODE;
            changed = true;
        }
    }
    if changed {
        if state.modes[active - 1].mode == VT_AUTO {
            if let Some(target) = state.pending_target.take() {
                complete_switch(&mut state, target);
            } else {
                publish(&state, active);
            }
        } else {
            publish(&state, active);
        }
        let active = active_vt();
        drop(state);
        console::repaint(active);
    }
}
