/*
 *
 *       modules/hda/hda.c
 *       Intel High Definition Audio controller driver
 *
 *       2026/11/11 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 * Register, verb and stream-format definitions follow the Intel High
 * Definition Audio Specification 1.0a and the Linux HDA register/verb headers.
 * The controller is driven through CORB/RIRB, one output and one input stream,
 * with the ALSA core's coherent ring buffer described by a BDL.
 */

#include "spk/module.h"
#include "spk/sound.h"

#define HDA_GCAP 0x00
#define HDA_GCTL 0x08
#define HDA_GCTL_RESET 0x00000001u
#define HDA_GCTL_FCNTRL 0x00000002u
#define HDA_GCTL_UNSOL 0x00000100u
#define HDA_WAKEEN 0x0c
#define HDA_STATESTS 0x0e
#define HDA_STATESTS_MASK 0x7fffu
#define HDA_INTCTL 0x20
#define HDA_INTSTS 0x24
#define HDA_INT_CTRL 0x40000000u
#define HDA_INT_GLOBAL 0x80000000u
#define HDA_INT_ALL_STREAM 0x3fffffffu
#define HDA_CORBLBASE 0x40
#define HDA_CORBUBASE 0x44
#define HDA_CORBWP 0x48
#define HDA_CORBRP 0x4a
#define HDA_CORBCTL 0x4c
#define HDA_CORBSTS 0x4d
#define HDA_CORBSIZE 0x4e
#define HDA_RIRBLBASE 0x50
#define HDA_RIRBUBASE 0x54
#define HDA_RIRBWP 0x58
#define HDA_RINTCNT 0x5a
#define HDA_RIRBCTL 0x5c
#define HDA_RIRBSTS 0x5d
#define HDA_RIRBSIZE 0x5e
#define HDA_CORBRP_RST 0x8000
#define HDA_RIRBWP_RST 0x8000
#define HDA_CORBCTL_RUN 0x02
#define HDA_CORBSTS_CMEI 0x01u
#define HDA_RIRBCTL_IRQ 0x01u
#define HDA_RIRBCTL_DMA 0x02u
#define HDA_RIRBSTS_IRQ 0x01u
#define HDA_RIRBSTS_OVERRUN 0x04u
#define HDA_RIRBSTS_MASK (HDA_RIRBSTS_IRQ | HDA_RIRBSTS_OVERRUN)
#define HDA_PCIREG_TCSEL 0x44

/* Stream descriptor registers, relative to the stream base. */
#define SD_CTL 0x00
#define SD_STS 0x03
#define SD_LPIB 0x04
#define SD_CBL 0x08
#define SD_LVI 0x0c
#define SD_FORMAT 0x12
#define SD_BDLPL 0x18
#define SD_BDLPU 0x1c

#define SD_CTL_RESET 0x00000001u
#define SD_CTL_RUN 0x00000002u
#define SD_CTL_IOCE 0x00000004u
#define SD_CTL_DIR 0x00080000u
#define SD_CTL_TAG(tag) (((tag) & 0xfu) << 20)
#define SD_INT_MASK 0x1cu

/* Intel ICH6 stream descriptor layout: inputs SDI0-3 at 0x80, outputs
 * SDO0-3 at 0x100.  INTSTS uses bits 0-3 for inputs and 4-7 for outputs. */
#define HDA_PLAY_SD 0x100u
#define HDA_CAP_SD 0x080u
#define HDA_PLAY_BIT 4
#define HDA_CAP_BIT 0
#define HDA_STREAM_TAG 1

#define AC_VERB_GET_PARAMETERS 0x0f00u
#define AC_VERB_GET_CONFIG_DEFAULT 0x0f1cu
#define AC_VERB_SET_STREAM_FORMAT 0x0200u
#define AC_VERB_SET_AMP_GAIN_MUTE 0x0300u
#define AC_VERB_SET_CONNECT_SEL 0x0701u
#define AC_VERB_SET_POWER_STATE 0x0705u
#define AC_VERB_SET_CHANNEL_STREAMID 0x0706u
#define AC_VERB_SET_PIN_WIDGET_CONTROL 0x0707u
#define AC_VERB_SET_EAPD_BTLENABLE 0x070cu

#define AC_PAR_NODE_COUNT 0x04u
#define AC_PAR_FUNCTION_TYPE 0x05u
#define AC_PAR_AUDIO_WIDGET_CAP 0x09u
#define AC_PAR_PIN_CAP 0x0cu
#define AC_PAR_AMP_IN_CAP 0x11u
#define AC_PAR_AMP_OUT_CAP 0x12u

#define AC_WID_AUD_OUT 0x0u
#define AC_WID_AUD_IN 0x1u
#define AC_WID_PIN 0x4u

#define AC_WCAP_IN_AMP 0x00000002u
#define AC_WCAP_OUT_AMP 0x00000004u
#define AC_WCAP_DIGITAL 0x00000200u

#define AC_PINCAP_OUT 0x10u
#define AC_PINCAP_IN 0x20u
#define AC_JACK_PORT_NONE 0x1u

#define AC_PINCTL_OUT_EN 0x40u
#define AC_PINCTL_IN_EN 0x20u

#define AC_AMP_OUT_BOTH 0xb000u
#define AC_AMP_OUT_LEFT 0xa000u
#define AC_AMP_OUT_RIGHT 0x9000u
#define AC_AMP_IN_BOTH 0x7000u
#define AC_AMP_MUTE 0x0080u
#define AC_AMP_GAIN_MASK 0x007fu

struct hda_chip {
    void *mmio;
    unsigned long mmio_len;
    struct spk_pci_device *pci;
    struct snd_card *card;
    struct snd_pcm *pcm;
    struct snd_pcm_substream *playback;
    struct snd_pcm_substream *capture;

    unsigned int codec;
    unsigned int afg;
    unsigned int dac;
    unsigned int adc;
    unsigned int pin_out;
    unsigned int pin_in;

    volatile unsigned int *corb;
    volatile unsigned long long *rirb;
    unsigned int corb_entries;
    unsigned int rirb_entries;
    unsigned short corb_wp;
    unsigned char rirb_wp;
    unsigned char rirb_seen;

    void *bdl[2];
    unsigned long long bdl_addr[2];
    unsigned int bdl_entries[2];
    unsigned int frame_bytes[2];
    unsigned int period_bytes[2];
    unsigned int cbl[2];
    unsigned short format[2];
    unsigned int running[2];

    unsigned char gains[2];
    unsigned char master_vol[2];
    unsigned char master_mute[2];
    unsigned char out_gain_max;
    unsigned char out_gain_default;
    unsigned char out_gain_offset;
    unsigned char out_gain_qdb;
    unsigned char in_gain_max;
    unsigned char in_gain_default;
    unsigned char pin_out_amp;
    unsigned char pin_out_gain;
    unsigned char pin_in_amp;
    unsigned char pin_in_gain;
    unsigned char codec_ready;
    spk_spinlock codec_lock;
    spk_spinlock reg_lock;
    volatile unsigned int period_pending[2];
    volatile unsigned int stream_gen[2];
    volatile unsigned int period_scheduled_gen[2];
    volatile unsigned int shutdown;
};

static struct hda_chip *g_hda;
static unsigned int hda_verb_timeouts;

static inline unsigned int hda_readl(struct hda_chip *chip, unsigned int offset)
{
    return spk_readl((const volatile void *)((unsigned char *)chip->mmio + offset));
}

static inline void hda_writel(struct hda_chip *chip, unsigned int offset,
                              unsigned int value)
{
    spk_writel(value, (volatile void *)((unsigned char *)chip->mmio + offset));
}

static inline unsigned short hda_readw(struct hda_chip *chip, unsigned int offset)
{
    return spk_readw((const volatile void *)((unsigned char *)chip->mmio + offset));
}

static inline void hda_writew(struct hda_chip *chip, unsigned int offset,
                              unsigned short value)
{
    spk_writew(value, (volatile void *)((unsigned char *)chip->mmio + offset));
}

static inline unsigned char hda_readb(struct hda_chip *chip, unsigned int offset)
{
    return spk_readb((const volatile void *)((unsigned char *)chip->mmio + offset));
}

static inline void hda_writeb(struct hda_chip *chip, unsigned int offset,
                              unsigned char value)
{
    spk_writeb(value, (volatile void *)((unsigned char *)chip->mmio + offset));
}

static void hda_ack_rirb_sts(struct hda_chip *chip)
{
    spk_spin_lock(&chip->reg_lock);
    if (hda_readb(chip, HDA_RIRBSTS) & HDA_RIRBSTS_MASK) {
        hda_writeb(chip, HDA_RIRBSTS, HDA_RIRBSTS_MASK);
    }
    spk_spin_unlock(&chip->reg_lock);
}

static unsigned int hda_verb_unlocked(struct hda_chip *chip, unsigned int nid,
                                      unsigned int verb, unsigned int parm)
{
    unsigned int command = ((chip->codec & 0xfu) << 28) | ((nid & 0xffu) << 20) |
                           ((verb & 0xfffu) << 8) | (parm & 0xffffu);
    unsigned int corb_mask = (chip->corb_entries ? chip->corb_entries : 256u) - 1u;
    unsigned int rirb_mask = (chip->rirb_entries ? chip->rirb_entries : 256u) - 1u;
    unsigned int timeout;
    unsigned short next_wp = (unsigned short)((chip->corb_wp + 1u) & corb_mask);
    unsigned int previous;

    /* Clear any prior RIRB/CORB status and sync RIRBWP before issuing CORBWP
     * so RINTCNT throttling never stalls subsequent commands. */
    hda_ack_rirb_sts(chip);
    if (hda_readb(chip, HDA_CORBSTS) & HDA_CORBSTS_CMEI) {
        hda_writeb(chip, HDA_CORBSTS, HDA_CORBSTS_CMEI);
    }
    previous = hda_readw(chip, HDA_RIRBWP) & rirb_mask;
    chip->rirb_wp = (unsigned char)previous;

    chip->corb[next_wp] = command;
    spk_mb();
    hda_writew(chip, HDA_CORBWP, next_wp);
    chip->corb_wp = next_wp;
    for (timeout = 0; timeout < 10000u; timeout++) {
        unsigned int current = hda_readw(chip, HDA_RIRBWP) & rirb_mask;
        while (previous != current) {
            unsigned long long entry;
            unsigned int response;
            unsigned int res_ex;

            previous = (previous + 1u) & rirb_mask;
            spk_mb();
            entry = chip->rirb[previous];
            response = (unsigned int)entry;
            res_ex = (unsigned int)(entry >> 32);
            chip->rirb_wp = (unsigned char)previous;
            /* Ignore unsolicited events and responses from other codec addresses. */
            if ((res_ex & 0x10u) == 0u &&
                (res_ex & 0x0fu) == (chip->codec & 0x0fu)) {
                chip->rirb_seen = 1;
                hda_ack_rirb_sts(chip);
                return response;
            }
        }
        hda_ack_rirb_sts(chip);
        spk_udelay(10);
    }
    hda_ack_rirb_sts(chip);
    hda_verb_timeouts++;
    if (hda_verb_timeouts <= 16u) {
        spk_printk("hda: verb timeout nid=%x verb=%x parm=%x\n",
                   nid, verb, parm);
    }
    return 0xffffffffu;
}

static unsigned int hda_verb(struct hda_chip *chip, unsigned int nid,
                             unsigned int verb, unsigned int parm)
{
    unsigned int response;
    unsigned long flags = spk_spin_lock_irqsave(&chip->codec_lock);
    response = hda_verb_unlocked(chip, nid, verb, parm);
    spk_spin_unlock_irqrestore(&chip->codec_lock, flags);
    return response;
}

static unsigned int hda_ring_size(unsigned char reg_val, unsigned char *sel_out)
{
    if ((reg_val & 0x40u) != 0u || (reg_val & 0x70u) == 0u) {
        *sel_out = 0x02u;
        return 256u;
    }
    if ((reg_val & 0x20u) != 0u) {
        *sel_out = 0x01u;
        return 16u;
    }
    *sel_out = 0x00u;
    return 2u;
}

static void hda_stop_rings(struct hda_chip *chip)
{
    unsigned int timeout;

    hda_writeb(chip, HDA_CORBCTL, 0);
    hda_writeb(chip, HDA_RIRBCTL, 0);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if ((hda_readb(chip, HDA_CORBCTL) & HDA_CORBCTL_RUN) == 0u &&
            (hda_readb(chip, HDA_RIRBCTL) & HDA_RIRBCTL_DMA) == 0u) {
            break;
        }
        spk_udelay(1);
    }
}

static void hda_init_rings(struct hda_chip *chip)
{
    unsigned long long corb_phys = 0;
    unsigned long long rirb_phys = 0;
    unsigned char corb_sz_reg = 0x02u;
    unsigned char rirb_sz_reg = 0x02u;
    unsigned int corb_entries = hda_ring_size(hda_readb(chip, HDA_CORBSIZE), &corb_sz_reg);
    unsigned int rirb_entries = hda_ring_size(hda_readb(chip, HDA_RIRBSIZE), &rirb_sz_reg);
    void *corb = spk_dma_alloc_coherent(corb_entries * 4u, 128u, &corb_phys);
    void *rirb = spk_dma_alloc_coherent(rirb_entries * 8u, 128u, &rirb_phys);
    unsigned int timeout;

    if (corb == 0 || rirb == 0) {
        spk_dma_free_coherent(corb, corb_entries * 4u);
        spk_dma_free_coherent(rirb, rirb_entries * 8u);
        return;
    }
    hda_stop_rings(chip);

    chip->corb = (volatile unsigned int *)corb;
    chip->rirb = (volatile unsigned long long *)rirb;
    chip->corb_entries = corb_entries;
    chip->rirb_entries = rirb_entries;

    /* CORB set up matching Linux snd_hdac_bus_init_cmd_io(): base, size, WP=0, RP reset, run. */
    hda_writel(chip, HDA_CORBLBASE, (unsigned int)corb_phys);
    hda_writel(chip, HDA_CORBUBASE, (unsigned int)(corb_phys >> 32));
    hda_writeb(chip, HDA_CORBSIZE, corb_sz_reg);
    hda_writew(chip, HDA_CORBWP, 0);
    hda_writew(chip, HDA_CORBRP, HDA_CORBRP_RST);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if (hda_readw(chip, HDA_CORBRP) & HDA_CORBRP_RST) {
            break;
        }
        spk_udelay(1);
    }
    hda_writew(chip, HDA_CORBRP, 0);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if ((hda_readw(chip, HDA_CORBRP) & HDA_CORBRP_RST) == 0) {
            break;
        }
        spk_udelay(1);
    }
    hda_writeb(chip, HDA_CORBSTS, HDA_CORBSTS_CMEI);
    hda_writeb(chip, HDA_CORBCTL, HDA_CORBCTL_RUN);

    /* RIRB set up: RINTCNT=1 requires RIRBCTL IRQ bit so RIRBSTS updates and clears RIRB count. */
    hda_writel(chip, HDA_RIRBLBASE, (unsigned int)rirb_phys);
    hda_writel(chip, HDA_RIRBUBASE, (unsigned int)(rirb_phys >> 32));
    hda_writeb(chip, HDA_RIRBSIZE, rirb_sz_reg);
    hda_writew(chip, HDA_RIRBWP, HDA_RIRBWP_RST);
    hda_writeb(chip, HDA_RIRBSTS, HDA_RIRBSTS_MASK);
    hda_writew(chip, HDA_RINTCNT, 1);
    hda_writeb(chip, HDA_RIRBCTL, (unsigned char)(HDA_RIRBCTL_DMA | HDA_RIRBCTL_IRQ));
    chip->corb_wp = 0;
    chip->rirb_wp = (unsigned char)(hda_readw(chip, HDA_RIRBWP) & (rirb_entries - 1u));
    chip->rirb_seen = 0;
}

static void hda_query_amp_caps(struct hda_chip *chip, unsigned int nid,
                               unsigned int param, unsigned char *max_out,
                               unsigned char *def_out,
                               unsigned char *offset_out,
                               unsigned char *qdb_out)
{
    unsigned int amp = hda_verb(chip, nid, AC_VERB_GET_PARAMETERS, param);
    unsigned int steps;
    unsigned int offset;
    unsigned int qdb;

    if (amp == 0xffffffffu || amp == 0u) {
        *max_out = 0x4au;
        *def_out = 0x4au;
        if (offset_out) {
            *offset_out = 0x4au;
        }
        if (qdb_out) {
            *qdb_out = 4u;
        }
        return;
    }
    steps = (amp >> 8) & AC_AMP_GAIN_MASK;
    offset = amp & AC_AMP_GAIN_MASK;
    qdb = ((amp >> 16) & AC_AMP_GAIN_MASK) + 1u;
    if (offset > steps) {
        offset = steps;
    }
    *max_out = (unsigned char)steps;
    *def_out = (unsigned char)(offset > 0u ? offset : steps);
    if (offset_out) {
        *offset_out = (unsigned char)offset;
    }
    if (qdb_out) {
        *qdb_out = (unsigned char)qdb;
    }
}

static unsigned int hda_pct_to_step(struct hda_chip *chip, unsigned char pct)
{
    /* Piece-wise perceptual dB attenuation curve in 0.25 dB units (0%..100%). */
    static const unsigned short attn_qdb[21] = {
        200, 172, 152, 132, 116, 100, 88, 78, 68, 60,
        52,  44,  38,  32,  26,  20,  16, 12,  8,  4,  0,
    };
    unsigned int max_step = chip->out_gain_max ? (unsigned int)chip->out_gain_max : 0x4au;
    unsigned int ref_step = chip->out_gain_offset ? (unsigned int)chip->out_gain_offset : max_step;
    unsigned int qdb_step = chip->out_gain_qdb ? (unsigned int)chip->out_gain_qdb : 4u;
    unsigned int idx;
    unsigned int rem;
    unsigned int attn;
    unsigned int delta_steps;

    if (pct == 0u) {
        return AC_AMP_MUTE;
    }
    if (pct >= 100u) {
        return ref_step;
    }
    if (ref_step == 0u) {
        unsigned int linear = ((unsigned int)pct * max_step + 50u) / 100u;
        return linear == 0u ? AC_AMP_MUTE : linear;
    }
    idx = (unsigned int)pct / 5u;
    rem = (unsigned int)pct % 5u;
    attn = ((unsigned int)attn_qdb[idx] * (5u - rem) +
            (unsigned int)attn_qdb[idx + 1u] * rem) / 5u;
    delta_steps = (attn + qdb_step / 2u) / qdb_step;
    if (delta_steps >= ref_step) {
        return 1u;
    }
    return ref_step - delta_steps;
}

static void hda_apply_master_amp(struct hda_chip *chip)
{
    unsigned int gain_l =
        (chip->master_mute[0] || chip->master_vol[0] == 0u)
            ? AC_AMP_MUTE
            : hda_pct_to_step(chip, chip->master_vol[0]);
    unsigned int gain_r =
        (chip->master_mute[1] || chip->master_vol[1] == 0u)
            ? AC_AMP_MUTE
            : hda_pct_to_step(chip, chip->master_vol[1]);
    chip->gains[0] = (unsigned char)((gain_l & AC_AMP_MUTE) ? 0u : (gain_l & AC_AMP_GAIN_MASK));
    if (gain_l == gain_r) {
        hda_verb(chip, chip->dac, AC_VERB_SET_AMP_GAIN_MUTE,
                 AC_AMP_OUT_BOTH | gain_l);
    } else {
        hda_verb(chip, chip->dac, AC_VERB_SET_AMP_GAIN_MUTE,
                 AC_AMP_OUT_LEFT | gain_l);
        hda_verb(chip, chip->dac, AC_VERB_SET_AMP_GAIN_MUTE,
                 AC_AMP_OUT_RIGHT | gain_r);
    }
    if (chip->pin_out_amp) {
        unsigned int pin_mute = (chip->master_mute[0] && chip->master_mute[1])
                                    ? AC_AMP_MUTE
                                    : (unsigned int)chip->pin_out_gain;
        hda_verb(chip, chip->pin_out, AC_VERB_SET_AMP_GAIN_MUTE,
                 AC_AMP_OUT_BOTH | pin_mute);
    }
}

static int hda_sub_nodes(struct hda_chip *chip, unsigned int nid,
                         unsigned int *start_out, unsigned int *count_out)
{
    unsigned int val = hda_verb(chip, nid, AC_VERB_GET_PARAMETERS, AC_PAR_NODE_COUNT);
    if (val == 0xffffffffu) {
        *start_out = 0u;
        *count_out = 0u;
        return -1;
    }
    *start_out = (val >> 16) & 0xffu;
    *count_out = val & 0xffu;
    return 0;
}

static int hda_discover_codec(struct hda_chip *chip)
{
    unsigned int fg_start = 0;
    unsigned int fg_count = 0;
    unsigned int start = 0;
    unsigned int count = 0;
    unsigned int index;
    unsigned int dac_cap = 0;
    unsigned int adc_cap = 0;
    unsigned int pin_out_cap = 0;
    unsigned int pin_in_cap = 0;
    unsigned int fallback_dac = 0, fallback_dac_cap = 0;
    unsigned int fallback_adc = 0, fallback_adc_cap = 0;
    unsigned int fallback_pin_out = 0, fallback_pin_out_cap = 0;
    unsigned int fallback_pin_in = 0, fallback_pin_in_cap = 0;

    chip->afg = 0;
    chip->dac = 0;
    chip->adc = 0;
    chip->pin_out = 0;
    chip->pin_in = 0;
    if (hda_sub_nodes(chip, 0u, &fg_start, &fg_count) != 0 ||
        fg_start == 0u || fg_count == 0u || fg_start + fg_count > 256u) {
        fg_start = 1u;
        fg_count = 8u;
    }
    for (index = fg_start; index < fg_start + fg_count; index++) {
        unsigned int type = hda_verb(chip, index, AC_VERB_GET_PARAMETERS,
                                     AC_PAR_FUNCTION_TYPE);
        if (type != 0xffffffffu && (type & 0xffu) == 0x01u && chip->afg == 0u) {
            chip->afg = index;
            break;
        }
    }
    if (chip->afg == 0u) {
        return -19;
    }
    hda_verb(chip, chip->afg, AC_VERB_SET_POWER_STATE, 0);
    if (hda_sub_nodes(chip, chip->afg, &start, &count) != 0 ||
        start == 0u || count == 0u || start + count > 256u) {
        start = chip->afg + 1u;
        count = 8u;
    }
    for (index = start; index < start + count; index++) {
        unsigned int cap = hda_verb(chip, index, AC_VERB_GET_PARAMETERS,
                                    AC_PAR_AUDIO_WIDGET_CAP);
        unsigned int type;
        unsigned int is_digital;
        if (cap == 0xffffffffu) {
            continue;
        }
        type = (cap >> 20) & 0xfu;
        is_digital = (cap & AC_WCAP_DIGITAL) ? 1u : 0u;
        if (type == AC_WID_AUD_OUT) {
            if (!is_digital && chip->dac == 0u) {
                chip->dac = index;
                dac_cap = cap;
            } else if (fallback_dac == 0u) {
                fallback_dac = index;
                fallback_dac_cap = cap;
            }
        } else if (type == AC_WID_AUD_IN) {
            if (!is_digital && chip->adc == 0u) {
                chip->adc = index;
                adc_cap = cap;
            } else if (fallback_adc == 0u) {
                fallback_adc = index;
                fallback_adc_cap = cap;
            }
        } else if (type == AC_WID_PIN) {
            unsigned int pincap = hda_verb(chip, index, AC_VERB_GET_PARAMETERS,
                                           AC_PAR_PIN_CAP);
            unsigned int defcfg = hda_verb(chip, index, AC_VERB_GET_CONFIG_DEFAULT, 0);
            unsigned int conn = (defcfg != 0xffffffffu) ? ((defcfg >> 30) & 0x3u) : 0u;
            unsigned int connected = (!is_digital && conn != AC_JACK_PORT_NONE) ? 1u : 0u;
            if (pincap == 0xffffffffu) {
                continue;
            }
            if ((pincap & AC_PINCAP_OUT) != 0u) {
                if (connected && chip->pin_out == 0u) {
                    chip->pin_out = index;
                    pin_out_cap = cap;
                } else if (fallback_pin_out == 0u) {
                    fallback_pin_out = index;
                    fallback_pin_out_cap = cap;
                }
            }
            if ((pincap & AC_PINCAP_IN) != 0u) {
                if (connected && chip->pin_in == 0u) {
                    chip->pin_in = index;
                    pin_in_cap = cap;
                } else if (fallback_pin_in == 0u) {
                    fallback_pin_in = index;
                    fallback_pin_in_cap = cap;
                }
            }
        }
    }
    if (chip->dac == 0u && fallback_dac != 0u) {
        chip->dac = fallback_dac;
        dac_cap = fallback_dac_cap;
    }
    if (chip->adc == 0u && fallback_adc != 0u) {
        chip->adc = fallback_adc;
        adc_cap = fallback_adc_cap;
    }
    if (chip->pin_out == 0u && fallback_pin_out != 0u) {
        chip->pin_out = fallback_pin_out;
        pin_out_cap = fallback_pin_out_cap;
    }
    if (chip->pin_in == 0u && fallback_pin_in != 0u) {
        chip->pin_in = fallback_pin_in;
        pin_in_cap = fallback_pin_in_cap;
    }
    /* QEMU hda-duplex: AFG 1, DAC 2, line-out 3, ADC 4, line-in 5. */
    if (chip->dac == 0) {
        chip->dac = 2;
        dac_cap = hda_verb(chip, chip->dac, AC_VERB_GET_PARAMETERS,
                           AC_PAR_AUDIO_WIDGET_CAP);
    }
    if (chip->pin_out == 0) {
        chip->pin_out = 3;
        pin_out_cap = hda_verb(chip, chip->pin_out, AC_VERB_GET_PARAMETERS,
                               AC_PAR_AUDIO_WIDGET_CAP);
    }
    if (chip->adc == 0) {
        chip->adc = 4;
        adc_cap = hda_verb(chip, chip->adc, AC_VERB_GET_PARAMETERS,
                           AC_PAR_AUDIO_WIDGET_CAP);
    }
    if (chip->pin_in == 0) {
        chip->pin_in = 5;
        pin_in_cap = hda_verb(chip, chip->pin_in, AC_VERB_GET_PARAMETERS,
                              AC_PAR_AUDIO_WIDGET_CAP);
    }
    (void)dac_cap;
    (void)adc_cap;
    hda_query_amp_caps(chip, chip->dac, AC_PAR_AMP_OUT_CAP,
                       &chip->out_gain_max, &chip->out_gain_default,
                       &chip->out_gain_offset, &chip->out_gain_qdb);
    hda_query_amp_caps(chip, chip->adc, AC_PAR_AMP_IN_CAP,
                       &chip->in_gain_max, &chip->in_gain_default, 0, 0);
    chip->pin_out_amp = (pin_out_cap & AC_WCAP_OUT_AMP) ? 1u : 0u;
    if (chip->pin_out_amp) {
        unsigned char dummy_max = 0;
        hda_query_amp_caps(chip, chip->pin_out, AC_PAR_AMP_OUT_CAP,
                           &dummy_max, &chip->pin_out_gain, 0, 0);
    }
    chip->pin_in_amp = (pin_in_cap & AC_WCAP_IN_AMP) ? 1u : 0u;
    if (chip->pin_in_amp) {
        unsigned char dummy_max = 0;
        hda_query_amp_caps(chip, chip->pin_in, AC_PAR_AMP_IN_CAP,
                           &dummy_max, &chip->pin_in_gain, 0, 0);
    }
    chip->gains[0] = chip->out_gain_default;
    chip->gains[1] = chip->in_gain_default;
    chip->master_vol[0] = 100u;
    chip->master_vol[1] = 100u;
    chip->master_mute[0] = 0u;
    chip->master_mute[1] = 0u;
    chip->codec_ready = 1;
    return 0;
}

/* Rate encoding: base 48 kHz or 44.1 kHz, multiplier/divisor fields store
 * value-1, sample size and channels are encoded in the low bits. */
static unsigned short hda_rate_bits(unsigned int rate)
{
    switch (rate) {
    case 8000: return 0x0500;   /* 48/6 */
    case 11025: return 0x4300;  /* 44.1/4 */
    case 12000: return 0x0300;  /* 48/4 */
    case 16000: return 0x0200;  /* 48/3 */
    case 22050: return 0x4100;  /* 44.1/2 */
    case 24000: return 0x0100;  /* 48/2 */
    case 32000: return 0x0a00;  /* 48*2/3 */
    case 44100: return 0x4000;  /* 44.1 */
    case 48000: return 0x0000;  /* 48 */
    case 88200: return 0x4800;  /* 44.1*2 */
    case 96000: return 0x0800;  /* 48*2 */
    case 176400: return 0x5800; /* 44.1*4 */
    case 192000: return 0x1800; /* 48*4 */
    default: return 0xffff;
    }
}

static unsigned short hda_format(unsigned int rate, unsigned int channels,
                                 unsigned int bits)
{
    unsigned short format = hda_rate_bits(rate);
    if (format == 0xffff || channels == 0 || channels > 8) {
        return 0;
    }
    format |= (unsigned short)((channels - 1) & 0xfu);
    switch (bits) {
    case 16: format |= 1u << 4; break;
    case 20: format |= 2u << 4; break;
    case 24: format |= 3u << 4; break;
    case 32: format |= 4u << 4; break;
    default: return 0;
    }
    return format;
}

static void hda_program_converter(struct hda_chip *chip, unsigned int nid,
                                  unsigned int tag)
{
    hda_verb(chip, nid, AC_VERB_SET_POWER_STATE, 0);
    hda_verb(chip, nid, AC_VERB_SET_CHANNEL_STREAMID,
             ((tag & 0xfu) << 4) | 0u);
}

static int hda_configure_codec(struct hda_chip *chip, int capture,
                               unsigned short format)
{
    unsigned int nid = capture ? chip->adc : chip->dac;
    unsigned int pin = capture ? chip->pin_in : chip->pin_out;

    hda_verb(chip, chip->afg, AC_VERB_SET_POWER_STATE, 0);
    if (!capture) {
        hda_verb(chip, nid, AC_VERB_SET_STREAM_FORMAT, format);
        hda_program_converter(chip, nid, HDA_STREAM_TAG);
        hda_verb(chip, pin, AC_VERB_SET_PIN_WIDGET_CONTROL, AC_PINCTL_OUT_EN);
        hda_apply_master_amp(chip);
        hda_verb(chip, pin, AC_VERB_SET_EAPD_BTLENABLE, 0x02);
        hda_verb(chip, pin, AC_VERB_SET_POWER_STATE, 0);
    } else {
        hda_verb(chip, nid, AC_VERB_SET_STREAM_FORMAT, format);
        hda_program_converter(chip, nid, HDA_STREAM_TAG);
        hda_verb(chip, nid, AC_VERB_SET_AMP_GAIN_MUTE,
                 AC_AMP_IN_BOTH | chip->gains[1]);
        hda_verb(chip, pin, AC_VERB_SET_PIN_WIDGET_CONTROL, AC_PINCTL_IN_EN);
        if (chip->pin_in_amp) {
            hda_verb(chip, pin, AC_VERB_SET_AMP_GAIN_MUTE,
                     AC_AMP_IN_BOTH | chip->pin_in_gain);
        }
        hda_verb(chip, pin, AC_VERB_SET_POWER_STATE, 0);
    }
    return 0;
}

static unsigned int hda_stream_offset(int capture)
{
    return capture ? HDA_CAP_SD : HDA_PLAY_SD;
}

static unsigned int hda_int_bit(int capture)
{
    return 1u << (capture ? HDA_CAP_BIT : HDA_PLAY_BIT);
}

static void hda_stream_stop(struct hda_chip *chip, int capture);
static void hda_stream_reset(struct hda_chip *chip, int capture);
static void hda_stream_setup(struct hda_chip *chip, int capture);

static int hda_setup_bdl(struct hda_chip *chip, int capture)
{
    struct snd_pcm_substream *substream = capture ? chip->capture : chip->playback;
    unsigned int offset = hda_stream_offset(capture);
    unsigned long long buffer_addr;
    unsigned int buffer_bytes;
    unsigned int period_bytes;
    unsigned int entries;
    unsigned int index;
    unsigned long long bdl_phys = 0;
    void *bdl;

    if (substream == 0) {
        return -22;
    }
    buffer_bytes = (unsigned int)(snd_pcm_get_buffer_size(substream) *
                                  (unsigned long)snd_pcm_get_channels(substream) *
                                  (snd_pcm_get_format(substream) == 10 ? 4u : 2u));
    period_bytes = (unsigned int)(snd_pcm_get_period_size(substream) *
                                  (unsigned long)snd_pcm_get_channels(substream) *
                                  (snd_pcm_get_format(substream) == 10 ? 4u : 2u));
    if (buffer_bytes == 0 || period_bytes == 0 ||
        (buffer_bytes % 128u) != 0 || (period_bytes % 128u) != 0) {
        return -22;
    }
    entries = buffer_bytes / period_bytes;
    if (entries == 0 || entries > 256u) {
        return -22;
    }
    bdl = spk_dma_alloc_coherent(entries * 16u, 128u, &bdl_phys);
    if (bdl == 0) {
        return -12;
    }
    buffer_addr = snd_pcm_get_dma_addr(substream);
    for (index = 0; index < entries; index++) {
        volatile unsigned int *entry = (volatile unsigned int *)bdl + index * 4u;
        unsigned long long address = buffer_addr + (unsigned long long)index * period_bytes;
        entry[0] = (unsigned int)address;
        entry[1] = (unsigned int)(address >> 32);
        entry[2] = period_bytes;
        entry[3] = 0x01u; /* IOC */
    }
    spk_dma_free_coherent(chip->bdl[capture], chip->bdl_entries[capture] * 16u);
    chip->bdl[capture] = bdl;
    chip->bdl_addr[capture] = bdl_phys;
    chip->bdl_entries[capture] = entries;
    chip->frame_bytes[capture] =
        (unsigned int)snd_pcm_get_channels(substream) *
        (snd_pcm_get_format(substream) == 10 ? 4u : 2u);
    chip->period_bytes[capture] = period_bytes;
    chip->cbl[capture] = buffer_bytes;
    chip->format[capture] = hda_format(
        snd_pcm_get_rate(substream), snd_pcm_get_channels(substream),
        snd_pcm_get_format(substream) == 10 ? 32u : 16u);

    (void)offset;
    (void)bdl_phys;
    hda_stream_reset(chip, capture);
    hda_stream_setup(chip, capture);
    return 0;
}

static void hda_stream_reset(struct hda_chip *chip, int capture)
{
    unsigned int offset = hda_stream_offset(capture);
    unsigned int timeout;

    hda_writel(chip, offset + SD_CTL, SD_CTL_RESET);
    for (timeout = 0; timeout < 100u; timeout++) {
        unsigned int ctl = hda_readl(chip, offset + SD_CTL);
        if ((ctl & SD_CTL_RESET) != 0) {
            break;
        }
        spk_udelay(10);
    }
    hda_writel(chip, offset + SD_CTL,
               hda_readl(chip, offset + SD_CTL) & ~SD_CTL_RESET);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if ((hda_readl(chip, offset + SD_CTL) & SD_CTL_RESET) == 0) {
            break;
        }
        spk_udelay(10);
    }
}

/*
 * Program a freshly reset stream descriptor, mirroring Linux
 * snd_hdac_stream_setup(): the stream tag (SD_CTL bits 23:20) must be latched
 * before SD_CTL.RUN goes 0->1, because the controller (and QEMU's
 * intel_hda_set_st_ctl) samples the tag on the RUN edge to route DMA to the
 * codec converter programmed with AC_VERB_SET_CHANNEL_STREAMID.
 */
static void hda_stream_setup(struct hda_chip *chip, int capture)
{
    unsigned int offset = hda_stream_offset(capture);
    unsigned char ctl2;

    hda_writeb(chip, offset + SD_STS, SD_INT_MASK);
    ctl2 = hda_readb(chip, offset + SD_CTL + 2u);
    ctl2 = (unsigned char)((ctl2 & ~0xf8u) |
                           ((SD_CTL_TAG(HDA_STREAM_TAG) >> 16) & 0xf0u) |
                           (capture ? ((SD_CTL_DIR >> 16) & 0x08u) : 0u));
    hda_writeb(chip, offset + SD_CTL + 2u, ctl2);
    if (chip->bdl_entries[capture] == 0) {
        return;
    }
    hda_writel(chip, offset + SD_CBL, chip->cbl[capture]);
    hda_writew(chip, offset + SD_LVI,
               (unsigned short)(chip->bdl_entries[capture] - 1));
    hda_writew(chip, offset + SD_FORMAT, chip->format[capture]);
    hda_writel(chip, offset + SD_BDLPL, (unsigned int)chip->bdl_addr[capture]);
    hda_writel(chip, offset + SD_BDLPU,
               (unsigned int)(chip->bdl_addr[capture] >> 32));
}

static void hda_stream_stop(struct hda_chip *chip, int capture)
{
    unsigned int offset = hda_stream_offset(capture);
    unsigned long flags;
    unsigned int timeout;
    unsigned char ctl;

    flags = spk_spin_lock_irqsave(&chip->reg_lock);
    chip->running[capture] = 0;
    __atomic_add_fetch(&chip->stream_gen[capture], 1u, __ATOMIC_ACQ_REL);
    __atomic_store_n(&chip->period_pending[capture], 0u, __ATOMIC_RELEASE);
    ctl = hda_readb(chip, offset + SD_CTL);
    if ((ctl & (unsigned char)SD_CTL_RUN) != 0) {
        hda_writeb(chip, offset + SD_CTL,
                   ctl & (unsigned char)~(SD_CTL_RUN | SD_CTL_IOCE));
        for (timeout = 0; timeout < 50u; timeout++) {
            if ((hda_readb(chip, offset + SD_CTL) & (unsigned char)SD_CTL_RUN) == 0) {
                break;
            }
            spk_udelay(1);
        }
    } else {
        hda_writeb(chip, offset + SD_CTL, ctl & (unsigned char)~SD_CTL_IOCE);
    }
    hda_writeb(chip, offset + SD_STS, SD_INT_MASK);
    hda_writel(chip, HDA_INTCTL,
               hda_readl(chip, HDA_INTCTL) & ~hda_int_bit(capture));
    spk_spin_unlock_irqrestore(&chip->reg_lock, flags);
}

static void hda_period_dispatch(struct hda_chip *chip, int capture)
{
    unsigned int gen = __atomic_load_n(&chip->period_scheduled_gen[capture], __ATOMIC_ACQUIRE);
    struct snd_pcm_substream *sub = capture ? chip->capture : chip->playback;
    __atomic_store_n(&chip->period_pending[capture], 0u, __ATOMIC_RELEASE);
    if (chip->shutdown || !chip->running[capture] || sub == 0) {
        return;
    }
    if (gen != __atomic_load_n(&chip->stream_gen[capture], __ATOMIC_ACQUIRE)) {
        return;
    }
    snd_pcm_period_elapsed(sub);
}

static void hda_period_play(void *data)
{
    hda_period_dispatch((struct hda_chip *)data, 0);
}

static void hda_period_capture(void *data)
{
    hda_period_dispatch((struct hda_chip *)data, 1);
}

static void hda_irq(int irq, void *data)
{
    struct hda_chip *chip = (struct hda_chip *)data;
    int schedule_mask = 0;
    int repeat;
    (void)irq;

    for (repeat = 0; repeat < 8; repeat++) {
        unsigned long flags = spk_spin_lock_irqsave(&chip->reg_lock);
        unsigned int status = hda_readl(chip, HDA_INTSTS);
        unsigned char rirb_sts;
        int active = 0;

        if (chip->shutdown || status == 0 || status == 0xffffffffu) {
            spk_spin_unlock_irqrestore(&chip->reg_lock, flags);
            break;
        }
        if (status & hda_int_bit(0)) {
            unsigned char stream = hda_readb(chip, HDA_PLAY_SD + SD_STS);
            hda_writeb(chip, HDA_PLAY_SD + SD_STS, SD_INT_MASK);
            active = 1;
            if ((stream & 0x04u) != 0 && chip->playback != 0 && chip->running[0]) {
                if (__atomic_exchange_n(&chip->period_pending[0], 1u, __ATOMIC_ACQ_REL) == 0u) {
                    __atomic_store_n(&chip->period_scheduled_gen[0],
                                     __atomic_load_n(&chip->stream_gen[0], __ATOMIC_ACQUIRE),
                                     __ATOMIC_RELEASE);
                    schedule_mask |= 1;
                }
            }
        }
        if (status & hda_int_bit(1)) {
            unsigned char stream = hda_readb(chip, HDA_CAP_SD + SD_STS);
            hda_writeb(chip, HDA_CAP_SD + SD_STS, SD_INT_MASK);
            active = 1;
            if ((stream & 0x04u) != 0 && chip->capture != 0 && chip->running[1]) {
                if (__atomic_exchange_n(&chip->period_pending[1], 1u, __ATOMIC_ACQ_REL) == 0u) {
                    __atomic_store_n(&chip->period_scheduled_gen[1],
                                     __atomic_load_n(&chip->stream_gen[1], __ATOMIC_ACQUIRE),
                                     __ATOMIC_RELEASE);
                    schedule_mask |= 2;
                }
            }
        }
        rirb_sts = hda_readb(chip, HDA_RIRBSTS);
        if (rirb_sts & 0x05u) {
            hda_writeb(chip, HDA_RIRBSTS, 0x05u);
            active = 1;
        }
        spk_spin_unlock_irqrestore(&chip->reg_lock, flags);
        if (!active) {
            break;
        }
    }

    if ((schedule_mask & 1) != 0) {
        if (spk_work_schedule(hda_period_play, chip) != 0) {
            __atomic_store_n(&chip->period_pending[0], 0u, __ATOMIC_RELEASE);
        }
    }
    if ((schedule_mask & 2) != 0) {
        if (spk_work_schedule(hda_period_capture, chip) != 0) {
            __atomic_store_n(&chip->period_pending[1], 0u, __ATOMIC_RELEASE);
        }
    }
}

static int hda_open_common(struct snd_pcm_substream *substream, int capture)
{
    struct hda_chip *chip = g_hda;
    unsigned long long formats =
        (1ull << SNDRV_PCM_FORMAT_S16_LE) | (1ull << SNDRV_PCM_FORMAT_S32_LE);
    if (chip == 0) {
        return -19;
    }
    if (capture) {
        chip->capture = substream;
    } else {
        chip->playback = substream;
    }
    snd_pcm_set_formats(substream, formats);
    snd_pcm_hw_constraint_minmax(substream, SNDRV_PCM_HW_PARAM_RATE,
                                 8000, 192000);
    snd_pcm_hw_constraint_minmax(substream, SNDRV_PCM_HW_PARAM_CHANNELS, 1, 2);
    snd_pcm_hw_constraint_minmax(substream, SNDRV_PCM_HW_PARAM_PERIODS, 2, 32);
    snd_pcm_hw_constraint_minmax(substream, SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
                                 1024, 1024 * 1024);
    snd_pcm_hw_constraint_step(substream, SNDRV_PCM_HW_PARAM_PERIOD_BYTES, 128);
    snd_pcm_hw_constraint_step(substream, SNDRV_PCM_HW_PARAM_BUFFER_BYTES, 128);
    return 0;
}

static int hda_playback_open(struct snd_pcm_substream *substream)
{
    return hda_open_common(substream, 0);
}

static int hda_capture_open(struct snd_pcm_substream *substream)
{
    return hda_open_common(substream, 1);
}

static int hda_close_common(struct snd_pcm_substream *substream, int capture)
{
    struct hda_chip *chip = g_hda;
    if (chip == 0) {
        return 0;
    }
    hda_stream_stop(chip, capture);
    if (capture) {
        chip->capture = 0;
    } else {
        chip->playback = 0;
    }
    (void)substream;
    return 0;
}

static int hda_playback_close(struct snd_pcm_substream *substream)
{
    return hda_close_common(substream, 0);
}

static int hda_capture_close(struct snd_pcm_substream *substream)
{
    return hda_close_common(substream, 1);
}

static int hda_hw_params(struct snd_pcm_substream *substream,
                         struct snd_pcm_hw_params *params)
{
    struct hda_chip *chip = g_hda;
    int capture;
    unsigned int bits;
    unsigned short format;
    (void)params;

    if (chip == 0) {
        return -19;
    }
    capture = (substream == chip->capture);
    bits = snd_pcm_get_format(substream) == SNDRV_PCM_FORMAT_S32_LE ? 32u : 16u;
    format = hda_format(snd_pcm_get_rate(substream),
                        snd_pcm_get_channels(substream), bits);
    if (format == 0) {
        return -22;
    }
    if (hda_configure_codec(chip, capture, format) != 0) {
        return -5;
    }
    return hda_setup_bdl(chip, capture);
}

static int hda_hw_free(struct snd_pcm_substream *substream)
{
    struct hda_chip *chip = g_hda;
    int capture;
    if (chip == 0) {
        return 0;
    }
    capture = (substream == chip->capture);
    hda_stream_stop(chip, capture);
    spk_dma_free_coherent(chip->bdl[capture], chip->bdl_entries[capture] * 16u);
    chip->bdl[capture] = 0;
    chip->bdl_entries[capture] = 0;
    return 0;
}

static int hda_prepare(struct snd_pcm_substream *substream)
{
    struct hda_chip *chip = g_hda;
    int capture;
    unsigned int offset;
    if (chip == 0) {
        return -19;
    }
    capture = (substream == chip->capture);
    offset = hda_stream_offset(capture);
    __atomic_add_fetch(&chip->stream_gen[capture], 1u, __ATOMIC_ACQ_REL);
    __atomic_store_n(&chip->period_pending[capture], 0u, __ATOMIC_RELEASE);
    (void)offset;
    hda_stream_reset(chip, capture);
    hda_stream_setup(chip, capture);
    return 0;
}

static int hda_trigger(struct snd_pcm_substream *substream, int cmd)
{
    struct hda_chip *chip = g_hda;
    int capture;
    unsigned int offset;
    unsigned int value;
    if (chip == 0) {
        return -19;
    }
    capture = (substream == chip->capture);
    offset = hda_stream_offset(capture);
    switch (cmd) {
    case SNDRV_PCM_TRIGGER_START:
    case SNDRV_PCM_TRIGGER_RESUME:
    case SNDRV_PCM_TRIGGER_PAUSE_RELEASE: {
        unsigned long flags = spk_spin_lock_irqsave(&chip->reg_lock);
        __atomic_store_n(&chip->period_pending[capture], 0u, __ATOMIC_RELEASE);
        hda_writeb(chip, offset + SD_STS, SD_INT_MASK);
        chip->running[capture] = 1;
        hda_writel(chip, HDA_INTCTL,
                   hda_readl(chip, HDA_INTCTL) | HDA_INT_GLOBAL |
                   hda_int_bit(capture));
        /*
         * Re-assert the stream tag first (it must be valid on the RUN edge),
         * then set RUN|IOCE with a byte write so SD_STS (+0x03, W1C) is never
         * touched: Linux snd_hdac_stream_start() ->
         * snd_hdac_stream_updateb(SD_CTL, 0, SD_CTL_DMA_START | SD_INT_MASK).
         */
        value = hda_readb(chip, offset + SD_CTL + 2u);
        value = (value & ~0xf8u) | ((SD_CTL_TAG(HDA_STREAM_TAG) >> 16) & 0xf0u) |
                (capture ? ((SD_CTL_DIR >> 16) & 0x08u) : 0u);
        hda_writeb(chip, offset + SD_CTL + 2u, (unsigned char)value);
        value = hda_readb(chip, offset + SD_CTL);
        hda_writeb(chip, offset + SD_CTL,
                   (unsigned char)(value | SD_CTL_RUN | SD_CTL_IOCE));
        spk_spin_unlock_irqrestore(&chip->reg_lock, flags);
        break;
    }
    case SNDRV_PCM_TRIGGER_STOP:
    case SNDRV_PCM_TRIGGER_SUSPEND:
    case SNDRV_PCM_TRIGGER_PAUSE_PUSH:
        chip->running[capture] = 0;
        hda_stream_stop(chip, capture);
        break;
    default:
        return -22;
    }
    return 0;
}

static snd_pcm_uframes_t hda_pointer(struct snd_pcm_substream *substream)
{
    struct hda_chip *chip = g_hda;
    int capture;
    unsigned int offset;
    unsigned int lpib;
    if (chip == 0) {
        return 0;
    }
    capture = (substream == chip->capture);
    offset = hda_stream_offset(capture);
    lpib = hda_readl(chip, offset + SD_LPIB);
    if (chip->frame_bytes[capture] == 0) {
        return 0;
    }
    if (chip->cbl[capture] != 0) {
        lpib %= chip->cbl[capture];
    }
    return lpib / chip->frame_bytes[capture];
}

static struct snd_pcm_ops hda_playback_ops = {
    .open = hda_playback_open,
    .close = hda_playback_close,
    .ioctl = 0,
    .hw_params = hda_hw_params,
    .hw_free = hda_hw_free,
    .prepare = hda_prepare,
    .trigger = hda_trigger,
    .pointer = hda_pointer,
};

static struct snd_pcm_ops hda_capture_ops = {
    .open = hda_capture_open,
    .close = hda_capture_close,
    .ioctl = 0,
    .hw_params = hda_hw_params,
    .hw_free = hda_hw_free,
    .prepare = hda_prepare,
    .trigger = hda_trigger,
    .pointer = hda_pointer,
};

static int hda_volume_info(struct snd_kcontrol *kcontrol,
                           struct snd_ctl_elem_info *info)
{
    (void)kcontrol;
    info->type = SNDRV_CTL_ELEM_TYPE_INTEGER;
    info->count = 2;
    info->value.integer.min = 0;
    info->value.integer.max = 100;
    info->value.integer.step = 1;
    return 0;
}

static int hda_volume_get(struct snd_kcontrol *kcontrol,
                          struct snd_ctl_elem_value *value)
{
    struct hda_chip *chip = (struct hda_chip *)kcontrol->private_data;
    value->value.integer.value[0] = (long)chip->master_vol[0];
    value->value.integer.value[1] = (long)chip->master_vol[1];
    return 0;
}

static int hda_volume_put(struct snd_kcontrol *kcontrol,
                          struct snd_ctl_elem_value *value)
{
    struct hda_chip *chip = (struct hda_chip *)kcontrol->private_data;
    long left = value->value.integer.value[0];
    long right = value->value.integer.value[1];
    unsigned char changed;
    if (left < 0) left = 0;
    if (left > 100) left = 100;
    if (right < 0) right = 0;
    if (right > 100) right = 100;
    changed = (chip->master_vol[0] != (unsigned char)left) ||
              (chip->master_vol[1] != (unsigned char)right);
    if (!changed) {
        return 0;
    }
    chip->master_vol[0] = (unsigned char)left;
    chip->master_vol[1] = (unsigned char)right;
    hda_apply_master_amp(chip);
    return 1;
}

static int hda_switch_info(struct snd_kcontrol *kcontrol,
                           struct snd_ctl_elem_info *info)
{
    (void)kcontrol;
    info->type = SNDRV_CTL_ELEM_TYPE_BOOLEAN;
    info->count = 2;
    info->value.integer.min = 0;
    info->value.integer.max = 1;
    info->value.integer.step = 1;
    return 0;
}

static int hda_switch_get(struct snd_kcontrol *kcontrol,
                          struct snd_ctl_elem_value *value)
{
    struct hda_chip *chip = (struct hda_chip *)kcontrol->private_data;
    value->value.integer.value[0] = chip->master_mute[0] ? 0L : 1L;
    value->value.integer.value[1] = chip->master_mute[1] ? 0L : 1L;
    return 0;
}

static int hda_switch_put(struct snd_kcontrol *kcontrol,
                          struct snd_ctl_elem_value *value)
{
    struct hda_chip *chip = (struct hda_chip *)kcontrol->private_data;
    unsigned char m0 = value->value.integer.value[0] ? 0u : 1u;
    unsigned char m1 = value->value.integer.value[1] ? 0u : 1u;
    unsigned char changed = (chip->master_mute[0] != m0) || (chip->master_mute[1] != m1);
    if (!changed) {
        return 0;
    }
    chip->master_mute[0] = m0;
    chip->master_mute[1] = m1;
    hda_apply_master_amp(chip);
    return 1;
}

static struct snd_kcontrol_new hda_master_volume = {
    .iface = SNDRV_CTL_ELEM_IFACE_MIXER,
    .name = "Master Playback Volume",
    .access = SNDRV_CTL_ELEM_ACCESS_READWRITE,
    .count = 2,
    .info = hda_volume_info,
    .get = hda_volume_get,
    .put = hda_volume_put,
    .private_data = 0,
};

static struct snd_kcontrol_new hda_master_switch = {
    .iface = SNDRV_CTL_ELEM_IFACE_MIXER,
    .name = "Master Playback Switch",
    .access = SNDRV_CTL_ELEM_ACCESS_READWRITE,
    .count = 2,
    .info = hda_switch_info,
    .get = hda_switch_get,
    .put = hda_switch_put,
    .private_data = 0,
};

static int hda_probe(struct spk_pci_device *device)
{
    struct hda_chip *chip;
    unsigned long long phys;
    unsigned long len;
    unsigned int gctl;
    unsigned int statests = 0;
    unsigned int timeout;
    int result;

    if (g_hda != 0) {
        return -16;
    }
    len = spk_pci_resource_len(device, 0);
    phys = spk_pci_resource_start(device, 0);
    if (len < 0x2000u || phys == 0) {
        return -19;
    }
    if (spk_pci_enable_device(device) != 0) {
        return -5;
    }
    spk_pci_set_master(device);
    /* Mirror Linux azx_init_pci(): clear TCSEL bits 0..2 on Intel controllers
     * so isochronous traffic maps cleanly to TC0 on real chipsets. */
    if ((spk_pci_read_config_word(device, 0x00) & 0xffffu) == 0x8086u) {
        int tcsel = spk_pci_read_config_byte(device, HDA_PCIREG_TCSEL);
        if (tcsel >= 0) {
            (void)spk_pci_write_config_byte(device, HDA_PCIREG_TCSEL,
                                            (unsigned int)(tcsel & ~0x07));
        }
    }
    chip = (struct hda_chip *)spk_kzalloc(sizeof(*chip));
    if (chip == 0) {
        return -12;
    }
    chip->pci = device;
    chip->mmio_len = len;
    chip->mmio = spk_ioremap(phys, len);
    if (chip->mmio == 0) {
        spk_kfree(chip);
        return -12;
    }
    chip->out_gain_max = 0x4au;
    chip->out_gain_default = 0x4au;
    chip->in_gain_max = 0x4au;
    chip->in_gain_default = 0x4au;
    chip->gains[0] = chip->out_gain_default;
    chip->gains[1] = chip->in_gain_default;

    /* Reset sequence matching Linux snd_hdac_bus_reset_link(): clear STATESTS
     * only if not already in reset, hold reset low >= 521 us, then bring link
     * up and wait for codecs to assert STATESTS. */
    gctl = hda_readl(chip, HDA_GCTL);
    hda_writel(chip, HDA_INTCTL, 0);
    hda_writew(chip, HDA_WAKEEN, 0);
    if (gctl & HDA_GCTL_RESET) {
        hda_writew(chip, HDA_STATESTS, HDA_STATESTS_MASK);
    }
    hda_writel(chip, HDA_GCTL, gctl & ~HDA_GCTL_RESET);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if ((hda_readl(chip, HDA_GCTL) & HDA_GCTL_RESET) == 0) {
            break;
        }
        spk_udelay(100);
    }
    spk_udelay(1000);
    hda_writel(chip, HDA_GCTL, HDA_GCTL_RESET);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if ((hda_readl(chip, HDA_GCTL) & HDA_GCTL_RESET) != 0) {
            break;
        }
        spk_udelay(100);
    }
    spk_udelay(1000);
    for (timeout = 0; timeout < 200u; timeout++) {
        statests = hda_readw(chip, HDA_STATESTS) & HDA_STATESTS_MASK;
        if (statests != 0) {
            break;
        }
        spk_udelay(1000);
    }
    if (statests == 0) {
        result = -19;
        goto fail_mmio;
    }
    hda_writew(chip, HDA_STATESTS, HDA_STATESTS_MASK);
    hda_writeb(chip, HDA_RIRBSTS, HDA_RIRBSTS_MASK);
    hda_writeb(chip, HDA_CORBSTS, HDA_CORBSTS_CMEI);
    hda_writel(chip, HDA_INTSTS, HDA_INT_GLOBAL | HDA_INT_CTRL | HDA_INT_ALL_STREAM);

    hda_init_rings(chip);
    if (chip->corb == 0 || chip->rirb == 0) {
        result = -12;
        goto fail_mmio;
    }
    result = -19;
    for (chip->codec = 0; chip->codec < 15u; chip->codec++) {
        if ((statests & (1u << chip->codec)) != 0u &&
            hda_discover_codec(chip) == 0) {
            result = 0;
            break;
        }
    }
    if (result != 0) {
        goto fail_rings;
    }
    if (spk_pci_enable_msi(device, hda_irq, chip, 0, 0) != 0) {
        if (spk_request_irq(device, hda_irq, chip) != 0) {
            result = -19;
            goto fail_rings;
        }
    }
    if (snd_card_new(-1, "HDA", "HDA", "Intel HDA",
                     "Intel High Definition Audio", &chip->card) != 0) {
        result = -12;
        goto fail_irq;
    }
    if (snd_pcm_new(chip->card, "HDA PCM", 0, 1, 1, &chip->pcm) != 0) {
        result = -12;
        goto fail_card;
    }
    if (snd_pcm_set_ops(chip->pcm, SNDRV_PCM_STREAM_PLAYBACK,
                        &hda_playback_ops) != 0 ||
        snd_pcm_set_ops(chip->pcm, SNDRV_PCM_STREAM_CAPTURE,
                        &hda_capture_ops) != 0) {
        result = -5;
        goto fail_card;
    }
    hda_master_volume.private_data = chip;
    hda_master_switch.private_data = chip;
    if (snd_ctl_add(chip->card,
                    snd_ctl_new1(&hda_master_volume, chip)) != 0 ||
        snd_ctl_add(chip->card,
                    snd_ctl_new1(&hda_master_switch, chip)) != 0) {
        result = -12;
        goto fail_card;
    }
    g_hda = chip;
    if (snd_card_register(chip->card) != 0) {
        g_hda = 0;
        result = -5;
        goto fail_card;
    }
    spk_pr_info("hda: Intel High Definition Audio attached\n");
    return 0;

fail_card:
    snd_card_free(chip->card);
fail_irq:
    spk_free_irq(device);
fail_rings:
    hda_stop_rings(chip);
fail_mmio:
    spk_dma_free_coherent((void *)chip->corb,
                          (chip->corb_entries ? chip->corb_entries : 256u) * 4u);
    spk_dma_free_coherent((void *)chip->rirb,
                          (chip->rirb_entries ? chip->rirb_entries : 256u) * 8u);
    spk_iounmap(chip->mmio, chip->mmio_len);
    spk_kfree(chip);
    return result;
}

static void hda_remove(struct spk_pci_device *device)
{
    struct hda_chip *chip = g_hda;
    (void)device;
    if (chip == 0) {
        return;
    }
    chip->shutdown = 1;
    hda_writel(chip, HDA_INTCTL, 0);
    hda_writew(chip, HDA_WAKEEN, 0);
    hda_stream_stop(chip, 0);
    hda_stream_stop(chip, 1);
    hda_stop_rings(chip);
    g_hda = 0;
    snd_card_free(chip->card);
    spk_free_irq(chip->pci);
    spk_msleep(20);
    spk_dma_free_coherent(chip->bdl[0], chip->bdl_entries[0] * 16u);
    spk_dma_free_coherent(chip->bdl[1], chip->bdl_entries[1] * 16u);
    spk_dma_free_coherent((void *)chip->corb,
                          (chip->corb_entries ? chip->corb_entries : 256u) * 4u);
    spk_dma_free_coherent((void *)chip->rirb,
                          (chip->rirb_entries ? chip->rirb_entries : 256u) * 8u);
    spk_iounmap(chip->mmio, chip->mmio_len);
    spk_kfree(chip);
}

static const struct spk_pci_device_id hda_ids[] = {
    { 0x8086u, 0x2668u, 0, 0, 0, 0, 0 },
    { 0, 0, 0, 0, 0, 0, 0 },
};

static const struct spk_pci_driver hda_driver = {
    .name = "hda",
    .id_table = hda_ids,
    .probe = hda_probe,
    .remove = hda_remove,
};

static int hda_init(void)
{
    int result = spk_pci_driver_register(&hda_driver);
    if (result == 0) {
        spk_pr_info("HDA_MODULE_OK\n");
    }
    return result;
}

static void hda_exit(void)
{
    spk_pci_driver_unregister(&hda_driver);
}

SPK_MODULE("hda", "1.0", "GPL", hda_init, hda_exit);
