/*
 *       user/bin/mp3play.c
 *       SpaceOS-9 MP3 music player
 *
 *       2026/11/11 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 * MP3 decoding is provided by minimp3 (CC0, https://github.com/lieff/minimp3).
 * Audio is written through the Linux ALSA PCM ABI implemented by the kernel
 * sound core; the default device is chosen from /proc/asound/cards, preferring
 * the Intel HDA card.  `--decode` verifies the decoder without touching audio
 * hardware.
 *
 * Control interface used by the desktop music player:
 *   SIGUSR1          toggle pause/resume (SNDRV_PCM_IOCTL_PAUSE, position kept)
 *   SIGTERM/SIGINT   stop immediately (SNDRV_PCM_IOCTL_DROP) and exit 0
 *   --start-ms N     begin playback N milliseconds into the track (seek)
 *   --info           print "duration_ms=<n> rate=<hz> channels=<n>" and exit
 */

#include "splibc.h"
#include "alsa_pcm.h"
#include "minimp3.h"

#define DEFAULT_TRACK "/usr/share/music/spaceos-theme.mp3"
#define PERIOD_FRAMES 1024u
#define PERIODS 4u
#define BUFFER_FRAMES (PERIOD_FRAMES * PERIODS)

#ifndef EBADFD
#define EBADFD 77
#endif
#ifndef ESTRPIPE
#define ESTRPIPE 86
#endif

/* Written only from signal handlers, read from the main loop. */
static volatile int g_quit;
static volatile unsigned int g_pause_requests;

static void on_quit_signal(int sig)
{
    (void)sig;
    g_quit = 1;
}

static void on_pause_signal(int sig)
{
    (void)sig;
    g_pause_requests++;
}

static void install_signal_handlers(void)
{
    struct sigaction action;

    /* No SA_RESTART: a blocked WRITEI must return EINTR (or a short count)
     * so the main loop can act on pause/stop requests immediately. */
    memset(&action, 0, sizeof(action));
    action.sa_handler = on_quit_signal;
    sigemptyset(&action.sa_mask);
    (void)sigaction(SIGTERM, &action, 0);
    (void)sigaction(SIGINT, &action, 0);
    action.sa_handler = on_pause_signal;
    (void)sigaction(SIGUSR1, &action, 0);
    action.sa_handler = SIG_IGN;
    (void)sigaction(SIGPIPE, &action, 0);
}

static void sleep_ms(unsigned int ms)
{
    struct timespec request;
    request.tv_sec = ms / 1000u;
    request.tv_nsec = (long)(ms % 1000u) * 1000000L;
    (void)nanosleep(&request, 0);
}

static unsigned long parse_ulong(const char *text)
{
    unsigned long value = 0;
    while (*text >= '0' && *text <= '9') {
        value = value * 10u + (unsigned long)(*text - '0');
        text++;
    }
    return value;
}

static unsigned long string_length(const char *text)
{
    unsigned long length = 0;
    while (text[length] != 0) {
        length++;
    }
    return length;
}

static int string_contains(const char *haystack, const char *needle)
{
    unsigned long i;
    unsigned long n = string_length(needle);
    if (n == 0) {
        return 1;
    }
    for (i = 0; haystack[i] != 0; i++) {
        unsigned long j = 0;
        while (j < n && haystack[i + j] != 0 && haystack[i + j] == needle[j]) {
            j++;
        }
        if (j == n) {
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
        if (card < 0) {
            continue;
        }
        if (first < 0) {
            first = card;
        }
        if (string_contains(line, "HDA")) {
            hda = card;
        } else if (string_contains(line, "SB16")) {
            sb = card;
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

static int open_playback(void)
{
    char path[64];
    int card = choose_card();
    int fd;
    int index = 0;

    if (card >= 0) {
        snprintf(path, sizeof(path), "/dev/snd/pcmC%dD0p", card);
        fd = open(path, O_RDWR);
        if (fd >= 0) {
            printf("mp3play: using %s\n", path);
            return fd;
        }
    }
    for (index = 0; index < 8; index++) {
        snprintf(path, sizeof(path), "/dev/snd/pcmC%dD0p", index);
        fd = open(path, O_RDWR);
        if (fd >= 0) {
            printf("mp3play: using %s\n", path);
            return fd;
        }
    }
    return -1;
}

static int try_hw_params(int fd, unsigned int rate, unsigned int channels,
                         unsigned int period_frames, unsigned int periods)
{
    struct smp_hw_params hw;
    unsigned int buffer_frames = period_frames * periods;
    unsigned int index;

    memset(&hw, 0, sizeof(hw));
    hw.masks[SNDRV_PCM_HW_PARAM_ACCESS].bits[0] = 1u << SNDRV_PCM_ACCESS_RW_INTERLEAVED;
    hw.masks[SNDRV_PCM_HW_PARAM_FORMAT].bits[0] = 1u << SNDRV_PCM_FORMAT_S16_LE;
    hw.masks[SNDRV_PCM_HW_PARAM_SUBFORMAT].bits[0] = 1u << SNDRV_PCM_SUBFORMAT_STD;
    index = SNDRV_PCM_HW_PARAM_CHANNELS - SNDRV_PCM_HW_PARAM_SAMPLE_BITS;
    hw.intervals[index].min = channels;
    hw.intervals[index].max = channels;
    hw.intervals[index].flags = 4;
    index = SNDRV_PCM_HW_PARAM_RATE - SNDRV_PCM_HW_PARAM_SAMPLE_BITS;
    hw.intervals[index].min = rate;
    hw.intervals[index].max = rate;
    hw.intervals[index].flags = 4;
    index = SNDRV_PCM_HW_PARAM_PERIOD_SIZE - SNDRV_PCM_HW_PARAM_SAMPLE_BITS;
    hw.intervals[index].min = period_frames;
    hw.intervals[index].max = period_frames;
    hw.intervals[index].flags = 4;
    index = SNDRV_PCM_HW_PARAM_PERIODS - SNDRV_PCM_HW_PARAM_SAMPLE_BITS;
    hw.intervals[index].min = periods;
    hw.intervals[index].max = periods;
    hw.intervals[index].flags = 4;
    index = SNDRV_PCM_HW_PARAM_BUFFER_SIZE - SNDRV_PCM_HW_PARAM_SAMPLE_BITS;
    hw.intervals[index].min = buffer_frames;
    hw.intervals[index].max = buffer_frames;
    hw.intervals[index].flags = 4;
    if (ioctl(fd, SNDRV_PCM_IOCTL_HW_PARAMS, &hw) < 0) {
        return -1;
    }
    return (int)period_frames;
}

static int configure_pcm(int fd, unsigned int rate, unsigned int channels)
{
    /* Prefer a ~370 ms client ring so UI or disk stalls never starve the
     * mixer; fall back to smaller rings for constrained drivers. */
    static const unsigned int attempts[][2] = {
        { 1024, 16 },
        { 2048, 8 },
        { 1024, 8 },
        { 4096, 4 },
        { 512, 16 },
        { PERIOD_FRAMES, PERIODS * 2 },
        { PERIOD_FRAMES, PERIODS },
    };
    struct smp_sw_params sw;
    unsigned int period_frames = 0;
    unsigned int periods = 0;
    unsigned int buffer_frames;
    unsigned int start_threshold;
    unsigned int index;

    for (index = 0; index < sizeof(attempts) / sizeof(attempts[0]); index++) {
        int result = try_hw_params(fd, rate, channels, attempts[index][0], attempts[index][1]);
        if (result > 0) {
            period_frames = (unsigned int)result;
            periods = attempts[index][1];
            break;
        }
    }
    if (period_frames == 0 || periods == 0) {
        printf("mp3play: HW_PARAMS rejected for %u Hz / %u channel(s)\n", rate, channels);
        return -1;
    }

    buffer_frames = period_frames * periods;
    start_threshold = period_frames * 4u;
    if (start_threshold > buffer_frames) {
        start_threshold = buffer_frames;
    }

    memset(&sw, 0, sizeof(sw));
    sw.avail_min = period_frames;
    sw.start_threshold = start_threshold;
    sw.stop_threshold = buffer_frames;
    /* 0 lets the kernel pick Linux's default_boundary(): the largest
     * power-of-two multiple of buffer_size below LONG_MAX, so appl_ptr and
     * hw_ptr never wrap during a track. */
    sw.boundary = 0;
    sw.proto = SNDRV_PCM_VERSION;
    if (ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) < 0) {
        printf("mp3play: SW_PARAMS rejected\n");
        return -1;
    }
    if (ioctl(fd, SNDRV_PCM_IOCTL_PREPARE, 0) < 0) {
        printf("mp3play: PREPARE rejected\n");
        return -1;
    }
    return 0;
}

/* Recover from XRUN (EPIPE) or suspend (ESTRPIPE) like snd_pcm_recover(). */
static int recover_pcm(int fd, int error)
{
    if (error == ESTRPIPE) {
        int tries;
        for (tries = 0; tries < 50; tries++) {
            if (ioctl(fd, SNDRV_PCM_IOCTL_RESUME, 0) == 0) {
                return 0;
            }
            if (errno != EAGAIN) {
                break;
            }
            sleep_ms(10);
        }
    }
    if (error == EPIPE || error == ESTRPIPE || error == EBADFD) {
        return ioctl(fd, SNDRV_PCM_IOCTL_PREPARE, 0) < 0 ? -1 : 0;
    }
    return -1;
}

/* Pause until SIGUSR1 arrives again or a stop is requested. Returns 1 when a
 * stop was requested while paused. */
static int handle_pause(int fd)
{
    int value = 1;
    int hw_paused = 0;

    if ((g_pause_requests & 1u) == 0) {
        return 0;
    }
    hw_paused = ioctl(fd, SNDRV_PCM_IOCTL_PAUSE, &value) == 0;
    printf("mp3play: paused\n");
    while (!g_quit && (g_pause_requests & 1u) != 0) {
        sleep_ms(20);
    }
    if (g_quit) {
        return 1;
    }
    if (hw_paused) {
        value = 0;
        if (ioctl(fd, SNDRV_PCM_IOCTL_PAUSE, &value) < 0) {
            /* Stream fell out of PAUSED (e.g. device reset): re-prepare; the
             * next write restarts it at start_threshold. */
            (void)recover_pcm(fd, EPIPE);
        }
    }
    printf("mp3play: resumed\n");
    return 0;
}

/* Returns 0 when every frame was queued, 1 when a stop was requested, -1 on
 * an unrecoverable device error. */
static int write_pcm(int fd, const short *samples, unsigned int frames,
                     unsigned int channels)
{
    unsigned int written = 0;
    while (written < frames) {
        struct smp_xferi transfer;
        int result;
        if (g_quit) {
            return 1;
        }
        if ((g_pause_requests & 1u) != 0) {
            if (handle_pause(fd) != 0) {
                return 1;
            }
            continue;
        }
        transfer.buf = (void *)(samples + written * channels);
        transfer.frames = frames - written;
        transfer.result = 0;
        result = ioctl(fd, SNDRV_PCM_IOCTL_WRITEI_FRAMES, &transfer);
        if (result < 0) {
            int error = errno;
            if (error == EINTR || error == EAGAIN) {
                continue;
            }
            if (recover_pcm(fd, error) == 0) {
                continue;
            }
            return -1;
        }
        if (transfer.result <= 0) {
            printf("mp3play: short PCM write result=%ld frames=%lu\n",
                   transfer.result, transfer.frames);
            return -1;
        }
        written += (unsigned int)transfer.result;
    }
    return 0;
}

static unsigned char *read_track(const char *path, unsigned long *size)
{
    struct stat info;
    int fd = open(path, O_RDONLY);
    unsigned char *data;
    unsigned long done = 0;
    if (fd < 0) {
        return 0;
    }
    if (fstat(fd, &info) != 0 || info.st_size <= 0 || info.st_size > (32 * 1024 * 1024)) {
        close(fd);
        return 0;
    }
    data = (unsigned char *)malloc((size_t)info.st_size);
    if (data == 0) {
        close(fd);
        return 0;
    }
    while (done < (unsigned long)info.st_size) {
        long got = read(fd, data + done, (unsigned long)info.st_size - done);
        if (got <= 0) {
            break;
        }
        done += (unsigned long)got;
    }
    close(fd);
    if (done == 0) {
        free(data);
        return 0;
    }
    *size = done;
    return data;
}

static unsigned long skip_id3(unsigned char *data, unsigned long size)
{
    if (size >= 10 && data[0] == 'I' && data[1] == 'D' && data[2] == '3') {
        unsigned long length = ((unsigned long)(data[6] & 0x7f) << 21) |
                               ((unsigned long)(data[7] & 0x7f) << 14) |
                               ((unsigned long)(data[8] & 0x7f) << 7) |
                               (unsigned long)(data[9] & 0x7f);
        if (10 + length < size) {
            return 10 + length;
        }
    }
    return 0;
}

/* Header-only scan (minimp3 with pcm == NULL) for duration reporting. */
static int print_info(const unsigned char *data, unsigned long size, unsigned long offset)
{
    mp3dec_t decoder;
    mp3dec_frame_info_t info;
    unsigned long long samples_total = 0;
    unsigned int rate = 0;
    unsigned int channels = 0;

    mp3dec_init(&decoder);
    while (offset < size) {
        int samples = mp3dec_decode_frame(&decoder, data + offset, (int)(size - offset), 0, &info);
        if (info.frame_bytes <= 0) {
            break;
        }
        offset += (unsigned long)info.frame_bytes;
        if (samples > 0) {
            samples_total += (unsigned long long)samples;
            if (rate == 0) {
                rate = (unsigned int)info.hz;
                channels = (unsigned int)info.channels;
            }
        }
    }
    if (rate == 0) {
        printf("mp3play: no MPEG audio frames\n");
        return 1;
    }
    printf("duration_ms=%lu rate=%u channels=%u\n",
           (unsigned long)(samples_total * 1000ull / rate), rate, channels);
    return 0;
}

int main(int argc, char **argv)
{
    const char *path = DEFAULT_TRACK;
    int decode_only = 0;
    int info_only = 0;
    int argument = 1;
    unsigned char *data;
    unsigned long size = 0;
    unsigned long offset;
    unsigned long start_ms = 0;
    unsigned long long skip_frames = 0;
    mp3dec_t decoder;
    mp3dec_frame_info_t info;
    short pcm[MINIMP3_MAX_SAMPLES_PER_FRAME];
    int pcm_fd = -1;
    unsigned int configured_rate = 0;
    unsigned int configured_channels = 0;
    unsigned long decoded_frames = 0;
    unsigned long last_report = 0;
    int status = 0;
    int stopped = 0;

    /* Install handlers before loading the track so an early SIGUSR1/SIGTERM
     * arriving right after exec cannot hit default signal disposition. */
    install_signal_handlers();

    while (argument < argc) {
        if (strcmp(argv[argument], "--decode") == 0) {
            decode_only = 1;
        } else if (strcmp(argv[argument], "--info") == 0) {
            info_only = 1;
        } else if (strcmp(argv[argument], "--start-ms") == 0 && argument + 1 < argc) {
            start_ms = parse_ulong(argv[++argument]);
        } else {
            path = argv[argument];
        }
        argument++;
    }
    if (g_quit) {
        return 0;
    }
    data = read_track(path, &size);
    if (data == 0) {
        if (g_quit) {
            return 0;
        }
        printf("mp3play: cannot read %s\n", path);
        return 1;
    }
    offset = skip_id3(data, size);
    if (info_only) {
        status = print_info(data, size, offset);
        free(data);
        return status;
    }
    mp3dec_init(&decoder);
    if (!decode_only) {
        pcm_fd = open_playback();
        if (pcm_fd < 0) {
            printf("mp3play: no ALSA playback device (use --decode to test decoding)\n");
            free(data);
            return 1;
        }
    }
    printf("mp3play: %s (%lu bytes)\n", path, size);

    /* Seek: skip whole frames by header only, keeping the last few frames
     * decoded so the Layer III bit reservoir is primed at the cut point. */
    if (start_ms > 0) {
        unsigned long long target = 0;
        unsigned long frame_offsets[4] = { 0, 0, 0, 0 };
        unsigned long long frame_starts[4] = { 0, 0, 0, 0 };
        unsigned long long position = 0;
        unsigned int count = 0;
        mp3dec_t probe;
        mp3dec_init(&probe);
        while (offset < size) {
            int samples = mp3dec_decode_frame(&probe, data + offset, (int)(size - offset), 0, &info);
            if (info.frame_bytes <= 0) {
                break;
            }
            if (target == 0 && info.hz > 0) {
                target = (unsigned long long)start_ms * (unsigned long long)info.hz / 1000ull;
            }
            if (samples > 0 && target != 0 && position + (unsigned long long)samples > target) {
                break;
            }
            frame_offsets[count % 4u] = offset;
            frame_starts[count % 4u] = position;
            count++;
            offset += (unsigned long)info.frame_bytes;
            if (samples > 0) {
                position += (unsigned long long)samples;
            }
        }
        if (count >= 3u) {
            /* Rewind three frames to warm the reservoir; their output is discarded. */
            unsigned int slot = (count - 3u) % 4u;
            offset = frame_offsets[slot];
            position = frame_starts[slot];
        }
        skip_frames = target > position ? target - position : 0;
        decoded_frames = (unsigned long)position;
        last_report = decoded_frames;
    }

    while (offset < size && !g_quit) {
        int samples = mp3dec_decode_frame(&decoder, data + offset,
                                          (int)(size - offset), pcm, &info);
        const short *frame_pcm = pcm;
        if (info.frame_bytes <= 0) {
            offset++;
            continue;
        }
        offset += (unsigned long)info.frame_bytes;
        if (samples <= 0) {
            continue;
        }
        if (configured_rate == 0) {
            configured_rate = (unsigned int)info.hz;
            configured_channels = (unsigned int)info.channels;
            printf("mp3play: %u Hz, %u channel(s), %u kbps\n",
                   configured_rate, configured_channels, (unsigned int)info.bitrate_kbps);
            if (!decode_only && configure_pcm(pcm_fd, configured_rate,
                                              configured_channels) != 0) {
                status = 1;
                break;
            }
        } else if ((unsigned int)info.hz != configured_rate ||
                   (unsigned int)info.channels != configured_channels) {
            printf("mp3play: stream format change is not supported\n");
            status = 1;
            break;
        }
        decoded_frames += (unsigned long)samples;
        if (skip_frames > 0) {
            if (skip_frames >= (unsigned long long)samples) {
                skip_frames -= (unsigned long long)samples;
                continue;
            }
            frame_pcm += skip_frames * configured_channels;
            samples -= (int)skip_frames;
            skip_frames = 0;
        }
        if (!decode_only) {
            int written_result = write_pcm(pcm_fd, frame_pcm, (unsigned int)samples,
                                           configured_channels);
            if (written_result > 0) {
                stopped = 1;
                break;
            }
            if (written_result < 0) {
                printf("mp3play: PCM write failed (errno %d)\n", errno);
                status = 1;
                break;
            }
        }
        if (!decode_only && decoded_frames - last_report >= (unsigned long)configured_rate) {
            unsigned long seconds = decoded_frames / configured_rate;
            printf("mp3play: %lu s\n", seconds);
            last_report = decoded_frames;
        }
    }
    if (g_quit) {
        stopped = 1;
    }

    if (pcm_fd >= 0) {
        if (stopped) {
            /* Stop immediately and discard queued audio (snd_pcm_drop). */
            (void)ioctl(pcm_fd, SNDRV_PCM_IOCTL_DROP, 0);
        } else if (status == 0) {
            int drained = 0;
            while (!drained) {
                if (ioctl(pcm_fd, SNDRV_PCM_IOCTL_DRAIN, 0) == 0) {
                    drained = 1;
                } else if (errno == EINTR && !g_quit) {
                    if ((g_pause_requests & 1u) != 0 && handle_pause(pcm_fd) != 0) {
                        stopped = 1;
                        break;
                    }
                } else {
                    break;
                }
            }
            if (!drained) {
                (void)ioctl(pcm_fd, SNDRV_PCM_IOCTL_DROP, 0);
            }
        }
        close(pcm_fd);
    }
    printf("mp3play: decoded %lu frames%s\n", decoded_frames,
           status == 0 ? (stopped ? " (stopped)" : "") : " (error)");
    free(data);
    return status;
}
