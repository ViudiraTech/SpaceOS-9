/*
 *       user/bin/alsa_pcm.h
 *       Minimal Linux ALSA PCM UAPI subset for SpaceOS-9 userland
 *
 *       2026/11/11 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
#ifndef SPACEOS_ALSA_PCM_H
#define SPACEOS_ALSA_PCM_H

#define SNDRV_PCM_ACCESS_MMAP_INTERLEAVED 0
#define SNDRV_PCM_ACCESS_RW_INTERLEAVED 3
#define SNDRV_PCM_FORMAT_S16_LE 2
#define SNDRV_PCM_SUBFORMAT_STD 0

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

struct smp_mask {
    unsigned int bits[8];
};

struct smp_interval {
    unsigned int min;
    unsigned int max;
    unsigned int flags;
};

struct smp_hw_params {
    unsigned int flags;
    struct smp_mask masks[3];
    struct smp_mask mres[5];
    struct smp_interval intervals[12];
    struct smp_interval ires[9];
    unsigned int rmask;
    unsigned int cmask;
    unsigned int info;
    unsigned int msbits;
    unsigned int rate_num;
    unsigned int rate_den;
    unsigned long fifo_size;
    unsigned char sync[16];
    unsigned char reserved[48];
};

struct smp_sw_params {
    int tstamp_mode;
    unsigned int period_step;
    unsigned int sleep_min;
    unsigned long avail_min;
    unsigned long xfer_align;
    unsigned long start_threshold;
    unsigned long stop_threshold;
    unsigned long silence_threshold;
    unsigned long silence_size;
    unsigned long boundary;
    unsigned int proto;
    unsigned int tstamp_type;
    unsigned char reserved[56];
};

struct smp_xferi {
    long result;
    void *buf;
    unsigned long frames;
};

struct smp_status {
    int state;
    unsigned char pad1[4];
    unsigned char trigger_tstamp[16];
    unsigned char tstamp[16];
    unsigned long appl_ptr;
    unsigned long hw_ptr;
    long delay;
    unsigned long avail;
    unsigned long avail_max;
    unsigned long overrange;
    int suspended_state;
    unsigned int audio_tstamp_data;
    unsigned char audio_tstamp[16];
    unsigned char driver_tstamp[16];
    unsigned int audio_tstamp_accuracy;
    unsigned char reserved[20];
};

#define SMP_IOC_NRBITS 8
#define SMP_IOC_TYPEBITS 8
#define SMP_IOC_SIZEBITS 14
#define SMP_IOC_DIRBITS 2
#define SMP_IOC_NRSHIFT 0
#define SMP_IOC_TYPESHIFT 8
#define SMP_IOC_SIZESHIFT 16
#define SMP_IOC_DIRSHIFT 30
#define SMP_IOC_NONE 0
#define SMP_IOC_WRITE 1
#define SMP_IOC_READ 2
#define SMP_IOC(dir, type, nr, size) \
    (((dir) << SMP_IOC_DIRSHIFT) | ((type) << SMP_IOC_TYPESHIFT) | \
     ((nr) << SMP_IOC_NRSHIFT) | ((size) << SMP_IOC_SIZESHIFT))
#define SMP_IO(type, nr) SMP_IOC(SMP_IOC_NONE, type, nr, 0)
#define SMP_IOR(type, nr, size) SMP_IOC(SMP_IOC_READ, type, nr, size)
#define SMP_IOW(type, nr, size) SMP_IOC(SMP_IOC_WRITE, type, nr, size)
#define SMP_IOWR(type, nr, size) SMP_IOC(SMP_IOC_READ | SMP_IOC_WRITE, type, nr, size)

#define SNDRV_PCM_IOCTL_HW_PARAMS SMP_IOWR('A', 0x11, sizeof(struct smp_hw_params))
#define SNDRV_PCM_IOCTL_HW_FREE SMP_IO('A', 0x12)
#define SNDRV_PCM_IOCTL_SW_PARAMS SMP_IOWR('A', 0x13, sizeof(struct smp_sw_params))
#define SNDRV_PCM_IOCTL_STATUS SMP_IOR('A', 0x20, sizeof(struct smp_status))
#define SNDRV_PCM_IOCTL_PREPARE SMP_IO('A', 0x40)
#define SNDRV_PCM_IOCTL_START SMP_IO('A', 0x42)
#define SNDRV_PCM_IOCTL_DRAIN SMP_IO('A', 0x44)
#define SNDRV_PCM_IOCTL_WRITEI_FRAMES SMP_IOW('A', 0x50, sizeof(struct smp_xferi))
#define SNDRV_PCM_VERSION 0x00020012u

_Static_assert(sizeof(struct smp_hw_params) == 608, "ALSA hw_params layout");
_Static_assert(sizeof(struct smp_sw_params) == 136, "ALSA sw_params layout");
_Static_assert(sizeof(struct smp_xferi) == 24, "ALSA xferi layout");
_Static_assert(sizeof(struct smp_status) == 152, "ALSA status layout");

#endif
