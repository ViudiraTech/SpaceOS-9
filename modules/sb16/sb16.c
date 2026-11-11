/*
 *
 *       modules/sb16/sb16.c
 *       Creative Sound Blaster 16 ISA driver
 *
 *       2026/11/11 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 * Programming follows Creative's SB16 DSP documentation (reset handshake,
 * 0x41/0x42 sample-rate commands with high byte first, B6/BE auto-init
 * commands with mode/length-low/length-high, 0xD9 stop, mixer registers) and
 * the Intel 8237 DMA controller register map.  The fixed ISA configuration is
 * the classic 0x220 / IRQ 5 / DMA 1+5 used by QEMU's sb16 device.
 */

#include "spk/module.h"
#include "spk/sound.h"

#define SB_BASE 0x220u
#define SB_IRQ 5u

#define SB_RESET (SB_BASE + 0x06u)
#define SB_READ_DATA (SB_BASE + 0x0au)
#define SB_WRITE_DATA (SB_BASE + 0x0cu)
#define SB_READ_STATUS (SB_BASE + 0x0eu)
#define SB_ACK16 (SB_BASE + 0x0fu)
#define SB_MIXER_ADDR (SB_BASE + 0x04u)
#define SB_MIXER_DATA (SB_BASE + 0x05u)

#define DSP_GET_VERSION 0xe1u
#define DSP_SPEAKER_ON 0xd1u
#define DSP_SPEAKER_OFF 0xd3u
#define DSP_STOP_16_AUTO 0xd9u
#define DSP_PAUSE_16 0xd5u
#define DSP_CONTINUE_16 0xd6u
#define DSP_DMA16_AUTO_DAC 0xb6u
#define DSP_DMA16_AUTO_ADC 0xbeu

#define DMA_MASK16 0xd4u
#define DMA_FLIPFLOP16 0xd8u
#define DMA_MODE16 0xd6u
#define DMA_ADDR16 0xc4u
#define DMA_COUNT16 0xc6u
#define DMA_PAGE16_5 0x8bu

struct sb16_chip {
    unsigned short base;
    struct snd_card *card;
    struct snd_pcm *pcm;
    struct snd_pcm_substream *playback;
    struct snd_pcm_substream *capture;
    struct snd_pcm_substream *active;
    unsigned int rate;
    unsigned int channels;
    unsigned int period_frames;
    unsigned int buffer_frames;
    unsigned int frame_bytes;
    unsigned int period_bytes;
    unsigned int buffer_bytes;
    unsigned int signed_samples;
    volatile unsigned long long hw_ptr;
    volatile unsigned int running;
    volatile unsigned int period_pending;
    volatile unsigned int shutdown;
};

static struct sb16_chip *g_sb;

static void sb_write(struct sb16_chip *chip, unsigned short offset,
                     unsigned char value)
{
    spk_outb((unsigned short)(chip->base + offset), value);
}

static unsigned char sb_read(struct sb16_chip *chip, unsigned short offset)
{
    return spk_inb((unsigned short)(chip->base + offset));
}

static int dsp_write(struct sb16_chip *chip, unsigned char value)
{
    unsigned int timeout;
    for (timeout = 0; timeout < 10000u; timeout++) {
        if ((sb_read(chip, 0x0cu) & 0x80u) == 0) {
            sb_write(chip, 0x0cu, value);
            return 0;
        }
        spk_udelay(1);
    }
    return -1;
}

static int dsp_read(struct sb16_chip *chip, unsigned char *value)
{
    unsigned int timeout;
    for (timeout = 0; timeout < 10000u; timeout++) {
        if ((sb_read(chip, 0x0eu) & 0x80u) != 0) {
            *value = sb_read(chip, 0x0au);
            return 0;
        }
        spk_udelay(1);
    }
    return -1;
}

static int dsp_reset(struct sb16_chip *chip)
{
    unsigned int attempt;
    unsigned int timeout;
    unsigned char value;

    for (attempt = 0; attempt < 4u; attempt++) {
        sb_write(chip, 0x06u, 1);
        spk_udelay(3);
        sb_write(chip, 0x06u, 0);
        for (timeout = 0; timeout < 1000u; timeout++) {
            if ((sb_read(chip, 0x0eu) & 0x80u) != 0) {
                break;
            }
            spk_udelay(100);
        }
        if (timeout == 1000u) {
            continue;
        }
        if (dsp_read(chip, &value) == 0 && value == 0xaau) {
            return 0;
        }
    }
    return -1;
}

static void mixer_write(struct sb16_chip *chip, unsigned char reg,
                        unsigned char value)
{
    sb_write(chip, 0x04u, reg);
    spk_udelay(1);
    sb_write(chip, 0x05u, value);
    spk_udelay(1);
}

static unsigned char mixer_read(struct sb16_chip *chip, unsigned char reg)
{
    sb_write(chip, 0x04u, reg);
    spk_udelay(1);
    return sb_read(chip, 0x05u);
}

static int dsp_set_rate(struct sb16_chip *chip, int capture, unsigned int rate)
{
    if (dsp_write(chip, (unsigned char)(capture ? 0x42u : 0x41u)) != 0) {
        return -1;
    }
    if (dsp_write(chip, (unsigned char)(rate >> 8)) != 0) {
        return -1;
    }
    if (dsp_write(chip, (unsigned char)(rate & 0xffu)) != 0) {
        return -1;
    }
    return 0;
}

static void sb_dma_mask(struct sb16_chip *chip, int mask)
{
    (void)chip;
    spk_outb(DMA_MASK16, (unsigned char)(0x04u | 0x01u));
    if (!mask) {
        spk_outb(DMA_MASK16, 0x01u);
    }
}

static int sb_dma_program(struct sb16_chip *chip, unsigned long long address,
                          unsigned int bytes, int capture)
{
    unsigned short words;
    unsigned short offset;

    if (bytes < 2u || (bytes & 1u) != 0 || bytes > 0x10000u) {
        return -1;
    }
    words = (unsigned short)(bytes / 2u - 1u);
    offset = (unsigned short)(address >> 1);
    sb_dma_mask(chip, 1);
    spk_outb(DMA_FLIPFLOP16, 0x00);
    spk_outb(DMA_MODE16, (unsigned char)(capture ? 0x55u : 0x59u));
    spk_outb(DMA_ADDR16, (unsigned char)(offset & 0xffu));
    spk_outb(DMA_ADDR16, (unsigned char)(offset >> 8));
    spk_outb(DMA_PAGE16_5, (unsigned char)((address >> 16) & 0xfeu));
    spk_outb(DMA_COUNT16, (unsigned char)(words & 0xffu));
    spk_outb(DMA_COUNT16, (unsigned char)(words >> 8));
    spk_outb(DMA_MASK16, 0x01u);
    return 0;
}

static void sb_dma_stop(void)
{
    spk_outb(DMA_MASK16, 0x05u);
}

static void sb_stop(struct sb16_chip *chip)
{
    chip->running = 0;
    if (dsp_write(chip, DSP_STOP_16_AUTO) == 0) {
        spk_udelay(100);
    }
    if (dsp_write(chip, DSP_SPEAKER_OFF) != 0) {
        /* best effort */
    }
    sb_dma_stop();
}

static void sb_period_work(void *data)
{
    struct sb16_chip *chip = (struct sb16_chip *)data;
    unsigned long long position;
    chip->period_pending = 0;
    if (chip->shutdown || !chip->running || chip->active == 0 ||
        chip->period_frames == 0) {
        return;
    }
    position = chip->hw_ptr + chip->period_frames;
    if (chip->buffer_frames != 0) {
        position %= chip->buffer_frames;
    }
    chip->hw_ptr = position;
    snd_pcm_period_elapsed(chip->active);
}

static void sb_irq(int irq, void *data)
{
    struct sb16_chip *chip = (struct sb16_chip *)data;
    (void)irq;

    (void)sb_read(chip, 0x0fu); /* acknowledge the 16-bit IRQ */
    if (!chip->shutdown && chip->running && chip->active != 0 &&
        !chip->period_pending) {
        chip->period_pending = 1;
        (void)spk_work_schedule(sb_period_work, chip);
    }
}

static int sb_open(struct snd_pcm_substream *substream, int capture)
{
    struct sb16_chip *chip = g_sb;
    unsigned long long formats =
        (1ull << SNDRV_PCM_FORMAT_S16_LE) | (1ull << SNDRV_PCM_FORMAT_U16_LE);

    if (chip == 0) {
        return -19;
    }
    if (chip->active != 0 && chip->active != substream) {
        return -16; /* SB16 is half duplex */
    }
    chip->active = substream;
    if (capture) {
        chip->capture = substream;
    } else {
        chip->playback = substream;
    }
    snd_pcm_set_formats(substream, formats);
    snd_pcm_set_isa_dma(substream);
    snd_pcm_hw_constraint_minmax(substream, SNDRV_PCM_HW_PARAM_RATE, 4000, 44100);
    snd_pcm_hw_constraint_minmax(substream, SNDRV_PCM_HW_PARAM_CHANNELS, 1, 2);
    snd_pcm_hw_constraint_minmax(substream, SNDRV_PCM_HW_PARAM_PERIODS, 2, 32);
    snd_pcm_hw_constraint_minmax(substream, SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
                                 4096, 65536);
    return 0;
}

static int sb_playback_open(struct snd_pcm_substream *substream)
{
    return sb_open(substream, 0);
}

static int sb_capture_open(struct snd_pcm_substream *substream)
{
    return sb_open(substream, 1);
}

static int sb_close(struct snd_pcm_substream *substream, int capture)
{
    struct sb16_chip *chip = g_sb;
    if (chip == 0) {
        return 0;
    }
    sb_stop(chip);
    if (capture) {
        chip->capture = 0;
    } else {
        chip->playback = 0;
    }
    if (chip->active == substream) {
        chip->active = 0;
    }
    return 0;
}

static int sb_playback_close(struct snd_pcm_substream *substream)
{
    return sb_close(substream, 0);
}

static int sb_capture_close(struct snd_pcm_substream *substream)
{
    return sb_close(substream, 1);
}

static int sb_hw_params(struct snd_pcm_substream *substream,
                        struct snd_pcm_hw_params *params)
{
    struct sb16_chip *chip = g_sb;
    int capture;
    (void)params;

    if (chip == 0) {
        return -19;
    }
    capture = (substream == chip->capture);
    chip->rate = snd_pcm_get_rate(substream);
    chip->channels = snd_pcm_get_channels(substream);
    chip->period_frames = (unsigned int)snd_pcm_get_period_size(substream);
    chip->buffer_frames = (unsigned int)snd_pcm_get_buffer_size(substream);
    chip->signed_samples = snd_pcm_get_format(substream) == SNDRV_PCM_FORMAT_S16_LE;
    chip->frame_bytes = chip->channels * 2u;
    chip->period_bytes = chip->period_frames * chip->frame_bytes;
    chip->buffer_bytes = chip->buffer_frames * chip->frame_bytes;
    if (chip->period_bytes == 0 || chip->buffer_bytes == 0 ||
        (chip->period_bytes & 1u) != 0 || (chip->buffer_bytes & 1u) != 0 ||
        chip->buffer_bytes > 0x10000u) {
        return -22;
    }
    mixer_write(chip, 0x0e, (unsigned char)(chip->channels == 2u ? 0x02u : 0x00u));
    if (dsp_set_rate(chip, capture, chip->rate) != 0) {
        return -5;
    }
    return 0;
}

static int sb_hw_free(struct snd_pcm_substream *substream)
{
    (void)substream;
    if (g_sb != 0) {
        sb_stop(g_sb);
    }
    return 0;
}

static int sb_prepare(struct snd_pcm_substream *substream)
{
    struct sb16_chip *chip = g_sb;
    (void)substream;
    if (chip == 0) {
        return -19;
    }
    sb_stop(chip);
    chip->hw_ptr = 0;
    return 0;
}

static int sb_trigger(struct snd_pcm_substream *substream, int cmd)
{
    struct sb16_chip *chip = g_sb;
    int capture;
    unsigned char command;
    unsigned char mode;
    unsigned short length;

    if (chip == 0) {
        return -19;
    }
    capture = (substream == chip->capture);
    switch (cmd) {
    case SNDRV_PCM_TRIGGER_START:
    case SNDRV_PCM_TRIGGER_RESUME:
    case SNDRV_PCM_TRIGGER_PAUSE_RELEASE:
        if (cmd == SNDRV_PCM_TRIGGER_PAUSE_RELEASE) {
            if (dsp_write(chip, DSP_CONTINUE_16) != 0) {
                return -5;
            }
            return 0;
        }
        if (sb_dma_program(chip, snd_pcm_get_dma_addr(substream),
                           chip->buffer_bytes, capture) != 0) {
            return -22;
        }
        command = (unsigned char)(capture ? DSP_DMA16_AUTO_ADC : DSP_DMA16_AUTO_DAC);
        mode = (unsigned char)((chip->channels == 2u ? 0x20u : 0x00u) |
                               (chip->signed_samples ? 0x10u : 0x00u));
        length = (unsigned short)(chip->period_bytes / 2u - 1u);
        if (dsp_write(chip, command) != 0 || dsp_write(chip, mode) != 0 ||
            dsp_write(chip, (unsigned char)(length & 0xffu)) != 0 ||
            dsp_write(chip, (unsigned char)(length >> 8)) != 0) {
            sb_dma_stop();
            return -5;
        }
        if (!capture && dsp_write(chip, DSP_SPEAKER_ON) != 0) {
            sb_dma_stop();
            return -5;
        }
        chip->hw_ptr = 0;
        chip->running = 1;
        break;
    case SNDRV_PCM_TRIGGER_STOP:
    case SNDRV_PCM_TRIGGER_SUSPEND:
        sb_stop(chip);
        break;
    case SNDRV_PCM_TRIGGER_PAUSE_PUSH:
        chip->running = 0;
        if (dsp_write(chip, DSP_PAUSE_16) != 0) {
            return -5;
        }
        break;
    default:
        return -22;
    }
    return 0;
}

static snd_pcm_uframes_t sb_pointer(struct snd_pcm_substream *substream)
{
    struct sb16_chip *chip = g_sb;
    (void)substream;
    if (chip == 0) {
        return 0;
    }
    return (snd_pcm_uframes_t)chip->hw_ptr;
}

static struct snd_pcm_ops sb_playback_ops = {
    .open = sb_playback_open,
    .close = sb_playback_close,
    .ioctl = 0,
    .hw_params = sb_hw_params,
    .hw_free = sb_hw_free,
    .prepare = sb_prepare,
    .trigger = sb_trigger,
    .pointer = sb_pointer,
};

static struct snd_pcm_ops sb_capture_ops = {
    .open = sb_capture_open,
    .close = sb_capture_close,
    .ioctl = 0,
    .hw_params = sb_hw_params,
    .hw_free = sb_hw_free,
    .prepare = sb_prepare,
    .trigger = sb_trigger,
    .pointer = sb_pointer,
};

static int sb_master_info(struct snd_kcontrol *kcontrol,
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

static int sb_master_get(struct snd_kcontrol *kcontrol,
                         struct snd_ctl_elem_value *value)
{
    struct sb16_chip *chip = (struct sb16_chip *)kcontrol->private_data;
    unsigned char raw = mixer_read(chip, 0x22u);
    value->value.integer.value[0] = (raw >> 4) * 100 / 15;
    value->value.integer.value[1] = (raw & 0x0fu) * 100 / 15;
    return 0;
}

static int sb_master_put(struct snd_kcontrol *kcontrol,
                         struct snd_ctl_elem_value *value)
{
    struct sb16_chip *chip = (struct sb16_chip *)kcontrol->private_data;
    long left = value->value.integer.value[0];
    long right = value->value.integer.value[1];
    unsigned char encoded;
    if (left < 0) left = 0;
    if (left > 100) left = 100;
    if (right < 0) right = 0;
    if (right > 100) right = 100;
    encoded = (unsigned char)(((left * 15 / 100) << 4) | (right * 15 / 100));
    mixer_write(chip, 0x22u, encoded);
    return 1;
}

static struct snd_kcontrol_new sb_master = {
    .iface = SNDRV_CTL_ELEM_IFACE_MIXER,
    .name = "Master Playback Volume",
    .access = SNDRV_CTL_ELEM_ACCESS_READWRITE,
    .count = 2,
    .info = sb_master_info,
    .get = sb_master_get,
    .put = sb_master_put,
    .private_data = 0,
};

static int sb_init(void)
{
    struct sb16_chip *chip;
    unsigned char major = 0;
    unsigned char minor = 0;
    int result;

    if (g_sb != 0) {
        return -16;
    }
    chip = (struct sb16_chip *)spk_kzalloc(sizeof(*chip));
    if (chip == 0) {
        return -12;
    }
    chip->base = SB_BASE;
    if (dsp_reset(chip) != 0) {
        spk_kfree(chip);
        return -19;
    }
    if (dsp_write(chip, DSP_GET_VERSION) != 0 ||
        dsp_read(chip, &major) != 0 || dsp_read(chip, &minor) != 0 ||
        major < 4u) {
        spk_kfree(chip);
        return -19;
    }
    mixer_write(chip, 0x22u, 0xccu);
    mixer_write(chip, 0x04u, 0xccu);
    mixer_write(chip, 0x0au, 0x08u);
    mixer_write(chip, 0x0cu, 0x00u);
    mixer_write(chip, 0x0eu, 0x00u);
    mixer_write(chip, 0x80u, 0x02u); /* IRQ 5 */
    mixer_write(chip, 0x81u, 0x02u); /* DMA 5 */
    if (spk_request_isa_irq(SB_IRQ, sb_irq, chip) != 0) {
        spk_kfree(chip);
        return -19;
    }
    if (snd_card_new(-1, "SB16", "SB16", "Sound Blaster 16",
                     "Creative Sound Blaster 16 (ISA)", &chip->card) != 0) {
        spk_free_isa_irq(SB_IRQ);
        spk_kfree(chip);
        return -12;
    }
    if (snd_pcm_new(chip->card, "SB16 PCM", 0, 1, 1, &chip->pcm) != 0 ||
        snd_pcm_set_ops(chip->pcm, SNDRV_PCM_STREAM_PLAYBACK,
                        &sb_playback_ops) != 0 ||
        snd_pcm_set_ops(chip->pcm, SNDRV_PCM_STREAM_CAPTURE,
                        &sb_capture_ops) != 0) {
        snd_card_free(chip->card);
        spk_free_isa_irq(SB_IRQ);
        spk_kfree(chip);
        return -12;
    }
    sb_master.private_data = chip;
    if (snd_ctl_add(chip->card, snd_ctl_new1(&sb_master, chip)) != 0) {
        snd_card_free(chip->card);
        spk_free_isa_irq(SB_IRQ);
        spk_kfree(chip);
        return -12;
    }
    g_sb = chip;
    result = snd_card_register(chip->card);
    if (result != 0) {
        g_sb = 0;
        snd_card_free(chip->card);
        spk_free_isa_irq(SB_IRQ);
        spk_kfree(chip);
        return result;
    }
    spk_pr_info("sb16: DSP 4.x attached at 0x220 irq 5 dma16 5\n");
    spk_pr_info("SB16_MODULE_OK\n");
    return 0;
}

static void sb_exit(void)
{
    struct sb16_chip *chip = g_sb;
    if (chip == 0) {
        return;
    }
    chip->shutdown = 1;
    sb_stop(chip);
    g_sb = 0;
    snd_card_free(chip->card);
    spk_free_isa_irq(SB_IRQ);
    spk_msleep(20);
    spk_kfree(chip);
}

SPK_MODULE("sb16", "1.0", "GPL", sb_init, sb_exit);
