/*
 *
 *       src/sound/self_test.rs
 *       ALSA ABI and runtime boot self-tests
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The early pass runs before the scheduler inside `kernel_test_scope`, so it
//! can drive the real ioctl entry points with kernel buffers and validate the
//! user-copy ABI.  The late pass runs on a kernel thread and exercises the
//! period timer, dummy pointer advancement and pause/resume transitions.

use crate::{
    sound::{self, control::ControlSession, hwdep, pcm, rawmidi, timer, uapi},
    vfs::Inode,
};

fn ctl(session: &ControlSession, cmd: u32, pointer: *mut impl Sized) -> bool {
    session.ioctl(cmd as u64, pointer as u64).is_ok()
}

fn pcm_ctl(
    substream: &alloc::sync::Arc<pcm::Substream>,
    cmd: u32,
    pointer: *mut impl Sized,
) -> bool {
    pcm::ioctl(substream, cmd, pointer as u64, false).is_ok()
}

fn hw_params_exact(period: u32, periods: u32) -> uapi::snd_pcm_hw_params {
    let mut params = uapi::snd_pcm_hw_params::zeroed();
    params.masks[uapi::SNDRV_PCM_HW_PARAM_ACCESS as usize]
        .set(uapi::SNDRV_PCM_ACCESS_MMAP_INTERLEAVED);
    params.masks[uapi::SNDRV_PCM_HW_PARAM_FORMAT as usize].set(uapi::SNDRV_PCM_FORMAT_S16_LE);
    params.masks[uapi::SNDRV_PCM_HW_PARAM_SUBFORMAT as usize].set(uapi::SNDRV_PCM_SUBFORMAT_STD);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_CHANNELS - 8) as usize] =
        uapi::snd_interval::exact(2);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_RATE - 8) as usize] =
        uapi::snd_interval::exact(48_000);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE - 8) as usize] =
        uapi::snd_interval::exact(period);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_PERIODS - 8) as usize] =
        uapi::snd_interval::exact(periods);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE - 8) as usize] =
        uapi::snd_interval::exact(period * periods);
    params
}

pub fn run_early() {
    let card = sound::find_card(0).expect("sound: dummy card missing");
    assert_eq!(card.number, 0, "sound: dummy card index");
    assert!(!card.pcms().is_empty(), "sound: card has no PCM");
    assert!(card.controls().len() >= 5, "sound: mixer elements missing");

    let session = ControlSession::new(card.clone());
    let mut card_info = uapi::snd_ctl_card_info::zeroed();
    assert!(ctl(
        &session,
        uapi::SNDRV_CTL_IOCTL_CARD_INFO,
        &mut card_info
    ));
    assert_eq!(&card_info.id[..5], b"Dummy", "sound: card id");
    assert!(card_info.components[0] != 0, "sound: components empty");

    let mut ids = [uapi::snd_ctl_elem_id::zeroed(); 16];
    let mut list = uapi::snd_ctl_elem_list::zeroed();
    list.space = ids.len() as u32;
    list.pids = ids.as_mut_ptr() as u64;
    assert!(
        ctl(&session, uapi::SNDRV_CTL_IOCTL_ELEM_LIST, &mut list),
        "sound: ELEM_LIST failed"
    );
    assert!(list.count >= 5 && list.used >= 5, "sound: element count");
    assert_eq!(ids[0].numid, 1, "sound: numid assignment");

    let mut elem_info = uapi::snd_ctl_elem_info::zeroed();
    elem_info.id = ids[0];
    assert!(
        ctl(&session, uapi::SNDRV_CTL_IOCTL_ELEM_INFO, &mut elem_info),
        "sound: ELEM_INFO failed"
    );
    assert_eq!(
        elem_info.kind,
        uapi::SNDRV_CTL_ELEM_TYPE_INTEGER,
        "sound: first element type"
    );
    assert!(elem_info.count >= 1, "sound: element count field");

    let mut value = uapi::snd_ctl_elem_value::zeroed();
    value.id = ids[0];
    assert!(
        ctl(&session, uapi::SNDRV_CTL_IOCTL_ELEM_READ, &mut value),
        "sound: ELEM_READ failed"
    );
    let mut written = [0i64; 128];
    written[0] = 42;
    written[1] = 43;
    value.set_integer(&written);
    assert!(
        ctl(&session, uapi::SNDRV_CTL_IOCTL_ELEM_WRITE, &mut value),
        "sound: ELEM_WRITE failed"
    );
    let mut readback = uapi::snd_ctl_elem_value::zeroed();
    readback.id = ids[0];
    assert!(
        ctl(&session, uapi::SNDRV_CTL_IOCTL_ELEM_READ, &mut readback),
        "sound: ELEM_READ back failed"
    );
    assert_eq!(readback.integer()[0], 42, "sound: mixer value round-trip");

    let mut pcm_info = uapi::snd_pcm_info::zeroed();
    pcm_info.device = 0;
    pcm_info.stream = uapi::SNDRV_PCM_STREAM_PLAYBACK;
    assert!(
        ctl(&session, uapi::SNDRV_CTL_IOCTL_PCM_INFO, &mut pcm_info),
        "sound: CTL PCM_INFO failed"
    );
    assert_eq!(pcm_info.card, 0, "sound: CTL PCM_INFO card");
    assert_eq!(pcm_info.subdevices_count, 2, "sound: PCM subdevice count");

    let mut next = -1i32;
    assert!(
        ctl(&session, uapi::SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE, &mut next),
        "sound: PCM_NEXT_DEVICE failed"
    );
    assert_eq!(next, 0, "sound: PCM_NEXT_DEVICE value");

    let pcm = card.find_pcm(0).expect("sound: PCM 0 missing");
    let node = pcm::PcmDeviceNode::new(pcm.substreams(pcm::Direction::Playback));
    let first = node.open(0).expect("sound: first PCM open");
    let second = node.open(0).expect("sound: second PCM subdevice open");
    assert!(
        node.open(0).is_err(),
        "sound: third open should fail with EAGAIN"
    );
    drop(first);
    drop(second);
    let playback = pcm
        .first_substream(pcm::Direction::Playback)
        .expect("sound: playback missing");
    let capture = pcm
        .first_substream(pcm::Direction::Capture)
        .expect("sound: capture missing");

    let mut info = uapi::snd_pcm_info::zeroed();
    assert!(pcm_ctl(&playback, uapi::SNDRV_PCM_IOCTL_INFO, &mut info));
    assert_eq!(info.stream, uapi::SNDRV_PCM_STREAM_PLAYBACK);
    assert_eq!(info.subdevices_avail, 2);

    let mut refine = uapi::snd_pcm_hw_params::zeroed();
    assert!(
        pcm_ctl(&playback, uapi::SNDRV_PCM_IOCTL_HW_REFINE, &mut refine),
        "sound: HW_REFINE failed"
    );
    assert!(
        refine
            .mask(uapi::SNDRV_PCM_HW_PARAM_ACCESS)
            .test(uapi::SNDRV_PCM_ACCESS_MMAP_INTERLEAVED),
        "sound: HW_REFINE access"
    );
    assert!(
        !refine.mask(uapi::SNDRV_PCM_HW_PARAM_FORMAT).is_empty(),
        "sound: HW_REFINE formats"
    );

    let mut params = hw_params_exact(256, 4);
    assert!(
        pcm_ctl(&playback, uapi::SNDRV_PCM_IOCTL_HW_PARAMS, &mut params),
        "sound: HW_PARAMS failed"
    );
    assert!(
        params.info & uapi::SNDRV_PCM_INFO_MMAP_VALID != 0,
        "sound: HW_PARAMS info flags"
    );
    assert_eq!(
        params
            .interval(uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE)
            .exact_value(),
        Some(1024),
        "sound: buffer size resolution"
    );
    assert_eq!(
        params
            .interval(uapi::SNDRV_PCM_HW_PARAM_PERIOD_BYTES)
            .exact_value(),
        Some(1024),
        "sound: period bytes (256 frames x 2 ch x 2 bytes)"
    );

    let mut sw = uapi::snd_pcm_sw_params::zeroed();
    sw.avail_min = 128;
    sw.start_threshold = 512;
    sw.stop_threshold = 1024;
    sw.boundary = 2048;
    assert!(
        pcm_ctl(&playback, uapi::SNDRV_PCM_IOCTL_SW_PARAMS, &mut sw),
        "sound: SW_PARAMS failed"
    );
    assert_eq!(sw.boundary, 2048);

    assert!(
        pcm_ctl(
            &playback,
            uapi::SNDRV_PCM_IOCTL_PREPARE,
            core::ptr::null_mut::<u8>()
        ),
        "sound: PREPARE failed"
    );
    let mut status = uapi::snd_pcm_status::zeroed();
    assert!(
        pcm_ctl(&playback, uapi::SNDRV_PCM_IOCTL_STATUS, &mut status),
        "sound: STATUS failed"
    );
    assert_eq!(status.state, uapi::SNDRV_PCM_STATE_PREPARED);
    assert_eq!(status.avail, 1024, "sound: playback avail after prepare");

    let mut channel = uapi::snd_pcm_channel_info::zeroed();
    channel.channel = 1;
    assert!(
        pcm_ctl(&playback, uapi::SNDRV_PCM_IOCTL_CHANNEL_INFO, &mut channel),
        "sound: CHANNEL_INFO failed"
    );
    assert_eq!(channel.first, 16, "sound: channel bit offset");
    assert_eq!(channel.step, 32, "sound: interleaved frame step");

    let mut capture_params = hw_params_exact(256, 4);
    assert!(
        pcm_ctl(
            &capture,
            uapi::SNDRV_PCM_IOCTL_HW_PARAMS,
            &mut capture_params
        ),
        "sound: capture HW_PARAMS failed"
    );
    assert!(
        pcm_ctl(
            &capture,
            uapi::SNDRV_PCM_IOCTL_PREPARE,
            core::ptr::null_mut::<u8>()
        ),
        "sound: capture PREPARE failed"
    );

    // Non-interleaved parameter set on the second playback substream.
    if let Some(noninterleaved) = pcm.substream(pcm::Direction::Playback, 1) {
        let mut ni = hw_params_exact(256, 4);
        ni.masks[uapi::SNDRV_PCM_HW_PARAM_ACCESS as usize] = uapi::snd_mask::zeroed();
        ni.masks[uapi::SNDRV_PCM_HW_PARAM_ACCESS as usize]
            .set(uapi::SNDRV_PCM_ACCESS_RW_NONINTERLEAVED);
        ni.flags = uapi::SNDRV_PCM_HW_PARAMS_EXPORT_BUFFER;
        assert!(
            pcm_ctl(&noninterleaved, uapi::SNDRV_PCM_IOCTL_HW_PARAMS, &mut ni),
            "sound: noninterleaved HW_PARAMS failed"
        );
        assert!(
            ni.info & uapi::SNDRV_PCM_INFO_NONINTERLEAVED != 0,
            "sound: noninterleaved info flag"
        );
        let mut channel_one = uapi::snd_pcm_channel_info::zeroed();
        channel_one.channel = 1;
        assert!(
            pcm_ctl(
                &noninterleaved,
                uapi::SNDRV_PCM_IOCTL_CHANNEL_INFO,
                &mut channel_one
            ),
            "sound: noninterleaved channel info"
        );
        assert_eq!(channel_one.offset, 2048);
        assert_eq!(channel_one.first, 0);
        assert_eq!(channel_one.step, 16);
        assert!(
            pcm_ctl(
                &noninterleaved,
                uapi::SNDRV_PCM_IOCTL_HW_FREE,
                core::ptr::null_mut::<u8>()
            ),
            "sound: noninterleaved HW_FREE"
        );
    }

    // Audio timestamp request/report round trip.
    let mut mode = uapi::SNDRV_PCM_TSTAMP_ENABLE;
    assert!(pcm_ctl(&playback, uapi::SNDRV_PCM_IOCTL_TSTAMP, &mut mode));
    let mut tstamp_type = uapi::SNDRV_PCM_TSTAMP_TYPE_MONOTONIC;
    assert!(pcm_ctl(
        &playback,
        uapi::SNDRV_PCM_IOCTL_TTSTAMP,
        &mut tstamp_type
    ));
    let mut extended = uapi::snd_pcm_status::zeroed();
    extended.audio_tstamp_data = uapi::SNDRV_PCM_AUDIO_TSTAMP_TYPE_DEFAULT as u32;
    assert!(pcm_ctl(
        &playback,
        uapi::SNDRV_PCM_IOCTL_STATUS_EXT,
        &mut extended
    ));
    assert!(
        extended.audio_tstamp_data & (1 << 16) != 0,
        "sound: audio timestamp report missing"
    );
    assert_eq!(
        (extended.audio_tstamp_data >> 17) & 0xF,
        uapi::SNDRV_PCM_AUDIO_TSTAMP_TYPE_DEFAULT as u32
    );

    // hwdep: info, DSP status/load and a driver-private ioctl.
    let hwdep_object = card.hwdeps().into_iter().next().expect("sound: hwdep");
    let hwdep_node = hwdep::HwdepDeviceNode::new(hwdep_object);
    let hwdep_session = hwdep_node
        .open(0)
        .expect("sound: hwdep open")
        .expect("sound: hwdep session");
    let mut hwdep_version = 0i32;
    assert!(
        hwdep_session
            .ioctl(
                uapi::SNDRV_HWDEP_IOCTL_PVERSION as u64,
                &mut hwdep_version as *mut i32 as u64
            )
            .is_ok()
    );
    assert_eq!(hwdep_version, uapi::SNDRV_HWDEP_VERSION as i32);
    let mut hwdep_info = uapi::snd_hwdep_info::zeroed();
    assert!(
        hwdep_session
            .ioctl(
                uapi::SNDRV_HWDEP_IOCTL_INFO as u64,
                &mut hwdep_info as *mut _ as u64
            )
            .is_ok()
    );
    assert_eq!(hwdep_info.iface, uapi::SNDRV_HWDEP_IFACE_HDA);
    let mut dsp_status = uapi::snd_hwdep_dsp_status::zeroed();
    assert!(
        hwdep_session
            .ioctl(
                uapi::SNDRV_HWDEP_IOCTL_DSP_STATUS as u64,
                &mut dsp_status as *mut _ as u64
            )
            .is_ok()
    );
    assert_eq!(dsp_status.num_dsps, 1);
    let dsp_data = b"firmware";
    let mut image = uapi::snd_hwdep_dsp_image {
        index: 0,
        name: [0; 64],
        _pad0: [0; 4],
        image: dsp_data.as_ptr() as u64,
        length: dsp_data.len(),
        driver_data: 0,
    };
    assert!(
        hwdep_session
            .ioctl(
                uapi::SNDRV_HWDEP_IOCTL_DSP_LOAD as u64,
                &mut image as *mut _ as u64
            )
            .is_ok()
    );
    let mut loaded = uapi::snd_hwdep_dsp_status::zeroed();
    assert!(
        hwdep_session
            .ioctl(
                uapi::SNDRV_HWDEP_IOCTL_DSP_STATUS as u64,
                &mut loaded as *mut _ as u64
            )
            .is_ok()
    );
    assert_eq!(loaded.dsp_loaded & 1, 1);
    let mut echo = 0u32;
    assert!(
        hwdep_session
            .ioctl(0x4004_4810, &mut echo as *mut u32 as u64)
            .is_ok()
    );
    assert_eq!(echo, 0x5a5a_5a5a);

    // Jack state is exported as a read-only control.
    let jack = card
        .controls()
        .into_iter()
        .find(|element| element.id().name_str() == b"Headphone Jack")
        .expect("sound: jack control");
    let mut jack_value = uapi::snd_ctl_elem_value::zeroed();
    jack.read(&mut jack_value);
    assert_eq!(jack_value.integer()[0], 1, "sound: jack state");

    // RawMIDI loopback: write to the output stream and read it back.
    let midi = card.rawmidis().into_iter().next().expect("sound: rawmidi");
    let midi_node = rawmidi::RawMidiDeviceNode::new(midi);
    let midi_session = midi_node
        .open(crate::vfs::abi::O_RDWR)
        .expect("sound: rawmidi open")
        .expect("sound: rawmidi session");
    let mut midi_info = uapi::snd_rawmidi_info::zeroed();
    midi_info.stream = uapi::SNDRV_RAWMIDI_STREAM_INPUT;
    assert!(
        midi_session
            .ioctl(
                uapi::SNDRV_RAWMIDI_IOCTL_INFO as u64,
                &mut midi_info as *mut _ as u64
            )
            .is_ok()
    );
    assert_eq!(
        midi_info.flags & uapi::SNDRV_RAWMIDI_INFO_DUPLEX,
        uapi::SNDRV_RAWMIDI_INFO_DUPLEX
    );
    let mut midi_params = uapi::snd_rawmidi_params::zeroed();
    midi_params.stream = uapi::SNDRV_RAWMIDI_STREAM_OUTPUT;
    midi_params.buffer_size = 64;
    midi_params.avail_min = 1;
    assert!(
        midi_session
            .ioctl(
                uapi::SNDRV_RAWMIDI_IOCTL_PARAMS as u64,
                &mut midi_params as *mut _ as u64
            )
            .is_ok()
    );
    let payload = b"MIDI";
    assert_eq!(
        midi_session
            .write_at(0, payload)
            .expect("sound: midi write"),
        4
    );
    let mut midi_read = [0u8; 4];
    assert_eq!(
        midi_session
            .read_at(0, &mut midi_read)
            .expect("sound: midi read"),
        4
    );
    assert_eq!(&midi_read, payload);

    let timers = timer::test_session();
    let mut version = 0i32;
    assert!(
        timers
            .ioctl(
                uapi::SNDRV_TIMER_IOCTL_PVERSION as u64,
                &mut version as *mut i32 as u64
            )
            .is_ok()
    );
    assert_eq!(version, uapi::SNDRV_TIMER_VERSION as i32);

    let mut tid = uapi::snd_timer_id::zeroed();
    assert!(
        timers
            .ioctl(
                uapi::SNDRV_TIMER_IOCTL_NEXT_DEVICE as u64,
                &mut tid as *mut _ as u64
            )
            .is_ok(),
        "sound: timer NEXT_DEVICE failed"
    );
    assert_eq!(tid.dev_class, uapi::SNDRV_TIMER_CLASS_GLOBAL);
    assert_eq!(tid.device, uapi::SNDRV_TIMER_GLOBAL_SYSTEM);

    let mut ginfo = uapi::snd_timer_ginfo::zeroed();
    ginfo.tid = tid;
    assert!(
        timers
            .ioctl(
                uapi::SNDRV_TIMER_IOCTL_GINFO as u64,
                &mut ginfo as *mut _ as u64
            )
            .is_ok(),
        "sound: timer GINFO failed"
    );
    assert!(ginfo.resolution > 0, "sound: timer resolution");

    let mut timer_params = uapi::snd_timer_params::zeroed();
    timer_params.ticks = 1;
    timer_params.queue_size = 32;
    assert!(
        timers
            .ioctl(
                uapi::SNDRV_TIMER_IOCTL_PARAMS as u64,
                &mut timer_params as *mut _ as u64
            )
            .is_ok(),
        "sound: timer PARAMS failed"
    );

    let mut timer_info = uapi::snd_timer_info::zeroed();
    assert!(
        timers
            .ioctl(
                uapi::SNDRV_TIMER_IOCTL_INFO as u64,
                &mut timer_info as *mut _ as u64
            )
            .is_ok(),
        "sound: timer INFO failed"
    );
    assert!(timer_info.resolution > 0, "sound: timer INFO resolution");

    let mut timer_status = uapi::snd_timer_status::zeroed();
    assert!(
        timers
            .ioctl(
                uapi::SNDRV_TIMER_IOCTL_STATUS as u64,
                &mut timer_status as *mut _ as u64
            )
            .is_ok(),
        "sound: timer STATUS failed"
    );

    for path in [
        "/dev/snd/controlC0",
        "/dev/snd/pcmC0D0p",
        "/dev/snd/pcmC0D0c",
        "/dev/snd/timer",
        "/proc/asound/cards",
        "/proc/asound/pcm",
    ] {
        assert!(
            crate::vfs::lookup_abs(path, true).is_ok(),
            "sound: missing {path}"
        );
    }
    let cards =
        crate::vfs::read_file("/proc/asound/cards").expect("sound: /proc/asound/cards unreadable");
    assert!(
        cards.windows(5).any(|window| window == b"Dummy"),
        "sound: card missing from /proc/asound/cards"
    );
}

pub fn run_late() -> &'static str {
    assert!(
        pcm::self_test_late(),
        "sound: PCM streaming self-test failed"
    );
    assert!(
        timer::self_test_late(),
        "sound: timer tick self-test failed"
    );
    "ok"
}

// Keep the module linked in configurations where only one pass is compiled.
#[allow(unused)]
fn _marker() {
    let _ = self::run_early;
}
