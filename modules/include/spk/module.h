/*
 *
 *       modules/include/spk/module.h
 *       Stable C ABI header for SpaceKernel loadable modules
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#ifndef SPK_MODULE_H
#define SPK_MODULE_H

#ifdef __cplusplus
extern "C" {
#endif

#define SPK_MODULE_MAGIC 0x4D4B5053u /* "SPKM" little endian */
#define SPK_MODULE_ABI_VERSION 1u

/*
 * The build system must pass -DSPK_VERMAGIC="\"<release> <arch> SMP\"".
 * A module built outside the tree with a mismatched string is rejected with
 * ENOEXEC by finit_module/init_module.
 */
#ifndef SPK_VERMAGIC
#define SPK_VERMAGIC "3.0-spk unknown SMP"
#endif

struct spk_module {
    unsigned int magic;
    unsigned int abi_version;
    const char *name;
    const char *version;
    const char *license;
    const char *vermagic;
    int (*init)(void);
    void (*exit)(void);
};

/* Logging. spk_printk follows C printf conversions (%s %c %d %i %u %x %X %p
 * %%), including width and zero padding. spk_pr_* take a plain string. */
void spk_printk(const char *format, ...);
void spk_pr_info(const char *message);
void spk_pr_warn(const char *message);
void spk_pr_err(const char *message);

/* Kernel heap. Pointers are 16-byte aligned and must be released with
 * spk_kfree() or spk_krealloc(). */
void *spk_kmalloc(unsigned long size);
void *spk_kzalloc(unsigned long size);
void *spk_krealloc(void *pointer, unsigned long size);
void spk_kfree(void *pointer);

/* Time. spk_msleep() sleeps in process context and busy-waits early in boot. */
unsigned long long spk_uptime_us(void);
unsigned long long spk_uptime_ms(void);
void spk_msleep(unsigned long milliseconds);
void spk_udelay(unsigned long microseconds);

/* Identity of the kernel and of the module currently running init/exit. */
const char *spk_kernel_version(void);
const char *spk_module_name(void);
unsigned int spk_getpid(void);
unsigned int spk_get_cpu(void);

/* Every C freestanding compiler may emit these; the kernel exports them. */
void *memcpy(void *destination, const void *source, unsigned long count);
void *memmove(void *destination, const void *source, unsigned long count);
void *memset(void *destination, int value, unsigned long count);
int memcmp(const void *left, const void *right, unsigned long count);

/*
 * Place exactly one metadata record in .spk_module. The loader reads it after
 * relocations and before calling init.
 */
#define SPK_MODULE(_name, _version, _license, _initfn, _exitfn)                 \
    static const struct spk_module spk_module_metadata                          \
        __attribute__((used, section(".spk_module"), aligned(8))) = {          \
            SPK_MODULE_MAGIC,                                                  \
            SPK_MODULE_ABI_VERSION,                                            \
            (_name),                                                           \
            (_version),                                                        \
            (_license),                                                        \
            SPK_VERMAGIC,                                                      \
            (_initfn),                                                         \
            (_exitfn)                                                          \
    }

#endif /* SPK_MODULE_H */
