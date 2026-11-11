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
#define SPK_MODULE_ABI_VERSION 2u

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

/* ---- PCI ---------------------------------------------------------------
 * Device IDs are matched against vendor/device and optionally class/class_mask.
 * The id table is terminated by an entry whose vendor and device are both 0.
 */
struct spk_pci_device;

struct spk_pci_device_id {
    unsigned int vendor;
    unsigned int device;
    unsigned int subvendor;
    unsigned int subdevice;
    unsigned int class;
    unsigned int class_mask;
    unsigned long driver_data;
};

struct spk_pci_driver {
    const char *name;
    const struct spk_pci_device_id *id_table;
    int (*probe)(struct spk_pci_device *device);
    void (*remove)(struct spk_pci_device *device);
};

int spk_pci_driver_register(const struct spk_pci_driver *driver);
void spk_pci_driver_unregister(const struct spk_pci_driver *driver);
int spk_pci_read_config_byte(struct spk_pci_device *device, int offset);
int spk_pci_read_config_word(struct spk_pci_device *device, int offset);
int spk_pci_read_config_dword(struct spk_pci_device *device, int offset);
int spk_pci_write_config_byte(struct spk_pci_device *device, int offset, unsigned int value);
int spk_pci_write_config_word(struct spk_pci_device *device, int offset, unsigned int value);
int spk_pci_write_config_dword(struct spk_pci_device *device, int offset, unsigned int value);
int spk_pci_enable_device(struct spk_pci_device *device);
void spk_pci_set_master(struct spk_pci_device *device);
unsigned long long spk_pci_resource_start(struct spk_pci_device *device, int bar);
unsigned long spk_pci_resource_len(struct spk_pci_device *device, int bar);
int spk_pci_irq_line(struct spk_pci_device *device);

/* Message-signalled interrupts. The kernel allocates a vector, programmes the
 * capability (MSI) or the MSI-X table, and releases everything on disable.
 * Returns 0, or a negative errno. The handler runs in interrupt context. */
int spk_pci_enable_msi(struct spk_pci_device *device,
                       void (*handler)(int irq, void *data), void *data,
                       unsigned int *message_address, unsigned int *message_data);
void spk_pci_disable_msi(struct spk_pci_device *device);
int spk_pci_enable_msix(struct spk_pci_device *device, unsigned int vectors,
                        void (*handler)(int irq, void *data), void *data);
void spk_pci_disable_msix(struct spk_pci_device *device);
int spk_request_irq(struct spk_pci_device *device,
                    void (*handler)(int irq, void *data), void *data);
void spk_free_irq(struct spk_pci_device *device);

/* ---- DMA and MMIO ------------------------------------------------------
 * Coherent memory is zeroed, page backed and at least `align`-byte aligned.
 * Streaming mappings require a physically contiguous, already mapped range.
 */
void *spk_dma_alloc_coherent(unsigned long size, unsigned long align,
                             unsigned long long *dma_address);
void spk_dma_free_coherent(void *address, unsigned long size);
unsigned long long spk_dma_map_single(void *address, unsigned long length);
void spk_dma_unmap_single(unsigned long long dma_address, unsigned long length);
void *spk_ioremap(unsigned long long physical, unsigned long length);
void spk_iounmap(void *address, unsigned long length);
unsigned int spk_readl(const volatile void *address);
void spk_writel(unsigned int value, volatile void *address);
unsigned short spk_readw(const volatile void *address);
void spk_writew(unsigned short value, volatile void *address);
unsigned char spk_readb(const volatile void *address);
/* ISA port I/O (x86 only; other architectures return 0 / ignore writes). */
unsigned char spk_inb(unsigned short port);
unsigned short spk_inw(unsigned short port);
unsigned int spk_inl(unsigned short port);
void spk_outb(unsigned short port, unsigned char value);
void spk_outw(unsigned short port, unsigned short value);
void spk_outl(unsigned short port, unsigned int value);
/* Legacy ISA IRQ routing (x86 only). */
int spk_request_isa_irq(unsigned int irq, void (*handler)(int irq, void *data), void *data);
void spk_free_isa_irq(unsigned int irq);
void spk_writeb(unsigned char value, volatile void *address);
void spk_mb(void);

/* ---- Synchronisation and deferred work --------------------------------- */
typedef struct {
    volatile unsigned int locked;
} spk_spinlock;

void spk_spin_lock(spk_spinlock *lock);
void spk_spin_unlock(spk_spinlock *lock);
unsigned long spk_spin_lock_irqsave(spk_spinlock *lock);
void spk_spin_unlock_irqrestore(spk_spinlock *lock, unsigned long flags);

/* Callback timers run in process context on the kernel timer thread and may
 * sleep. spk_timer_del() returns 1 when pending work was removed. */
unsigned long long spk_timer_add(unsigned long long delay_us,
                                 void (*callback)(void *data), void *data);
int spk_timer_del(unsigned long long id);
int spk_work_schedule(void (*callback)(void *data), void *data);

/* Create a kernel thread running fn(arg). Returns its tid, or -1. */
long spk_kthread_create(const char *name, int (*fn)(void *), void *arg);

/* ---- Network buffers and devices --------------------------------------- */
struct spk_skb;
struct spk_netdev;
struct spk_napi;

struct spk_net_ops {
    int (*open)(struct spk_netdev *device);
    int (*stop)(struct spk_netdev *device);
    /* Return 0 when the callee consumed the skb, 1 when the queue is busy
     * (the caller retains ownership after calling spk_netif_stop_queue). */
    int (*start_xmit)(struct spk_netdev *device, struct spk_skb *skb);
    void (*set_rx_mode)(struct spk_netdev *device);
    int (*set_mac_address)(struct spk_netdev *device, const unsigned char *address);
    int (*change_mtu)(struct spk_netdev *device, unsigned int mtu);
    void (*tx_timeout)(struct spk_netdev *device);
};

struct spk_netdev *spk_netdev_alloc(const char *name, const unsigned char *address,
                                    unsigned int address_len, void *priv,
                                    const struct spk_net_ops *ops);
int spk_netdev_register(struct spk_netdev *device);
void spk_netdev_unregister(struct spk_netdev *device);
void spk_netdev_free(struct spk_netdev *device);
void *spk_netdev_priv(struct spk_netdev *device);
unsigned int spk_netdev_ifindex(struct spk_netdev *device);
unsigned int spk_netdev_mtu(struct spk_netdev *device);
unsigned int spk_netdev_get_flags(struct spk_netdev *device);
int spk_netdev_name(struct spk_netdev *device, char *buffer, unsigned int length);
int spk_netdev_get_address(struct spk_netdev *device, unsigned char *buffer,
                           unsigned int length);

struct spk_skb *spk_skb_alloc(unsigned int headroom, unsigned int capacity);
unsigned char *spk_skb_put(struct spk_skb *skb, unsigned int length);
unsigned char *spk_skb_push(struct spk_skb *skb, unsigned int length);
unsigned char *spk_skb_pull(struct spk_skb *skb, unsigned int length);
void spk_skb_reserve(struct spk_skb *skb, unsigned int length);
void spk_skb_trim(struct spk_skb *skb, unsigned int length);
unsigned int spk_skb_len(const struct spk_skb *skb);
unsigned char *spk_skb_data(const struct spk_skb *skb);
void spk_skb_set_protocol(struct spk_skb *skb, unsigned short protocol);
void spk_skb_free(struct spk_skb *skb);
unsigned short spk_eth_type_trans(struct spk_skb *skb, struct spk_netdev *device);

struct spk_napi *spk_napi_add(struct spk_netdev *device,
                              int (*poll)(struct spk_napi *, int budget),
                              int weight);
void spk_napi_del(struct spk_netdev *device, struct spk_napi *napi);
void spk_napi_schedule(struct spk_napi *napi);
void spk_napi_complete(struct spk_napi *napi);
void spk_napi_enable(struct spk_napi *napi);
void spk_napi_disable(struct spk_napi *napi);

void spk_netif_rx(struct spk_netdev *device, struct spk_skb *skb);
void spk_netif_carrier_on(struct spk_netdev *device);
void spk_netif_carrier_off(struct spk_netdev *device);
void spk_netif_stop_queue(struct spk_netdev *device);
void spk_netif_wake_queue(struct spk_netdev *device);

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
