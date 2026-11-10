/*
 *
 *       modules/include/spk/sound.h
 *       Linux-compatible ALSA driver API for SpaceKernel modules
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#ifndef SPK_SOUND_H
#define SPK_SOUND_H

#ifdef __cplusplus
extern "C" {
#endif

/*
 * The userspace ABI is the Linux ALSA UAPI: /dev/snd nodes under major 116,
 * the raw 'A'/'U' ioctl words and the byte layouts below.  A module only sees
 * this driver-facing subset; all lifetime and locking rules follow
 * sound/core/pcm.c and sound/core/control.c.
 */

typedef unsigned long snd_pcm_uframes_t;
typedef signed long snd_pcm_sframes_t;

#define SNDRV_PCM_STREAM_PLAYBACK 0
#define SNDRV_PCM_STREAM_CAPTURE 1

#define SNDRV_PCM_ACCESS_MMAP_INTERLEAVED 0
#define SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED 1
#define SNDRV_PCM_ACCESS_MMAP_COMPLEX 2
#define SNDRV_PCM_ACCESS_RW_INTERLEAVED 3
#define SNDRV_PCM_ACCESS_RW_NONINTERLEAVED 4

#define SNDRV_PCM_FORMAT_S8 0
#define SNDRV_PCM_FORMAT_U8 1
#define SNDRV_PCM_FORMAT_S16_LE 2
#define SNDRV_PCM_FORMAT_S16_BE 3
#define SNDRV_PCM_FORMAT_U16_LE 4
#define SNDRV_PCM_FORMAT_U16_BE 5
#define SNDRV_PCM_FORMAT_S24_LE 6
#define SNDRV_PCM_FORMAT_S24_BE 7
#define SNDRV_PCM_FORMAT_U24_LE 8
#define SNDRV_PCM_FORMAT_U24_BE 9
#define SNDRV_PCM_FORMAT_S32_LE 10
#define SNDRV_PCM_FORMAT_S32_BE 11
#define SNDRV_PCM_FORMAT_U32_LE 12
#define SNDRV_PCM_FORMAT_U32_BE 13
#define SNDRV_PCM_FORMAT_FLOAT_LE 14
#define SNDRV_PCM_FORMAT_FLOAT_BE 15
#define SNDRV_PCM_FORMAT_FLOAT64_LE 16
#define SNDRV_PCM_FORMAT_FLOAT64_BE 17
#define SNDRV_PCM_FORMAT_S24_3LE 32
#define SNDRV_PCM_FORMAT_S24_3BE 33

#define SNDRV_PCM_HW_PARAM_ACCESS 0
#define SNDRV_PCM_HW_PARAM_FORMAT 1
#define SNDRV_PCM_HW_PARAM_SUBFORMAT 2
#define SNDRV_PCM_HW_PARAM_SAMPLE_BITS 8
#define SNDRV_PCM_HW_PARAM_FRAME_BITS 9
#define SNDRV_PCM_HW_PARAM_CHANNELS 10
#define SNDRV_PCM_HW_PARAM_RATE 11
#define SNDRV_PCM_HW_PARAM_PERIOD_TIME 12
#define SNDRV_PCM_HW_PARAM_PERIOD_SIZE 13
#define SNDRV_PCM_HW_PARAM_PERIOD_BYTES 14
#define SNDRV_PCM_HW_PARAM_PERIODS 15
#define SNDRV_PCM_HW_PARAM_BUFFER_TIME 16
#define SNDRV_PCM_HW_PARAM_BUFFER_SIZE 17
#define SNDRV_PCM_HW_PARAM_BUFFER_BYTES 18
#define SNDRV_PCM_HW_PARAM_TICK_TIME 19

#define SNDRV_PCM_TRIGGER_STOP 0
#define SNDRV_PCM_TRIGGER_START 1
#define SNDRV_PCM_TRIGGER_PAUSE_PUSH 2
#define SNDRV_PCM_TRIGGER_PAUSE_RELEASE 3
#define SNDRV_PCM_TRIGGER_SUSPEND 4
#define SNDRV_PCM_TRIGGER_RESUME 5
#define SNDRV_PCM_TRIGGER_DRAIN 6

#define SNDRV_PCM_STATE_OPEN 0
#define SNDRV_PCM_STATE_SETUP 1
#define SNDRV_PCM_STATE_PREPARED 2
#define SNDRV_PCM_STATE_RUNNING 3
#define SNDRV_PCM_STATE_XRUN 4
#define SNDRV_PCM_STATE_DRAINING 5
#define SNDRV_PCM_STATE_PAUSED 6
#define SNDRV_PCM_STATE_SUSPENDED 7
#define SNDRV_PCM_STATE_DISCONNECTED 8

/* Control element numbering. */
#define SNDRV_CTL_ELEM_IFACE_CARD 0
#define SNDRV_CTL_ELEM_IFACE_HWDEP 1
#define SNDRV_CTL_ELEM_IFACE_MIXER 2
#define SNDRV_CTL_ELEM_IFACE_PCM 3
#define SNDRV_CTL_ELEM_IFACE_RAWMIDI 4
#define SNDRV_CTL_ELEM_IFACE_TIMER 5
#define SNDRV_CTL_ELEM_IFACE_SEQUENCER 6

#define SNDRV_CTL_ELEM_TYPE_NONE 0
#define SNDRV_CTL_ELEM_TYPE_BOOLEAN 1
#define SNDRV_CTL_ELEM_TYPE_INTEGER 2
#define SNDRV_CTL_ELEM_TYPE_ENUMERATED 3
#define SNDRV_CTL_ELEM_TYPE_BYTES 4
#define SNDRV_CTL_ELEM_TYPE_IEC958 5
#define SNDRV_CTL_ELEM_TYPE_INTEGER64 6

#define SNDRV_CTL_ELEM_ACCESS_READ (1u << 0)
#define SNDRV_CTL_ELEM_ACCESS_WRITE (1u << 1)
#define SNDRV_CTL_ELEM_ACCESS_READWRITE 3u
#define SNDRV_CTL_ELEM_ACCESS_VOLATILE (1u << 2)
#define SNDRV_CTL_ELEM_ACCESS_TLV_READ (1u << 4)
#define SNDRV_CTL_ELEM_ACCESS_TLV_WRITE (1u << 5)
#define SNDRV_CTL_ELEM_ACCESS_TLV_COMMAND (1u << 6)
#define SNDRV_CTL_ELEM_ACCESS_INACTIVE (1u << 8)

#define SNDRV_CTL_ELEM_ID_NAME_MAXLEN 44

#define SNDRV_CTL_EVENT_MASK_VALUE (1u << 0)
#define SNDRV_CTL_EVENT_MASK_INFO (1u << 1)
#define SNDRV_CTL_EVENT_MASK_ADD (1u << 2)
#define SNDRV_CTL_EVENT_MASK_TLV (1u << 3)
#define SNDRV_CTL_EVENT_MASK_REMOVE (~0u)

struct snd_mask {
    unsigned int bits[8];
};

struct snd_interval {
    unsigned int min;
    unsigned int max;
    unsigned int openmin : 1, openmax : 1, integer : 1, empty : 1;
};

struct snd_pcm_hw_params {
    unsigned int flags;
    struct snd_mask masks[3];
    struct snd_mask mres[5];
    struct snd_interval intervals[12];
    struct snd_interval ires[9];
    unsigned int rmask;
    unsigned int cmask;
    unsigned int info;
    unsigned int msbits;
    unsigned int rate_num;
    unsigned int rate_den;
    snd_pcm_uframes_t fifo_size;
    unsigned char sync[16];
    unsigned char reserved[48];
};

struct snd_pcm_sw_params {
    int tstamp_mode;
    unsigned int period_step;
    unsigned int sleep_min;
    snd_pcm_uframes_t avail_min;
    snd_pcm_uframes_t xfer_align;
    snd_pcm_uframes_t start_threshold;
    snd_pcm_uframes_t stop_threshold;
    snd_pcm_uframes_t silence_threshold;
    snd_pcm_uframes_t silence_size;
    snd_pcm_uframes_t boundary;
    unsigned int proto;
    unsigned int tstamp_type;
    unsigned char reserved[56];
};

struct snd_ctl_elem_id {
    unsigned int numid;
    unsigned int iface;
    unsigned int device;
    unsigned int subdevice;
    unsigned char name[SNDRV_CTL_ELEM_ID_NAME_MAXLEN];
    unsigned int index;
};

struct snd_ctl_elem_info {
    struct snd_ctl_elem_id id;
    unsigned int type;
    unsigned int access;
    unsigned int count;
    int owner;
    union {
        struct {
            long min;
            long max;
            long step;
        } integer;
        struct {
            long long min;
            long long max;
            long long step;
        } integer64;
        struct {
            unsigned int items;
            unsigned int item;
            char name[64];
            unsigned long long names_ptr;
            unsigned int names_length;
        } enumerated;
        unsigned char reserved[128];
    } value;
    unsigned char reserved[64];
};

struct snd_aes_iec958 {
    unsigned char status[24];
    unsigned char subcode[147];
    unsigned char pad;
    unsigned char dig_subframe[4];
};

struct snd_ctl_elem_value {
    struct snd_ctl_elem_id id;
    unsigned int indirect : 1;
    union {
        union {
            long value[128];
            long *value_ptr;
        } integer;
        union {
            long long value[64];
            long long *value_ptr;
        } integer64;
        union {
            unsigned int item[128];
            unsigned int *item_ptr;
        } enumerated;
        union {
            unsigned char data[512];
            unsigned char *data_ptr;
        } bytes;
        struct snd_aes_iec958 iec958;
    } value;
    unsigned char reserved[128];
};

struct snd_pcm_substream;

struct snd_pcm_ops {
    int (*open)(struct snd_pcm_substream *substream);
    int (*close)(struct snd_pcm_substream *substream);
    int (*ioctl)(struct snd_pcm_substream *substream, unsigned int cmd, void *arg);
    int (*hw_params)(struct snd_pcm_substream *substream,
                     struct snd_pcm_hw_params *params);
    int (*hw_free)(struct snd_pcm_substream *substream);
    int (*prepare)(struct snd_pcm_substream *substream);
    int (*trigger)(struct snd_pcm_substream *substream, int cmd);
    snd_pcm_uframes_t (*pointer)(struct snd_pcm_substream *substream);
};

/* Minimal kcontrol object; drivers only read private_data and id. */
struct snd_kcontrol {
    void *private_data;
    struct snd_ctl_elem_id id;
    unsigned int access;
    unsigned int count;
};

struct snd_kcontrol_new {
    int iface;
    unsigned int device;
    unsigned int subdevice;
    unsigned int index;
    const char *name;
    unsigned int access;
    int count;
    int (*info)(struct snd_kcontrol *kcontrol, struct snd_ctl_elem_info *uinfo);
    int (*get)(struct snd_kcontrol *kcontrol, struct snd_ctl_elem_value *ucontrol);
    int (*put)(struct snd_kcontrol *kcontrol, struct snd_ctl_elem_value *ucontrol);
    void *private_data;
};

struct snd_card;
struct snd_pcm;

/* ---- card ------------------------------------------------------------- */

int snd_card_new(int index, const char *id, const char *driver,
                 const char *shortname, const char *longname,
                 struct snd_card **card_out);
int snd_card_register(struct snd_card *card);
void snd_card_free(struct snd_card *card);

/* ---- PCM -------------------------------------------------------------- */

int snd_pcm_new(struct snd_card *card, const char *id, int device,
                int playback_count, int capture_count,
                struct snd_pcm **pcm_out);
int snd_pcm_set_ops(struct snd_pcm *pcm, int stream,
                    const struct snd_pcm_ops *ops);
void snd_pcm_period_elapsed(struct snd_pcm_substream *substream);
int snd_pcm_stop(struct snd_pcm_substream *substream, int state);
int snd_pcm_stop_xrun(struct snd_pcm_substream *substream);

int snd_pcm_hw_constraint_minmax(struct snd_pcm_substream *substream,
                                 unsigned int param, unsigned int min,
                                 unsigned int max);
int snd_pcm_hw_constraint_mask64(struct snd_pcm_substream *substream,
                                 unsigned int param, unsigned long long mask);
int snd_pcm_set_formats(struct snd_pcm_substream *substream,
                        unsigned long long formats);
int snd_pcm_lib_malloc_pages(struct snd_pcm_substream *substream,
                             unsigned long size);
int snd_pcm_lib_free_pages(struct snd_pcm_substream *substream);
unsigned long long snd_pcm_get_dma_addr(struct snd_pcm_substream *substream);
void *snd_pcm_get_buffer(struct snd_pcm_substream *substream);
snd_pcm_uframes_t snd_pcm_get_period_size(struct snd_pcm_substream *substream);
snd_pcm_uframes_t snd_pcm_get_buffer_size(struct snd_pcm_substream *substream);
unsigned int snd_pcm_get_rate(struct snd_pcm_substream *substream);
unsigned int snd_pcm_get_channels(struct snd_pcm_substream *substream);
unsigned int snd_pcm_get_format(struct snd_pcm_substream *substream);

/* ---- control ---------------------------------------------------------- */

struct snd_kcontrol *snd_ctl_new1(const struct snd_kcontrol_new *template,
                                  void *private_data);
int snd_ctl_add(struct snd_card *card, struct snd_kcontrol *kcontrol);
void snd_ctl_notify(struct snd_card *card, unsigned int mask,
                    const struct snd_ctl_elem_id *id);
void snd_ctl_free_one(struct snd_kcontrol *kcontrol);

#ifdef __cplusplus
}
#endif

#endif /* SPK_SOUND_H */
