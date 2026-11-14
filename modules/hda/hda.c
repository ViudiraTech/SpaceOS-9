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
#define HDA_STATESTS 0x0e
#define HDA_INTCTL 0x20
#define HDA_INTSTS 0x24
#define HDA_INT_GLOBAL 0x80000000u
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
#define HDA_RIRBCTL_DMA 0x02

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

#define AC_PINCAP_OUT 0x10u
#define AC_PINCAP_IN 0x20u

#define AC_PINCTL_OUT_EN 0x40u
#define AC_PINCTL_IN_EN 0x20u

#define AC_AMP_OUT_BOTH 0xb000u
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
    unsigned char out_gain_max;
    unsigned char out_gain_default;
    unsigned char in_gain_max;
    unsigned char in_gain_default;
    unsigned char pin_out_amp;
    unsigned char pin_out_gain;
    unsigned char pin_in_amp;
    unsigned char pin_in_gain;
    unsigned char codec_ready;
    volatile unsigned int period_pending[2];
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

static unsigned int hda_verb(struct hda_chip *chip, unsigned int nid,
                             unsigned int verb, unsigned int parm)
{
    unsigned int command = ((chip->codec & 0xfu) << 28) | ((nid & 0xffu) << 20) |
                           ((verb & 0xfffu) << 8) | (parm & 0xffffu);
    unsigned int timeout;
    unsigned short next_wp = (unsigned short)((chip->corb_wp + 1u) & 0xffu);
    unsigned char previous = chip->rirb_wp;

    chip->corb[next_wp] = command;
    spk_mb();
    hda_writew(chip, HDA_CORBWP, next_wp);
    chip->corb_wp = next_wp;
    for (timeout = 0; timeout < 100000u; timeout++) {
        unsigned char current = (unsigned char)(hda_readw(chip, HDA_RIRBWP) & 0xffu);
        if (current != previous) {
            unsigned int response = (unsigned int)chip->rirb[current];
            chip->rirb_wp = current;
            chip->rirb_seen = 1;
            return response;
        }
        spk_udelay(1);
    }
    hda_verb_timeouts++;
    if (hda_verb_timeouts <= 16u) {
        spk_printk("hda: verb timeout nid=%x verb=%x parm=%x\n",
                   nid, verb, parm);
    }
    return 0xffffffffu;
}

static void hda_init_rings(struct hda_chip *chip)
{
    unsigned long long corb_phys = 0;
    unsigned long long rirb_phys = 0;
    void *corb = spk_dma_alloc_coherent(256u * 4u, 128u, &corb_phys);
    void *rirb = spk_dma_alloc_coherent(256u * 8u, 128u, &rirb_phys);
    unsigned int timeout;

    if (corb == 0 || rirb == 0) {
        return;
    }
    chip->corb = (volatile unsigned int *)corb;
    chip->rirb = (volatile unsigned long long *)rirb;
    hda_writel(chip, HDA_CORBLBASE, (unsigned int)corb_phys);
    hda_writel(chip, HDA_CORBUBASE, (unsigned int)(corb_phys >> 32));
    hda_writel(chip, HDA_RIRBLBASE, (unsigned int)rirb_phys);
    hda_writel(chip, HDA_RIRBUBASE, (unsigned int)(rirb_phys >> 32));
    hda_writeb(chip, HDA_CORBSIZE, 0x02); /* 256 entries */
    hda_writeb(chip, HDA_RIRBSIZE, 0x02);
    hda_writew(chip, HDA_CORBRP, HDA_CORBRP_RST);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if (hda_readw(chip, HDA_CORBRP) & HDA_CORBRP_RST) {
            break;
        }
        spk_udelay(10);
    }
    hda_writew(chip, HDA_CORBRP, 0);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if ((hda_readw(chip, HDA_CORBRP) & HDA_CORBRP_RST) == 0) {
            break;
        }
        spk_udelay(10);
    }
    hda_writew(chip, HDA_CORBWP, 0);
    hda_writew(chip, HDA_RIRBWP, HDA_RIRBWP_RST);
    hda_writew(chip, HDA_RINTCNT, 0xff);
    hda_writeb(chip, HDA_CORBCTL, HDA_CORBCTL_RUN);
    hda_writeb(chip, HDA_RIRBCTL, HDA_RIRBCTL_DMA);
    chip->corb_wp = 0;
    chip->rirb_wp = (unsigned char)(hda_readw(chip, HDA_RIRBWP) & 0xffu);
    chip->rirb_seen = 0;
}

static void hda_query_amp_caps(struct hda_chip *chip, unsigned int nid,
                               unsigned int param, unsigned char *max_out,
                               unsigned char *def_out)
{
    unsigned int amp = hda_verb(chip, nid, AC_VERB_GET_PARAMETERS, param);
    unsigned int steps;
    unsigned int offset;

    if (amp == 0xffffffffu || amp == 0u) {
        *max_out = 0x4au;
        *def_out = 0x4au;
        return;
    }
    steps = (amp >> 8) & AC_AMP_GAIN_MASK;
    offset = amp & AC_AMP_GAIN_MASK;
    if (offset > steps) {
        offset = steps;
    }
    *max_out = (unsigned char)steps;
    *def_out = (unsigned char)(offset > 0u ? offset : steps);
}

static int hda_discover_codec(struct hda_chip *chip)
{
    unsigned int nodes = hda_verb(chip, 0, AC_VERB_GET_PARAMETERS,
                                  AC_PAR_NODE_COUNT);
    unsigned int start = nodes & 0xffu;
    unsigned int count = (nodes >> 8) & 0xffu;
    unsigned int index;
    unsigned int dac_cap = 0;
    unsigned int adc_cap = 0;
    unsigned int pin_out_cap = 0;
    unsigned int pin_in_cap = 0;

    chip->afg = 0;
    chip->dac = 0;
    chip->adc = 0;
    chip->pin_out = 0;
    chip->pin_in = 0;
    if (start == 0 || count == 0 || start + count > 256u) {
        start = 1;
        count = 8;
    }
    for (index = start; index < start + count; index++) {
        unsigned int type = hda_verb(chip, index, AC_VERB_GET_PARAMETERS,
                                     AC_PAR_FUNCTION_TYPE) & 0xffu;
        if (type == 0x01u && chip->afg == 0) {
            chip->afg = index;
        }
    }
    if (chip->afg == 0) {
        chip->afg = 1;
    }
    for (index = chip->afg + 1; index < start + count; index++) {
        unsigned int cap = hda_verb(chip, index, AC_VERB_GET_PARAMETERS,
                                    AC_PAR_AUDIO_WIDGET_CAP);
        unsigned int type = (cap >> 20) & 0xfu;
        if (type == AC_WID_AUD_OUT && chip->dac == 0) {
            chip->dac = index;
            dac_cap = cap;
        } else if (type == AC_WID_AUD_IN && chip->adc == 0) {
            chip->adc = index;
            adc_cap = cap;
        } else if (type == AC_WID_PIN) {
            unsigned int pincap = hda_verb(chip, index, AC_VERB_GET_PARAMETERS,
                                           AC_PAR_PIN_CAP);
            if ((pincap & AC_PINCAP_OUT) && chip->pin_out == 0) {
                chip->pin_out = index;
                pin_out_cap = cap;
            }
            if ((pincap & AC_PINCAP_IN) && chip->pin_in == 0) {
                chip->pin_in = index;
                pin_in_cap = cap;
            }
        }
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
                       &chip->out_gain_max, &chip->out_gain_default);
    hda_query_amp_caps(chip, chip->adc, AC_PAR_AMP_IN_CAP,
                       &chip->in_gain_max, &chip->in_gain_default);
    chip->pin_out_amp = (pin_out_cap & AC_WCAP_OUT_AMP) ? 1u : 0u;
    if (chip->pin_out_amp) {
        unsigned char dummy_max = 0;
        hda_query_amp_caps(chip, chip->pin_out, AC_PAR_AMP_OUT_CAP,
                           &dummy_max, &chip->pin_out_gain);
    }
    chip->pin_in_amp = (pin_in_cap & AC_WCAP_IN_AMP) ? 1u : 0u;
    if (chip->pin_in_amp) {
        unsigned char dummy_max = 0;
        hda_query_amp_caps(chip, chip->pin_in, AC_PAR_AMP_IN_CAP,
                           &dummy_max, &chip->pin_in_gain);
    }
    chip->gains[0] = chip->out_gain_default;
    chip->gains[1] = chip->in_gain_default;
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
        hda_verb(chip, nid, AC_VERB_SET_AMP_GAIN_MUTE,
                 AC_AMP_OUT_BOTH |
                     (chip->gains[0] ? chip->gains[0] : AC_AMP_MUTE));
        hda_program_converter(chip, nid, HDA_STREAM_TAG);
        hda_verb(chip, pin, AC_VERB_SET_PIN_WIDGET_CONTROL, AC_PINCTL_OUT_EN);
        if (chip->pin_out_amp) {
            hda_verb(chip, pin, AC_VERB_SET_AMP_GAIN_MUTE,
                     AC_AMP_OUT_BOTH | chip->pin_out_gain);
        }
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

    hda_stream_reset(chip, capture);
    hda_writeb(chip, offset + SD_STS, SD_INT_MASK);
    hda_writel(chip, offset + SD_CBL, chip->cbl[capture]);
    hda_writew(chip, offset + SD_LVI, (unsigned short)(entries - 1));
    hda_writew(chip, offset + SD_FORMAT, chip->format[capture]);
    hda_writel(chip, offset + SD_BDLPL, (unsigned int)bdl_phys);
    hda_writel(chip, offset + SD_BDLPU, (unsigned int)(bdl_phys >> 32));
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

static void hda_stream_stop(struct hda_chip *chip, int capture)
{
    unsigned int offset = hda_stream_offset(capture);
    unsigned int value = hda_readl(chip, offset + SD_CTL);
    unsigned int timeout;

    if ((value & SD_CTL_RUN) != 0) {
        hda_writel(chip, offset + SD_CTL, value & ~SD_CTL_RUN);
        for (timeout = 0; timeout < 1000u; timeout++) {
            if ((hda_readl(chip, offset + SD_CTL) & SD_CTL_RUN) == 0) {
                break;
            }
            spk_udelay(10);
        }
    }
    hda_writel(chip, HDA_INTCTL,
               hda_readl(chip, HDA_INTCTL) & ~hda_int_bit(capture));
    chip->running[capture] = 0;
}

static void hda_period_play(void *data)
{
    struct hda_chip *chip = (struct hda_chip *)data;
    chip->period_pending[0] = 0;
    if (chip->shutdown) {
        return;
    }
    if (chip->running[0] && chip->playback != 0) {
        snd_pcm_period_elapsed(chip->playback);
    }
}

static void hda_period_capture(void *data)
{
    struct hda_chip *chip = (struct hda_chip *)data;
    chip->period_pending[1] = 0;
    if (chip->shutdown) {
        return;
    }
    if (chip->running[1] && chip->capture != 0) {
        snd_pcm_period_elapsed(chip->capture);
    }
}

static void hda_irq(int irq, void *data)
{
    struct hda_chip *chip = (struct hda_chip *)data;
    unsigned int status;
    (void)irq;

    status = hda_readl(chip, HDA_INTSTS);
    if (status == 0) {
        return;
    }
    if (status & hda_int_bit(0)) {
        unsigned char stream = hda_readb(chip, HDA_PLAY_SD + SD_STS);
        hda_writeb(chip, HDA_PLAY_SD + SD_STS, SD_INT_MASK);
        if ((stream & 0x04u) != 0 && chip->playback != 0 &&
            !chip->shutdown && !chip->period_pending[0]) {
            chip->period_pending[0] = 1;
            (void)spk_work_schedule(hda_period_play, chip);
        }
    }
    if (status & hda_int_bit(1)) {
        unsigned char stream = hda_readb(chip, HDA_CAP_SD + SD_STS);
        hda_writeb(chip, HDA_CAP_SD + SD_STS, SD_INT_MASK);
        if ((stream & 0x04u) != 0 && chip->capture != 0 &&
            !chip->shutdown && !chip->period_pending[1]) {
            chip->period_pending[1] = 1;
            (void)spk_work_schedule(hda_period_capture, chip);
        }
    }
    hda_writel(chip, HDA_INTSTS, status);
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
    hda_stream_reset(chip, capture);
    hda_writeb(chip, offset + SD_STS, SD_INT_MASK);
    if (chip->bdl_entries[capture] != 0) {
        hda_writel(chip, offset + SD_CBL, chip->cbl[capture]);
        hda_writew(chip, offset + SD_LVI,
                   (unsigned short)(chip->bdl_entries[capture] - 1));
        hda_writew(chip, offset + SD_FORMAT, chip->format[capture]);
        hda_writel(chip, offset + SD_BDLPL,
                   (unsigned int)chip->bdl_addr[capture]);
        hda_writel(chip, offset + SD_BDLPU,
                   (unsigned int)(chip->bdl_addr[capture] >> 32));
    }
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
    case SNDRV_PCM_TRIGGER_PAUSE_RELEASE:
        value = SD_CTL_TAG(HDA_STREAM_TAG) | SD_CTL_IOCE | SD_CTL_RUN;
        if (capture) {
            value |= SD_CTL_DIR;
        }
        hda_writel(chip, offset + SD_CTL, value);
        hda_writel(chip, HDA_INTCTL,
                   hda_readl(chip, HDA_INTCTL) | HDA_INT_GLOBAL |
                   hda_int_bit(capture));
        chip->running[capture] = 1;
        break;
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
    long max_step = chip->out_gain_max ? (long)chip->out_gain_max : 127L;
    long pct = max_step > 0 ? ((long)chip->gains[0] * 100L + max_step / 2) / max_step : 0;
    if (pct > 100) {
        pct = 100;
    }
    value->value.integer.value[0] = pct;
    value->value.integer.value[1] = pct;
    return 0;
}

static int hda_volume_put(struct snd_kcontrol *kcontrol,
                          struct snd_ctl_elem_value *value)
{
    struct hda_chip *chip = (struct hda_chip *)kcontrol->private_data;
    long max_step = chip->out_gain_max ? (long)chip->out_gain_max : 127L;
    long gain = value->value.integer.value[0];
    if (gain < 0) {
        gain = 0;
    }
    if (gain > 100) {
        gain = 100;
    }
    chip->gains[0] = (unsigned char)((gain * max_step + 50L) / 100L);
    hda_verb(chip, chip->dac, AC_VERB_SET_AMP_GAIN_MUTE,
             AC_AMP_OUT_BOTH |
                 ((gain == 0 || chip->gains[0] == 0) ? AC_AMP_MUTE : chip->gains[0]));
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

    gctl = hda_readl(chip, HDA_GCTL);
    hda_writel(chip, HDA_GCTL, gctl & ~HDA_GCTL_RESET);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if ((hda_readl(chip, HDA_GCTL) & HDA_GCTL_RESET) == 0) {
            break;
        }
        spk_udelay(100);
    }
    hda_writel(chip, HDA_GCTL, gctl | HDA_GCTL_RESET);
    for (timeout = 0; timeout < 1000u; timeout++) {
        if ((hda_readl(chip, HDA_GCTL) & HDA_GCTL_RESET) != 0) {
            break;
        }
        spk_udelay(100);
    }
    spk_udelay(1000);
    for (timeout = 0; timeout < 1000u; timeout++) {
        statests = hda_readw(chip, HDA_STATESTS);
        if (statests != 0) {
            break;
        }
        spk_udelay(1000);
    }
    if (statests == 0) {
        result = -19;
        goto fail_mmio;
    }
    chip->codec = 0;
    while (chip->codec < 16u && (statests & (1u << chip->codec)) == 0) {
        chip->codec++;
    }
    hda_init_rings(chip);
    if (chip->corb == 0 || chip->rirb == 0) {
        result = -12;
        goto fail_mmio;
    }
    if (hda_discover_codec(chip) != 0) {
        result = -19;
        goto fail_mmio;
    }
    if (spk_pci_enable_msi(device, hda_irq, chip, 0, 0) != 0) {
        if (spk_request_irq(device, hda_irq, chip) != 0) {
            result = -19;
            goto fail_mmio;
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
    if (snd_ctl_add(chip->card,
                    snd_ctl_new1(&hda_master_volume, chip)) != 0) {
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
fail_mmio:
    spk_dma_free_coherent((void *)chip->corb, 256u * 4u);
    spk_dma_free_coherent((void *)chip->rirb, 256u * 8u);
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
    hda_stream_stop(chip, 0);
    hda_stream_stop(chip, 1);
    g_hda = 0;
    snd_card_free(chip->card);
    spk_free_irq(chip->pci);
    spk_msleep(20);
    spk_dma_free_coherent(chip->bdl[0], chip->bdl_entries[0] * 16u);
    spk_dma_free_coherent(chip->bdl[1], chip->bdl_entries[1] * 16u);
    spk_dma_free_coherent((void *)chip->corb, 256u * 4u);
    spk_dma_free_coherent((void *)chip->rirb, 256u * 8u);
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
