/*
 *
 *       user/bin/wavplay.c
 *       SpaceOS-9 low-latency PCM WAV sound effect player
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 * Designed specifically for short system sound effects: parses standard RIFF
 * 16-bit PCM WAV headers with zero decoding overhead and writes directly to
 * the kernel ALSA PCM playback device (/proc/asound/cards, preferring HDA).
 */

#include "splibc.h"
#include "alsa_pcm.h"

#define PERIOD_FRAMES 512u
#define PERIODS 4u

static unsigned int read_u32_le(const unsigned char *p)
{
    return (unsigned int)p[0] | ((unsigned int)p[1] << 8) |
           ((unsigned int)p[2] << 16) | ((unsigned int)p[3] << 24);
}

static unsigned short read_u16_le(const unsigned char *p)
{
    return (unsigned short)p[0] | ((unsigned short)p[1] << 8);
}

static int string_contains(const char *haystack, const char *needle)
{
    unsigned long i;
    for (i = 0; haystack[i] != 0; i++) {
        unsigned long j = 0;
        while (needle[j] != 0 && haystack[i + j] != 0 &&
               haystack[i + j] == needle[j]) {
            j++;
        }
        if (needle[j] == 0) {
            return 1;
        }
    }
    return 0;
}

static int parse_card_number(const char *line)
{
    int value = 0;
    int seen = 0;
    while (*line == ' ' || *line == '\t') {
        line++;
    }
    while (*line >= '0' && *line <= '9') {
        value = value * 10 + (*line - '0');
        seen = 1;
        line++;
    }
    return seen ? value : -1;
}

static int choose_card(void)
{
    char buffer[4096];
    int fd = open("/proc/asound/cards", O_RDONLY);
    long length;
    int first = -1;
    int hda = -1;
    int sb = -1;
    unsigned long offset = 0;

    if (fd < 0) {
        return -1;
    }
    length = read(fd, buffer, sizeof(buffer) - 1);
    close(fd);
    if (length <= 0) {
        return -1;
    }
    buffer[length] = 0;

    while (offset < (unsigned long)length) {
        char line[256];
        unsigned long index = 0;
        int card;
        while (offset < (unsigned long)length && buffer[offset] != '\n' &&
               index + 1 < sizeof(line)) {
            line[index++] = buffer[offset++];
        }
        while (offset < (unsigned long)length && buffer[offset] != '\n') {
            offset++;
        }
        if (offset < (unsigned long)length && buffer[offset] == '\n') {
            offset++;
        }
        line[index] = 0;
        card = parse_card_number(line);
        if (card >= 0) {
            if (first < 0) {
                first = card;
            }
            if (hda < 0 && string_contains(line, "HDA")) {
                hda = card;
            }
            if (sb < 0 && string_contains(line, "SB16")) {
                sb = card;
            }
        }
    }
    if (hda >= 0) {
        return hda;
    }
    if (sb >= 0) {
        return sb;
    }
    return first;
}

static void hw_set_mask(struct smp_hw_params *hw, int index, unsigned int bit)
{
    hw->masks[index].bits[bit / 32u] |= 1u << (bit % 32u);
}

static void hw_set_interval(struct smp_hw_params *hw, int param,
                            unsigned int min, unsigned int max)
{
    unsigned int idx = (unsigned int)(param - SNDRV_PCM_HW_PARAM_SAMPLE_BITS);
    hw->intervals[idx].min = min;
    hw->intervals[idx].max = max;
    hw->intervals[idx].flags = 4;
}

static int try_hw_params(int fd, unsigned int rate, unsigned int channels,
                         unsigned int period_frames, unsigned int periods,
                         unsigned int *period_out)
{
    struct smp_hw_params hw;
    unsigned int buffer_frames = period_frames * periods;
    unsigned int ps_idx = (unsigned int)(SNDRV_PCM_HW_PARAM_PERIOD_SIZE - SNDRV_PCM_HW_PARAM_SAMPLE_BITS);

    memset(&hw, 0, sizeof(hw));
    hw_set_mask(&hw, SNDRV_PCM_HW_PARAM_ACCESS, SNDRV_PCM_ACCESS_RW_INTERLEAVED);
    hw_set_mask(&hw, SNDRV_PCM_HW_PARAM_FORMAT, SNDRV_PCM_FORMAT_S16_LE);
    hw_set_mask(&hw, SNDRV_PCM_HW_PARAM_SUBFORMAT, SNDRV_PCM_SUBFORMAT_STD);
    hw_set_interval(&hw, SNDRV_PCM_HW_PARAM_SAMPLE_BITS, 16, 16);
    hw_set_interval(&hw, SNDRV_PCM_HW_PARAM_FRAME_BITS, 16u * channels, 16u * channels);
    hw_set_interval(&hw, SNDRV_PCM_HW_PARAM_CHANNELS, channels, channels);
    hw_set_interval(&hw, SNDRV_PCM_HW_PARAM_RATE, rate, rate);
    hw_set_interval(&hw, SNDRV_PCM_HW_PARAM_PERIOD_SIZE, period_frames, period_frames);
    hw_set_interval(&hw, SNDRV_PCM_HW_PARAM_PERIODS, periods, periods);
    hw_set_interval(&hw, SNDRV_PCM_HW_PARAM_BUFFER_SIZE, buffer_frames, buffer_frames);

    if (ioctl(fd, SNDRV_PCM_IOCTL_HW_PARAMS, &hw) < 0) {
        return -1;
    }
    *period_out = hw.intervals[ps_idx].min;
    if (*period_out == 0) {
        *period_out = period_frames;
    }
    return 0;
}

static int configure_pcm(int fd, unsigned int rate, unsigned int channels,
                         unsigned int *period_out)
{
    static const unsigned int attempts[][2] = {
        { 512, 4 },
        { 1024, 4 },
        { 2048, 4 },
        { 512, 8 },
        { 2048, 8 },
    };
    struct smp_sw_params sw;
    unsigned int period_frames = 0;
    unsigned int periods = 0;
    unsigned int buffer_frames;
    unsigned int start_threshold;
    unsigned int index;

    for (index = 0; index < sizeof(attempts) / sizeof(attempts[0]); index++) {
        if (try_hw_params(fd, rate, channels, attempts[index][0], attempts[index][1],
                          &period_frames) == 0) {
            periods = attempts[index][1];
            break;
        }
    }
    if (period_frames == 0 || periods == 0) {
        return -1;
    }

    buffer_frames = period_frames * periods;
    start_threshold = period_frames;
    if (start_threshold > buffer_frames) {
        start_threshold = buffer_frames;
    }

    memset(&sw, 0, sizeof(sw));
    sw.avail_min = period_frames;
    sw.start_threshold = start_threshold;
    sw.stop_threshold = buffer_frames;
    sw.boundary = buffer_frames * 2u;
    sw.proto = SNDRV_PCM_VERSION;
    if (ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) < 0) {
        return -1;
    }
    if (ioctl(fd, SNDRV_PCM_IOCTL_PREPARE, 0) < 0) {
        return -1;
    }
    *period_out = period_frames;
    return 0;
}

static int write_pcm(int fd, const short *samples, unsigned int frames,
                     unsigned int channels)
{
    unsigned int written = 0;
    while (written < frames) {
        struct smp_xferi transfer;
        int result;
        transfer.buf = (void *)(samples + written * channels);
        transfer.frames = frames - written;
        transfer.result = 0;
        result = ioctl(fd, SNDRV_PCM_IOCTL_WRITEI_FRAMES, &transfer);
        if (result < 0) {
            if (errno == EINTR) {
                continue;
            }
            if (errno == EPIPE) {
                if (ioctl(fd, SNDRV_PCM_IOCTL_PREPARE, 0) < 0) {
                    return -1;
                }
                continue;
            }
            return -1;
        }
        if (transfer.result <= 0) {
            return -1;
        }
        written += (unsigned int)transfer.result;
    }
    return 0;
}

static int parse_int_arg(const char *s, int *out)
{
    int val = 0;
    int seen = 0;
    while (*s >= '0' && *s <= '9') {
        val = val * 10 + (*s - '0');
        seen = 1;
        s++;
    }
    if (!seen || *s != 0) {
        return -1;
    }
    *out = val;
    return 0;
}

int main(int argc, char **argv)
{
    const char *path = 0;
    int scale_pct = 100;
    int i;
    int fd;
    struct stat info;
    unsigned char *file_buf;
    unsigned long size;
    unsigned long done = 0;
    unsigned long pos;
    unsigned int sample_rate = 0;
    unsigned short channels = 0;
    unsigned short bits = 0;
    unsigned short audio_fmt = 0;
    const unsigned char *pcm_ptr = 0;
    unsigned int pcm_bytes = 0;
    unsigned int frames;
    unsigned int pad_frames;
    unsigned int total_frames;
    unsigned int period_frames = PERIOD_FRAMES;
    int card;
    char device[64];
    int pcm_fd;
    short *samples;
    int gain_q15;

    for (i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--scale") == 0 && i + 1 < argc) {
            int parsed = 100;
            if (parse_int_arg(argv[++i], &parsed) == 0) {
                if (parsed < 0) parsed = 0;
                if (parsed > 100) parsed = 100;
                scale_pct = parsed;
            }
        } else if (argv[i][0] != '-') {
            path = argv[i];
        }
    }

    if (path == 0 || scale_pct == 0) {
        return path == 0 ? 1 : 0;
    }

    fd = open(path, O_RDONLY);
    if (fd < 0) {
        return 1;
    }
    if (fstat(fd, &info) != 0 || info.st_size < 44 || info.st_size > 8 * 1024 * 1024) {
        close(fd);
        return 1;
    }
    size = (unsigned long)info.st_size;
    file_buf = (unsigned char *)malloc(size);
    if (file_buf == 0) {
        close(fd);
        return 1;
    }
    while (done < size) {
        long got = read(fd, file_buf + done, size - done);
        if (got <= 0) {
            free(file_buf);
            close(fd);
            return 1;
        }
        done += (unsigned long)got;
    }
    close(fd);

    if (memcmp(file_buf, "RIFF", 4) != 0 || memcmp(file_buf + 8, "WAVE", 4) != 0) {
        free(file_buf);
        return 1;
    }

    pos = 12;
    while (pos + 8 <= size) {
        unsigned int chunk_len = read_u32_le(file_buf + pos + 4);
        if (pos + 8 + chunk_len > size) {
            chunk_len = (unsigned int)(size - (pos + 8));
        }
        if (memcmp(file_buf + pos, "fmt ", 4) == 0 && chunk_len >= 16) {
            const unsigned char *f = file_buf + pos + 8;
            audio_fmt = read_u16_le(f);
            channels = read_u16_le(f + 2);
            sample_rate = read_u32_le(f + 4);
            bits = read_u16_le(f + 14);
        } else if (memcmp(file_buf + pos, "data", 4) == 0) {
            pcm_ptr = file_buf + pos + 8;
            pcm_bytes = chunk_len;
        }
        pos += 8 + chunk_len + (chunk_len & 1u);
    }

    if (audio_fmt != 1 || bits != 16 || channels == 0 || channels > 2 ||
        sample_rate == 0 || pcm_ptr == 0 || pcm_bytes < (unsigned int)(channels * 2)) {
        free(file_buf);
        return 1;
    }

    card = choose_card();
    if (card < 0) {
        free(file_buf);
        return 0;
    }
    snprintf(device, sizeof(device), "/dev/snd/pcmC%dD0p", card);
    pcm_fd = open(device, O_RDWR);
    if (pcm_fd < 0) {
        /* Fail quietly when playback device is busy (e.g. music player active). */
        free(file_buf);
        return 0;
    }

    if (configure_pcm(pcm_fd, sample_rate, channels, &period_frames) != 0) {
        close(pcm_fd);
        free(file_buf);
        return 1;
    }

    frames = pcm_bytes / (channels * 2u);
    /* Align to period boundary + add ~83 ms silent tail so short SFX drain cleanly through the mixer. */
    pad_frames = (sample_rate / 12u);
    total_frames = ((frames + pad_frames + period_frames - 1u) / period_frames) * period_frames;
    samples = (short *)malloc((unsigned long)total_frames * channels * sizeof(short));
    if (samples == 0) {
        close(pcm_fd);
        free(file_buf);
        return 1;
    }
    memset(samples, 0, (unsigned long)total_frames * channels * sizeof(short));

    /* Apply soft perceptual scaling (cubic-like via squared percentage) when < 100%. */
    if (scale_pct >= 100) {
        memcpy(samples, pcm_ptr, frames * channels * 2u);
    } else {
        unsigned int total_samples = frames * channels;
        unsigned int s_idx;
        long linear = (long)scale_pct * (long)scale_pct;
        gain_q15 = (int)((linear * 32768L + 5000L) / 10000L);
        for (s_idx = 0; s_idx < total_samples; s_idx++) {
            const unsigned char *sp = pcm_ptr + s_idx * 2u;
            short sample = (short)((unsigned short)sp[0] | ((unsigned short)sp[1] << 8));
            int scaled = ((int)sample * gain_q15) >> 15;
            samples[s_idx] = (short)scaled;
        }
    }

    if (write_pcm(pcm_fd, samples, total_frames, channels) == 0) {
        (void)ioctl(pcm_fd, SNDRV_PCM_IOCTL_START, 0);
        (void)ioctl(pcm_fd, SNDRV_PCM_IOCTL_DRAIN, 0);
    }

    free(samples);
    close(pcm_fd);
    free(file_buf);
    return 0;
}
