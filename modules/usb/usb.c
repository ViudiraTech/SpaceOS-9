/*
 *
 *       modules/usb/usb.c
 *       USB stack module: xHCI host controller, enumeration, HID boot
 *       keyboard/mouse and Bulk-Only mass storage
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

/*
 * The driver uses only the module HAL (modules/include/spk/module.h):
 * PCI/MSI/DMA/MMIO for the controller, completions and mutexes for waiting,
 * and the input and block device classes for what it finds.
 *
 * Everything that waits is interrupt driven: commands and transfers sleep on
 * a completion that the event-ring handler signals; port resets wait for the
 * port status change event. HID interrupt endpoints are re-armed directly
 * from the interrupt handler. Hot-plug runs on the kernel work queue.
 *
 * References: xHCI 1.2 (Intel), USB 2.0 ch. 9, HID 1.11 appendix B (boot
 * protocol), USB Mass Storage Bulk-Only Transport 1.0, SCSI SBC-3.
 */

#include <spk/module.h>

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;

#define EIO 5
#define ENOMEM 12
#define ENODEV 19
#define EINVAL 22
#define ETIMEDOUT 110

/* ---- xHCI register layout ------------------------------------------- */
#define CAP_CAPLENGTH 0x00
#define CAP_HCSPARAMS1 0x04
#define CAP_HCSPARAMS2 0x08
#define CAP_HCCPARAMS1 0x10
#define CAP_DBOFF 0x14
#define CAP_RTSOFF 0x18

#define OP_USBCMD 0x00
#define OP_USBSTS 0x04
#define OP_CRCR 0x18
#define OP_DCBAAP 0x30
#define OP_CONFIG 0x38
#define OP_PORTSC(n) (0x400 + 0x10 * ((n)-1))

#define CMD_RS (1u << 0)
#define CMD_HCRST (1u << 1)
#define CMD_INTE (1u << 2)
#define STS_HCH (1u << 0)
#define STS_EINT (1u << 3)
#define STS_CNR (1u << 11)

#define PORT_CCS (1u << 0)
#define PORT_PED (1u << 1)
#define PORT_PR (1u << 4)
#define PORT_CSC (1u << 17)
#define PORT_PEC (1u << 18)
#define PORT_WRC (1u << 19)
#define PORT_OCC (1u << 20)
#define PORT_PRC (1u << 21)
#define PORT_PLC (1u << 22)
#define PORT_CEC (1u << 23)
#define PORT_CHANGES (PORT_CSC | PORT_PEC | PORT_WRC | PORT_OCC | PORT_PRC | PORT_PLC | PORT_CEC)
/* Bits that keep their value when written back (Linux XHCI_PORT_RO|RWS). */
#define PORT_PRESERVE ((1u << 0) | (1u << 3) | (0xfu << 10) | (1u << 30) | (0xfu << 5) | \
                       (1u << 9) | (3u << 14) | (7u << 25))

#define IR_IMAN 0x20
#define IR_IMOD 0x24
#define IR_ERSTSZ 0x28
#define IR_ERSTBA 0x30
#define IR_ERDP 0x38
#define IMAN_IP (1u << 0)
#define IMAN_IE (1u << 1)
#define ERDP_EHB (1u << 3)

/* ---- TRBs ----------------------------------------------------------- */
#define TRB_NORMAL 1
#define TRB_SETUP 2
#define TRB_DATA 3
#define TRB_STATUS 4
#define TRB_LINK 6
#define TRB_ENABLE_SLOT 9
#define TRB_DISABLE_SLOT 10
#define TRB_ADDRESS_DEVICE 11
#define TRB_CONFIGURE_EP 12
#define TRB_EVALUATE_CONTEXT 13
#define TRB_TRANSFER_EVENT 32
#define TRB_COMMAND_COMPLETION 33
#define TRB_PORT_STATUS_CHANGE 34

#define TRB_CYCLE (1u << 0)
#define TRB_TC (1u << 1) /* link: toggle cycle */
#define TRB_ISP (1u << 2)
#define TRB_IOC (1u << 5)
#define TRB_IDT (1u << 6)
#define TRB_DIR_IN (1u << 16)
#define TRB_TYPE(t) ((u32)(t) << 10)

#define CC_SUCCESS 1
#define CC_SHORT_PACKET 13

#define RING_TRBS 256 /* one 4 KiB page; the last TRB is the link */
#define MAX_SLOTS 32
#define MAX_PORTS 32
#define BOUNCE_SIZE 65536

struct trb {
    u32 p0, p1, status, control;
};

struct ring {
    volatile struct trb *trbs;
    u64 dma;
    unsigned int enqueue;
    u32 cycle;
};

struct transfer {
    struct spk_completion *done;
    volatile int cc;
    volatile u32 residue;
};

struct endpoint {
    int active;
    unsigned int dci;
    unsigned int max_packet;
    struct ring ring;
    struct transfer xfer;
    struct device *owner;
    int hid; /* index into owner->hid, or -1 */
};

enum hid_kind { HID_NONE, HID_KEYBOARD, HID_MOUSE };

struct hid {
    enum hid_kind kind;
    struct endpoint *ep;
    u8 *buffer;
    u64 buffer_dma;
    u8 previous[8];
    struct spk_input_dev *input;
};

struct msc {
    int active;
    unsigned int interface;
    struct endpoint *in, *out;
    struct spk_mutex *lock;
    u8 *buffer; /* BOUNCE_SIZE, also used for CBW/CSW */
    u64 buffer_dma;
    u32 tag;
    u32 block_size;
    u64 blocks;
    struct spk_blkdev *blkdev;
};

struct device {
    int present;
    unsigned int slot, port, speed;
    u16 vendor, product;
    void *out_ctx, *in_ctx;
    u64 out_ctx_dma, in_ctx_dma;
    u8 *control_buffer; /* 4 KiB for descriptors */
    u64 control_dma;
    struct endpoint ep[32]; /* by DCI; ep[1] is the default control pipe */
    struct hid hid[2];
    struct msc msc;
    char phys[32];
};

static struct {
    struct spk_pci_device *pci;
    volatile u8 *base;
    unsigned long base_len;
    volatile u8 *op, *rt, *db;
    unsigned int max_slots, max_ports, csz;
    volatile u64 *dcbaa;
    u64 dcbaa_dma;
    u64 *scratch_array;
    u64 scratch_array_dma;
    unsigned int scratch_count;
    struct ring command;
    volatile struct trb *events;
    u64 events_dma;
    unsigned int event_dequeue;
    u32 event_cycle;
    u64 *erst;
    u64 erst_dma;
    struct spk_mutex *command_lock;
    struct spk_completion *command_done;
    volatile int command_cc;
    volatile u32 command_slot;
    struct spk_completion *port_done[MAX_PORTS + 1];
    struct spk_mutex *topology_lock;
    struct device *by_slot[MAX_SLOTS + 1];
    struct device *by_port[MAX_PORTS + 1];
    volatile u32 pending_ports; /* bitmap of ports with unhandled changes */
    spk_spinlock pending_lock;
    int irq_mode; /* 1 MSI, 2 INTx */
    int shutting_down;
} hc;

/* ---- MMIO helpers ---------------------------------------------------- */
static u32 rd(volatile u8 *base, u32 offset) { return spk_readl(base + offset); }
static void wr(volatile u8 *base, u32 offset, u32 value) { spk_writel(value, base + offset); }
static void wr64(volatile u8 *base, u32 offset, u64 value)
{
    wr(base, offset, (u32)value);
    wr(base, offset + 4, (u32)(value >> 32));
}

static int wait_bits(volatile u8 *base, u32 offset, u32 mask, u32 want, unsigned int ms)
{
    /* Controller handshakes (reset, halt) have no completion interrupt. */
    for (unsigned int i = 0; i <= ms; i++) {
        if ((rd(base, offset) & mask) == want)
            return 0;
        spk_msleep(1);
    }
    return -ETIMEDOUT;
}

static void *dma_alloc(unsigned long size, unsigned long align, u64 *dma)
{
    return spk_dma_alloc_coherent(size, align, dma);
}

/* ---- rings ------------------------------------------------------------ */
static int ring_init(struct ring *ring)
{
    ring->trbs = dma_alloc(RING_TRBS * sizeof(struct trb), 64, &ring->dma);
    if (!ring->trbs)
        return -ENOMEM;
    ring->enqueue = 0;
    ring->cycle = 1;
    volatile struct trb *link = &ring->trbs[RING_TRBS - 1];
    link->p0 = (u32)ring->dma;
    link->p1 = (u32)(ring->dma >> 32);
    link->status = 0;
    link->control = TRB_TYPE(TRB_LINK) | TRB_TC; /* cycle set when reached */
    return 0;
}

static void ring_free(struct ring *ring)
{
    if (ring->trbs)
        spk_dma_free_coherent((void *)ring->trbs, RING_TRBS * sizeof(struct trb));
    ring->trbs = 0;
}

/* Queue one TRB; the cycle bit is written last so the controller never
 * sees a half-built TRB. Returns the TRB's bus address. */
static u64 ring_push(struct ring *ring, u32 p0, u32 p1, u32 status, u32 control)
{
    volatile struct trb *trb = &ring->trbs[ring->enqueue];
    u64 address = ring->dma + (u64)ring->enqueue * sizeof(struct trb);
    trb->p0 = p0;
    trb->p1 = p1;
    trb->status = status;
    spk_mb();
    trb->control = (control & ~TRB_CYCLE) | ring->cycle;
    ring->enqueue++;
    if (ring->enqueue == RING_TRBS - 1) {
        volatile struct trb *link = &ring->trbs[RING_TRBS - 1];
        /* Chain the link when the TD continues past it. */
        u32 chain = control & (1u << 4);
        spk_mb();
        link->control = TRB_TYPE(TRB_LINK) | TRB_TC | chain | ring->cycle;
        ring->enqueue = 0;
        ring->cycle ^= 1;
    }
    return address;
}

static void doorbell(unsigned int slot, unsigned int target)
{
    spk_mb();
    wr(hc.db, 4 * slot, target);
}

/* ---- contexts ---------------------------------------------------------- */
static volatile u32 *in_ctrl(struct device *dev) { return (volatile u32 *)dev->in_ctx; }
static volatile u32 *in_slot(struct device *dev)
{
    return (volatile u32 *)((u8 *)dev->in_ctx + hc.csz);
}
static volatile u32 *in_ep(struct device *dev, unsigned int dci)
{
    return (volatile u32 *)((u8 *)dev->in_ctx + hc.csz * (dci + 1));
}

/* ---- event handling (interrupt context) ------------------------------- */
static void hid_report(struct device *dev, struct hid *hid, unsigned int length);
static void hid_rearm(struct device *dev, struct hid *hid);
static void port_work(void *unused);

static void handle_transfer_event(volatile struct trb *event)
{
    unsigned int slot = event->control >> 24;
    unsigned int dci = (event->control >> 16) & 0x1f;
    int cc = event->status >> 24;
    u32 residue = event->status & 0xffffff;
    if (slot == 0 || slot > MAX_SLOTS || !hc.by_slot[slot])
        return;
    struct device *dev = hc.by_slot[slot];
    struct endpoint *ep = &dev->ep[dci];
    if (!ep->active)
        return;
    if (ep->hid >= 0) {
        struct hid *hid = &dev->hid[ep->hid];
        if (cc == CC_SUCCESS || cc == CC_SHORT_PACKET)
            hid_report(dev, hid, ep->max_packet - (residue < ep->max_packet ? residue : 0));
        if (!hc.shutting_down && dev->present)
            hid_rearm(dev, hid);
        return;
    }
    ep->xfer.cc = cc;
    ep->xfer.residue = residue;
    spk_complete(ep->xfer.done);
}

static void process_events(void)
{
    for (;;) {
        volatile struct trb *event = &hc.events[hc.event_dequeue];
        if ((event->control & TRB_CYCLE) != hc.event_cycle)
            break;
        unsigned int type = (event->control >> 10) & 0x3f;
        switch (type) {
        case TRB_COMMAND_COMPLETION:
            hc.command_cc = event->status >> 24;
            hc.command_slot = event->control >> 24;
            spk_complete(hc.command_done);
            break;
        case TRB_TRANSFER_EVENT:
            handle_transfer_event(event);
            break;
        case TRB_PORT_STATUS_CHANGE: {
            unsigned int port = event->p0 >> 24;
            if (port >= 1 && port <= hc.max_ports) {
                if (hc.port_done[port])
                    spk_complete(hc.port_done[port]);
                unsigned long flags = spk_spin_lock_irqsave(&hc.pending_lock);
                int idle = hc.pending_ports == 0;
                hc.pending_ports |= 1u << port;
                spk_spin_unlock_irqrestore(&hc.pending_lock, flags);
                if (idle && !hc.shutting_down)
                    spk_work_schedule(port_work, 0);
            }
            break;
        }
        default:
            break;
        }
        hc.event_dequeue++;
        if (hc.event_dequeue == RING_TRBS) {
            hc.event_dequeue = 0;
            hc.event_cycle ^= 1;
        }
    }
    wr64(hc.rt, IR_ERDP, (hc.events_dma + (u64)hc.event_dequeue * sizeof(struct trb)) | ERDP_EHB);
}

static void xhci_irq(int irq, void *data)
{
    (void)irq;
    (void)data;
    u32 status = rd(hc.op, OP_USBSTS);
    if (status & STS_EINT)
        wr(hc.op, OP_USBSTS, STS_EINT);
    u32 iman = rd(hc.rt, IR_IMAN);
    wr(hc.rt, IR_IMAN, iman | IMAN_IP | IMAN_IE);
    process_events();
}

/* ---- commands and transfers (process context) ------------------------- */
static int command(u32 p0, u32 p1, u32 control, unsigned int *slot_out)
{
    spk_mutex_lock(hc.command_lock);
    spk_completion_reinit(hc.command_done);
    ring_push(&hc.command, p0, p1, 0, control);
    doorbell(0, 0);
    int done = spk_wait_for_completion_timeout(hc.command_done, 2000);
    int cc = hc.command_cc;
    unsigned int slot = hc.command_slot;
    spk_mutex_unlock(hc.command_lock);
    if (!done) {
        spk_pr_warn("usb: command timed out");
        return -ETIMEDOUT;
    }
    if (cc != CC_SUCCESS) {
        spk_printk("usb: command type %u failed, completion code %d\n",
                   (control >> 10) & 0x3f, cc);
        return -EIO;
    }
    if (slot_out)
        *slot_out = slot;
    return 0;
}

static int wait_transfer(struct endpoint *ep, unsigned int timeout_ms)
{
    if (!spk_wait_for_completion_timeout(ep->xfer.done, timeout_ms))
        return -ETIMEDOUT;
    if (ep->xfer.cc != CC_SUCCESS && ep->xfer.cc != CC_SHORT_PACKET)
        return -EIO;
    return 0;
}

/* Control transfer on the default pipe; data goes through control_buffer. */
static int control_transfer(struct device *dev, u8 request_type, u8 request, u16 value,
                            u16 index, u16 length, void *data)
{
    struct endpoint *ep = &dev->ep[1];
    int in = request_type & 0x80;
    if (length > 4096)
        return -EINVAL;
    if (!in && length)
        memcpy(dev->control_buffer, data, length);
    spk_completion_reinit(ep->xfer.done);
    u32 trt = length ? (in ? 3u : 2u) : 0u;
    ring_push(&ep->ring, request_type | (u32)request << 8 | (u32)value << 16,
              index | (u32)length << 16, 8, TRB_TYPE(TRB_SETUP) | TRB_IDT | trt << 16);
    if (length)
        ring_push(&ep->ring, (u32)dev->control_dma, (u32)(dev->control_dma >> 32), length,
                  TRB_TYPE(TRB_DATA) | (in ? TRB_DIR_IN : 0));
    ring_push(&ep->ring, 0, 0, 0,
              TRB_TYPE(TRB_STATUS) | TRB_IOC | ((length && in) ? 0 : TRB_DIR_IN));
    doorbell(dev->slot, 1);
    int error = wait_transfer(ep, 2000);
    if (!error && in && length)
        memcpy(data, dev->control_buffer, length);
    return error;
}

static int bulk_transfer(struct device *dev, struct endpoint *ep, u64 dma, unsigned int length,
                         unsigned int *actual)
{
    spk_completion_reinit(ep->xfer.done);
    ring_push(&ep->ring, (u32)dma, (u32)(dma >> 32), length,
              TRB_TYPE(TRB_NORMAL) | TRB_IOC | TRB_ISP);
    doorbell(dev->slot, ep->dci);
    int error = wait_transfer(ep, 10000);
    if (actual)
        *actual = error ? 0 : length - (ep->xfer.residue < length ? ep->xfer.residue : length);
    return error;
}

/* ---- endpoint setup ---------------------------------------------------- */
static int endpoint_init(struct device *dev, unsigned int dci, unsigned int max_packet)
{
    struct endpoint *ep = &dev->ep[dci];
    if (ring_init(&ep->ring))
        return -ENOMEM;
    ep->xfer.done = spk_completion_alloc();
    if (!ep->xfer.done) {
        ring_free(&ep->ring);
        return -ENOMEM;
    }
    ep->dci = dci;
    ep->max_packet = max_packet;
    ep->owner = dev;
    ep->hid = -1;
    ep->active = 1;
    return 0;
}

static void endpoint_free(struct endpoint *ep)
{
    if (!ep->active)
        return;
    ep->active = 0;
    ring_free(&ep->ring);
    spk_completion_free(ep->xfer.done);
}

/* xHCI interval exponent (125 us units) from a descriptor bInterval. */
static unsigned int interval_exponent(unsigned int speed, unsigned int b_interval)
{
    if (b_interval == 0)
        b_interval = 1;
    if (speed >= 3) /* high/super speed: 2^(bInterval-1) microframes */
        return b_interval - 1 > 15 ? 15 : b_interval - 1;
    unsigned int frames = b_interval * 8, exponent = 0;
    while ((1u << (exponent + 1)) <= frames && exponent < 10)
        exponent++;
    return exponent < 3 ? 3 : exponent;
}

static void fill_ep_context(struct device *dev, unsigned int dci, unsigned int type,
                            unsigned int max_packet, unsigned int interval)
{
    volatile u32 *ep = in_ep(dev, dci);
    for (unsigned int i = 0; i < hc.csz / 4; i++)
        ep[i] = 0;
    ep[0] = interval << 16;
    ep[1] = 3u << 1 | type << 3 | max_packet << 16;
    u64 dequeue = dev->ep[dci].ring.dma | 1;
    ep[2] = (u32)dequeue;
    ep[3] = (u32)(dequeue >> 32);
    unsigned int average = type == 4 ? 8 : max_packet;
    ep[4] = average | (type == 3 || type == 7 ? max_packet << 16 : 0);
}

/* ---- HID boot protocol ------------------------------------------------- */
static const u8 hid_keymap[0x68] = {
    0,   0,   0,   0,   30,  48,  46,  32,  18,  33,  34,  35,  23,  36,  37,  38,
    50,  49,  24,  25,  16,  19,  31,  20,  22,  47,  17,  45,  21,  44,  2,   3,
    4,   5,   6,   7,   8,   9,   10,  11,  28,  1,   14,  15,  57,  12,  13,  26,
    27,  43,  43,  39,  40,  41,  51,  52,  53,  58,  59,  60,  61,  62,  63,  64,
    65,  66,  67,  68,  87,  88,  99,  70,  119, 110, 102, 104, 111, 107, 109, 106,
    105, 108, 103, 69,  98,  55,  74,  78,  96,  79,  80,  81,  75,  76,  77,  71,
    72,  73,  82,  83,  86,  127, 116, 117,
};
static const u8 modifier_keys[8] = {29, 42, 56, 125, 97, 54, 100, 126};

#define BTN_LEFT 0x110
#define BTN_RIGHT 0x111
#define BTN_MIDDLE 0x112
#define REL_X 0x00
#define REL_Y 0x01
#define REL_WHEEL 0x08

static int key_in(const u8 *report, u8 usage)
{
    for (int i = 2; i < 8; i++)
        if (report[i] == usage)
            return 1;
    return 0;
}

static void hid_report(struct device *dev, struct hid *hid, unsigned int length)
{
    (void)dev;
    const u8 *report = hid->buffer;
    if (!hid->input)
        return;
    if (hid->kind == HID_KEYBOARD) {
        if (length < 8 || report[2] == 1) /* ErrorRollOver */
            return;
        u8 changed = report[0] ^ hid->previous[0];
        for (int bit = 0; bit < 8; bit++)
            if (changed & (1u << bit))
                spk_input_event(hid->input, SPK_EV_KEY, modifier_keys[bit],
                                (report[0] >> bit) & 1);
        for (int i = 2; i < 8; i++) {
            u8 usage = hid->previous[i];
            if (usage > 3 && usage < sizeof(hid_keymap) && hid_keymap[usage] &&
                !key_in(report, usage))
                spk_input_event(hid->input, SPK_EV_KEY, hid_keymap[usage], 0);
        }
        for (int i = 2; i < 8; i++) {
            u8 usage = report[i];
            if (usage > 3 && usage < sizeof(hid_keymap) && hid_keymap[usage] &&
                !key_in(hid->previous, usage))
                spk_input_event(hid->input, SPK_EV_KEY, hid_keymap[usage], 1);
        }
        memcpy(hid->previous, report, 8);
    } else {
        if (length < 3)
            return;
        static const unsigned int buttons[3] = {BTN_LEFT, BTN_RIGHT, BTN_MIDDLE};
        u8 changed = report[0] ^ hid->previous[0];
        for (int bit = 0; bit < 3; bit++)
            if (changed & (1u << bit))
                spk_input_event(hid->input, SPK_EV_KEY, buttons[bit], (report[0] >> bit) & 1);
        if (report[1])
            spk_input_event(hid->input, SPK_EV_REL, REL_X, (signed char)report[1]);
        if (report[2])
            spk_input_event(hid->input, SPK_EV_REL, REL_Y, (signed char)report[2]);
        if (length >= 4 && report[3])
            spk_input_event(hid->input, SPK_EV_REL, REL_WHEEL, (signed char)report[3]);
        hid->previous[0] = report[0];
    }
    spk_input_sync(hid->input);
}

static void hid_rearm(struct device *dev, struct hid *hid)
{
    struct endpoint *ep = hid->ep;
    ring_push(&ep->ring, (u32)hid->buffer_dma, (u32)(hid->buffer_dma >> 32), ep->max_packet,
              TRB_TYPE(TRB_NORMAL) | TRB_IOC | TRB_ISP);
    doorbell(dev->slot, ep->dci);
}

static int hid_start(struct device *dev, int index, unsigned int interface)
{
    struct hid *hid = &dev->hid[index];
    /* Boot protocol and no idle repeat: reports only on change. */
    if (control_transfer(dev, 0x21, 0x0b, 0, interface, 0, 0))
        spk_pr_warn("usb: SET_PROTOCOL(boot) failed");
    control_transfer(dev, 0x21, 0x0a, 0, interface, 0, 0);
    hid->buffer = dma_alloc(64, 64, &hid->buffer_dma);
    if (!hid->buffer)
        return -ENOMEM;
    const char *name = hid->kind == HID_KEYBOARD ? "USB Boot Keyboard" : "USB Boot Mouse";
    hid->input = spk_input_alloc(name, dev->phys, SPK_BUS_USB, dev->vendor, dev->product);
    if (!hid->input)
        return -ENOMEM;
    if (hid->kind == HID_KEYBOARD) {
        for (unsigned int i = 4; i < sizeof(hid_keymap); i++)
            if (hid_keymap[i])
                spk_input_set_capability(hid->input, SPK_EV_KEY, hid_keymap[i]);
        for (int i = 0; i < 8; i++)
            spk_input_set_capability(hid->input, SPK_EV_KEY, modifier_keys[i]);
    } else {
        spk_input_set_capability(hid->input, SPK_EV_KEY, BTN_LEFT);
        spk_input_set_capability(hid->input, SPK_EV_KEY, BTN_RIGHT);
        spk_input_set_capability(hid->input, SPK_EV_KEY, BTN_MIDDLE);
        spk_input_set_capability(hid->input, SPK_EV_REL, REL_X);
        spk_input_set_capability(hid->input, SPK_EV_REL, REL_Y);
        spk_input_set_capability(hid->input, SPK_EV_REL, REL_WHEEL);
    }
    int error = spk_input_register(hid->input);
    if (error) {
        spk_input_free(hid->input);
        hid->input = 0;
        return error;
    }
    hid_rearm(dev, hid);
    spk_printk("USB_HID_OK %s port %u %04x:%04x\n",
               hid->kind == HID_KEYBOARD ? "keyboard" : "mouse", dev->port, dev->vendor,
               dev->product);
    return 0;
}

/* ---- mass storage (Bulk-Only Transport + SCSI) -------------------------- */
static int msc_command(struct device *dev, const u8 *cdb, unsigned int cdb_length, int in,
                       u64 data_dma, unsigned int length, unsigned int *actual)
{
    struct msc *msc = &dev->msc;
    /* CBW and CSW live at the end of the bounce buffer, after the data. */
    u8 *cbw = msc->buffer + BOUNCE_SIZE - 64;
    u64 cbw_dma = msc->buffer_dma + BOUNCE_SIZE - 64;
    u8 *csw = msc->buffer + BOUNCE_SIZE - 32;
    u64 csw_dma = msc->buffer_dma + BOUNCE_SIZE - 32;
    u32 tag = ++msc->tag;
    memset(cbw, 0, 31);
    cbw[0] = 'U', cbw[1] = 'S', cbw[2] = 'B', cbw[3] = 'C';
    memcpy(cbw + 4, &tag, 4);
    memcpy(cbw + 8, &length, 4);
    cbw[12] = in ? 0x80 : 0x00;
    cbw[13] = 0; /* LUN 0 */
    cbw[14] = (u8)cdb_length;
    memcpy(cbw + 15, cdb, cdb_length);
    int error = bulk_transfer(dev, msc->out, cbw_dma, 31, 0);
    if (error)
        return error;
    unsigned int moved = 0;
    if (length) {
        error = bulk_transfer(dev, in ? msc->in : msc->out, data_dma, length, &moved);
        if (error)
            return error;
    }
    error = bulk_transfer(dev, msc->in, csw_dma, 13, 0);
    if (error)
        return error;
    u32 signature, csw_tag;
    memcpy(&signature, csw, 4);
    memcpy(&csw_tag, csw + 4, 4);
    if (signature != 0x53425355 || csw_tag != tag || csw[12] != 0)
        return -EIO;
    if (actual)
        *actual = moved;
    return 0;
}

static int msc_rw(struct device *dev, int write, u64 lba, unsigned int count, u8 *buffer)
{
    struct msc *msc = &dev->msc;
    unsigned int per_chunk = (BOUNCE_SIZE - 64) / msc->block_size;
    int error = 0;
    spk_mutex_lock(msc->lock);
    while (count && !error) {
        unsigned int blocks = count < per_chunk ? count : per_chunk;
        unsigned int bytes = blocks * msc->block_size;
        u8 cdb[10] = {write ? 0x2a : 0x28, 0, (u8)(lba >> 24), (u8)(lba >> 16), (u8)(lba >> 8),
                      (u8)lba, 0, (u8)(blocks >> 8), (u8)blocks, 0};
        if (lba + blocks > 0xffffffffull) {
            error = -EINVAL;
            break;
        }
        if (write)
            memcpy(msc->buffer, buffer, bytes);
        unsigned int moved = 0;
        error = msc_command(dev, cdb, 10, !write, msc->buffer_dma, bytes, &moved);
        if (!error && moved != bytes)
            error = -EIO;
        if (!error && !write)
            memcpy(buffer, msc->buffer, bytes);
        buffer += bytes;
        lba += blocks;
        count -= blocks;
    }
    spk_mutex_unlock(msc->lock);
    return error;
}

static int msc_read(void *priv, unsigned long long lba, unsigned int count, void *buffer)
{
    struct device *dev = priv;
    if (!dev->present)
        return -ENODEV;
    return msc_rw(dev, 0, lba, count, buffer);
}

static int msc_write(void *priv, unsigned long long lba, unsigned int count, const void *buffer)
{
    struct device *dev = priv;
    if (!dev->present)
        return -ENODEV;
    return msc_rw(dev, 1, lba, count, (u8 *)buffer);
}

static int msc_flush(void *priv)
{
    struct device *dev = priv;
    if (!dev->present)
        return -ENODEV;
    u8 cdb[10] = {0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    spk_mutex_lock(dev->msc.lock);
    int error = msc_command(dev, cdb, 10, 0, 0, 0, 0);
    spk_mutex_unlock(dev->msc.lock);
    return error;
}

static const struct spk_blkdev_ops msc_ops = {
    .read = msc_read,
    .write = msc_write,
    .flush = msc_flush,
};

static void trim_ascii(char *out, const u8 *in, unsigned int length)
{
    unsigned int n = 0;
    for (unsigned int i = 0; i < length; i++)
        out[n++] = (in[i] >= 32 && in[i] < 127) ? (char)in[i] : ' ';
    while (n && out[n - 1] == ' ')
        n--;
    out[n] = 0;
}

static int msc_start(struct device *dev)
{
    struct msc *msc = &dev->msc;
    msc->lock = spk_mutex_alloc();
    msc->buffer = dma_alloc(BOUNCE_SIZE, 4096, &msc->buffer_dma);
    if (!msc->lock || !msc->buffer)
        return -ENOMEM;
    u8 *data = msc->buffer;
    u8 inquiry[6] = {0x12, 0, 0, 0, 36, 0};
    if (msc_command(dev, inquiry, 6, 1, msc->buffer_dma, 36, 0))
        return -EIO;
    char model[32];
    {
        char vendor[9], product[17];
        trim_ascii(vendor, data + 8, 8);
        trim_ascii(product, data + 16, 16);
        unsigned int n = 0;
        for (const char *p = vendor; *p && n < 30; p++)
            model[n++] = *p;
        if (n < 30)
            model[n++] = ' ';
        for (const char *p = product; *p && n < 31; p++)
            model[n++] = *p;
        model[n] = 0;
    }
    u8 ready[6] = {0, 0, 0, 0, 0, 0};
    int error = -EIO;
    for (int attempt = 0; attempt < 5 && error; attempt++) {
        error = msc_command(dev, ready, 6, 0, 0, 0, 0);
        if (error)
            spk_msleep(100); /* media spin-up; Linux retries the same way */
    }
    u8 capacity[10] = {0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    if (msc_command(dev, capacity, 10, 1, msc->buffer_dma, 8, 0))
        return -EIO;
    u32 last = (u32)data[0] << 24 | (u32)data[1] << 16 | (u32)data[2] << 8 | data[3];
    msc->block_size = (u32)data[4] << 24 | (u32)data[5] << 16 | (u32)data[6] << 8 | data[7];
    msc->blocks = (u64)last + 1;
    if (msc->block_size == 0 || msc->block_size > 4096 || msc->block_size & (msc->block_size - 1))
        return -EIO;
    char serial[16];
    unsigned int n = 0;
    const char hex[] = "0123456789abcdef";
    serial[n++] = 'u', serial[n++] = 's', serial[n++] = 'b';
    serial[n++] = hex[(dev->port >> 4) & 15], serial[n++] = hex[dev->port & 15];
    serial[n] = 0;
    msc->blkdev = spk_blkdev_register("sd", model, serial, msc->block_size, msc->blocks, 1, 0,
                                      &msc_ops, dev);
    if (!msc->blkdev)
        return -ENOMEM;
    char name[16];
    spk_blkdev_name(msc->blkdev, name, sizeof(name));
    spk_printk("USB_MSC_OK %s '%s' %u blocks of %u bytes\n", name, model, (u32)msc->blocks,
               msc->block_size);
    return 0;
}

/* ---- enumeration ---------------------------------------------------------- */
static void device_release(struct device *dev)
{
    for (int i = 0; i < 2; i++) {
        if (dev->hid[i].input)
            spk_input_unregister(dev->hid[i].input);
        dev->hid[i].input = 0;
    }
    if (dev->msc.blkdev)
        spk_blkdev_unregister(dev->msc.blkdev);
    dev->msc.blkdev = 0;
    if (dev->slot) {
        hc.by_slot[dev->slot] = 0;
        command(0, 0, TRB_TYPE(TRB_DISABLE_SLOT) | dev->slot << 24, 0);
        hc.dcbaa[dev->slot] = 0;
    }
    for (int i = 0; i < 32; i++)
        endpoint_free(&dev->ep[i]);
    for (int i = 0; i < 2; i++)
        if (dev->hid[i].buffer)
            spk_dma_free_coherent(dev->hid[i].buffer, 64);
    if (dev->msc.buffer)
        spk_dma_free_coherent(dev->msc.buffer, BOUNCE_SIZE);
    if (dev->msc.lock)
        spk_mutex_free(dev->msc.lock);
    if (dev->control_buffer)
        spk_dma_free_coherent(dev->control_buffer, 4096);
    if (dev->in_ctx)
        spk_dma_free_coherent(dev->in_ctx, 33 * hc.csz);
    if (dev->out_ctx)
        spk_dma_free_coherent(dev->out_ctx, 32 * hc.csz);
    spk_kfree(dev);
}

static int port_reset(unsigned int port)
{
    u32 portsc = rd(hc.op, OP_PORTSC(port));
    if (portsc & PORT_PED)
        return 0; /* USB3 ports enable themselves after link training */
    spk_completion_reinit(hc.port_done[port]);
    wr(hc.op, OP_PORTSC(port), (portsc & PORT_PRESERVE) | PORT_PR);
    for (int attempt = 0; attempt < 10; attempt++) {
        spk_wait_for_completion_timeout(hc.port_done[port], 100);
        portsc = rd(hc.op, OP_PORTSC(port));
        if (!(portsc & PORT_PR) && (portsc & PORT_PED))
            return 0;
        if (!(portsc & PORT_CCS))
            return -ENODEV;
    }
    return -ETIMEDOUT;
}

static int configure(struct device *dev, const u8 *config, unsigned int length)
{
    volatile u32 *ctrl = in_ctrl(dev);
    unsigned int add = 1, max_dci = 1;
    int hid_count = 0;
    unsigned int msc_interface = 0;
    struct endpoint *msc_in = 0, *msc_out = 0;
    int pending_hid[2] = {-1, -1};
    unsigned int hid_interface[2] = {0, 0};
    u8 klass = 0, subclass = 0, protocol = 0, interface = 0;
    for (unsigned int i = 0; i + 2 <= length && config[i] >= 2; i += config[i]) {
        const u8 *d = config + i;
        if (i + d[0] > length)
            break;
        if (d[1] == 4 && d[0] >= 9) { /* interface */
            interface = d[2];
            klass = d[5], subclass = d[6], protocol = d[7];
        } else if (d[1] == 5 && d[0] >= 7) { /* endpoint */
            unsigned int address = d[2], attributes = d[3] & 3;
            unsigned int max_packet = (d[4] | d[5] << 8) & 0x7ff;
            unsigned int dci = (address & 0xf) * 2 + ((address & 0x80) ? 1 : 0);
            int is_hid = klass == 3 && subclass == 1 && (protocol == 1 || protocol == 2) &&
                         attributes == 3 && (address & 0x80) && hid_count < 2;
            int is_msc = klass == 8 && subclass == 6 && protocol == 0x50 && attributes == 2;
            if (!(is_hid || is_msc) || dci < 2 || dci > 31 || dev->ep[dci].active)
                continue;
            if (endpoint_init(dev, dci, max_packet))
                return -ENOMEM;
            unsigned int type = attributes == 3 ? ((address & 0x80) ? 7 : 3)
                                                : ((address & 0x80) ? 6 : 2);
            fill_ep_context(dev, dci, type, max_packet,
                            attributes == 3 ? interval_exponent(dev->speed, d[6]) : 0);
            add |= 1u << dci;
            if (dci > max_dci)
                max_dci = dci;
            if (is_hid) {
                dev->hid[hid_count].kind = protocol == 1 ? HID_KEYBOARD : HID_MOUSE;
                dev->hid[hid_count].ep = &dev->ep[dci];
                dev->ep[dci].hid = hid_count;
                pending_hid[hid_count] = hid_count;
                hid_interface[hid_count] = interface;
                hid_count++;
            } else if (address & 0x80) {
                msc_in = &dev->ep[dci];
                msc_interface = interface;
            } else {
                msc_out = &dev->ep[dci];
            }
        }
    }
    if (add == 1)
        return 0; /* nothing this driver handles */
    ctrl[0] = 0;
    ctrl[1] = add;
    volatile u32 *slot = in_slot(dev);
    slot[0] = (slot[0] & ~(0x1fu << 27)) | max_dci << 27;
    int error = command((u32)dev->in_ctx_dma, (u32)(dev->in_ctx_dma >> 32),
                        TRB_TYPE(TRB_CONFIGURE_EP) | dev->slot << 24, 0);
    if (error)
        return error;
    for (int i = 0; i < hid_count; i++)
        if (pending_hid[i] >= 0)
            hid_start(dev, i, hid_interface[i]);
    if (msc_in && msc_out) {
        dev->msc.active = 1;
        dev->msc.interface = msc_interface;
        dev->msc.in = msc_in;
        dev->msc.out = msc_out;
        if (msc_start(dev))
            spk_pr_warn("usb: mass-storage device did not initialize");
    }
    return 0;
}

static int attach(unsigned int port)
{
    int error = port_reset(port);
    if (error)
        return error;
    u32 portsc = rd(hc.op, OP_PORTSC(port));
    struct device *dev = spk_kzalloc(sizeof(*dev));
    if (!dev)
        return -ENOMEM;
    dev->port = port;
    dev->speed = (portsc >> 10) & 0xf;
    {
        const char prefix[] = "usb-xhci/port";
        unsigned int n = 0;
        for (; prefix[n]; n++)
            dev->phys[n] = prefix[n];
        if (port >= 10)
            dev->phys[n++] = (char)('0' + port / 10);
        dev->phys[n++] = (char)('0' + port % 10);
        dev->phys[n] = 0;
    }
    dev->out_ctx = dma_alloc(32 * hc.csz, 64, &dev->out_ctx_dma);
    dev->in_ctx = dma_alloc(33 * hc.csz, 64, &dev->in_ctx_dma);
    dev->control_buffer = dma_alloc(4096, 64, &dev->control_dma);
    if (!dev->out_ctx || !dev->in_ctx || !dev->control_buffer) {
        device_release(dev);
        return -ENOMEM;
    }
    unsigned int slot = 0;
    error = command(0, 0, TRB_TYPE(TRB_ENABLE_SLOT), &slot);
    if (error || slot == 0 || slot > hc.max_slots || slot > MAX_SLOTS) {
        device_release(dev);
        return error ? error : -EIO;
    }
    dev->slot = slot;
    hc.dcbaa[slot] = dev->out_ctx_dma;
    hc.by_slot[slot] = dev;
    /* Default control pipe: 8 bytes for low/full speed until the device
     * descriptor says otherwise, 64 for high speed, 512 for SuperSpeed. */
    unsigned int max_packet = dev->speed == 4 ? 512 : dev->speed == 3 ? 64 : 8;
    if (endpoint_init(dev, 1, max_packet)) {
        device_release(dev);
        return -ENOMEM;
    }
    volatile u32 *ctrl = in_ctrl(dev);
    ctrl[0] = 0;
    ctrl[1] = 3; /* A0 slot, A1 EP0 */
    volatile u32 *slot_ctx = in_slot(dev);
    slot_ctx[0] = dev->speed << 20 | 1u << 27;
    slot_ctx[1] = port << 16;
    fill_ep_context(dev, 1, 4, max_packet, 0);
    error = command((u32)dev->in_ctx_dma, (u32)(dev->in_ctx_dma >> 32),
                    TRB_TYPE(TRB_ADDRESS_DEVICE) | slot << 24, 0);
    if (error) {
        device_release(dev);
        return error;
    }
    u8 descriptor[18];
    error = control_transfer(dev, 0x80, 6, 0x0100, 0, 8, descriptor);
    if (!error && descriptor[7] && descriptor[7] != max_packet && dev->speed < 3) {
        max_packet = descriptor[7];
        dev->ep[1].max_packet = max_packet;
        ctrl[0] = 0;
        ctrl[1] = 2; /* evaluate EP0 */
        volatile u32 *ep0 = in_ep(dev, 1);
        ep0[1] = (ep0[1] & 0xffff) | max_packet << 16;
        error = command((u32)dev->in_ctx_dma, (u32)(dev->in_ctx_dma >> 32),
                        TRB_TYPE(TRB_EVALUATE_CONTEXT) | slot << 24, 0);
    }
    if (!error)
        error = control_transfer(dev, 0x80, 6, 0x0100, 0, 18, descriptor);
    if (error) {
        device_release(dev);
        return error;
    }
    dev->vendor = descriptor[8] | descriptor[9] << 8;
    dev->product = descriptor[10] | descriptor[11] << 8;
    u8 header[9];
    error = control_transfer(dev, 0x80, 6, 0x0200, 0, 9, header);
    unsigned int total = error ? 0 : (unsigned int)(header[2] | header[3] << 8);
    if (total < 9 || total > 4096) {
        device_release(dev);
        return -EIO;
    }
    static u8 config[4096];
    error = control_transfer(dev, 0x80, 6, 0x0200, 0, (u16)total, config);
    if (!error)
        error = control_transfer(dev, 0x00, 9, header[5], 0, 0, 0); /* SET_CONFIGURATION */
    if (error) {
        device_release(dev);
        return error;
    }
    spk_printk("usb: port %u speed %u device %04x:%04x class %u, slot %u\n", port, dev->speed,
               dev->vendor, dev->product, descriptor[4], slot);
    dev->present = 1;
    hc.by_port[port] = dev;
    error = configure(dev, config, total);
    if (error)
        spk_printk("usb: port %u configuration failed (%d)\n", port, error);
    return 0;
}

static void detach(unsigned int port)
{
    struct device *dev = hc.by_port[port];
    if (!dev)
        return;
    hc.by_port[port] = 0;
    dev->present = 0;
    spk_printk("usb: port %u device %04x:%04x removed\n", port, dev->vendor, dev->product);
    device_release(dev);
}

static void check_port(unsigned int port)
{
    u32 portsc = rd(hc.op, OP_PORTSC(port));
    /* Acknowledge the change bits (write 1 to clear), keeping state bits. */
    wr(hc.op, OP_PORTSC(port), (portsc & PORT_PRESERVE) | (portsc & PORT_CHANGES));
    int connected = (portsc & PORT_CCS) != 0;
    if (hc.by_port[port] && (!connected || (portsc & PORT_CSC))) {
        detach(port);
    }
    if (connected && !hc.by_port[port]) {
        int error = attach(port);
        if (error)
            spk_printk("usb: port %u attach failed (%d)\n", port, error);
    }
}

static void port_work(void *unused)
{
    (void)unused;
    spk_mutex_lock(hc.topology_lock);
    for (;;) {
        unsigned long flags = spk_spin_lock_irqsave(&hc.pending_lock);
        u32 pending = hc.pending_ports;
        hc.pending_ports = 0;
        spk_spin_unlock_irqrestore(&hc.pending_lock, flags);
        if (!pending || hc.shutting_down)
            break;
        for (unsigned int port = 1; port <= hc.max_ports; port++)
            if (pending & (1u << port))
                check_port(port);
    }
    spk_mutex_unlock(hc.topology_lock);
}

/* ---- controller bring-up -------------------------------------------------- */
static void *scratch_pages[64];

static void controller_free(void)
{
    if (hc.irq_mode == 1)
        spk_pci_disable_msi(hc.pci);
    else if (hc.irq_mode == 2)
        spk_free_irq(hc.pci);
    hc.irq_mode = 0;
    ring_free(&hc.command);
    if (hc.events)
        spk_dma_free_coherent((void *)hc.events, RING_TRBS * sizeof(struct trb));
    if (hc.erst)
        spk_dma_free_coherent(hc.erst, 64);
    for (unsigned int i = 0; i < 64; i++) {
        if (scratch_pages[i])
            spk_dma_free_coherent(scratch_pages[i], 4096);
        scratch_pages[i] = 0;
    }
    if (hc.scratch_array)
        spk_dma_free_coherent(hc.scratch_array, 8 * hc.scratch_count);
    if (hc.dcbaa)
        spk_dma_free_coherent((void *)hc.dcbaa, 8 * (MAX_SLOTS + 1));
    for (unsigned int i = 0; i <= MAX_PORTS; i++)
        if (hc.port_done[i])
            spk_completion_free(hc.port_done[i]);
    if (hc.command_done)
        spk_completion_free(hc.command_done);
    if (hc.command_lock)
        spk_mutex_free(hc.command_lock);
    if (hc.topology_lock)
        spk_mutex_free(hc.topology_lock);
    if (hc.base)
        spk_iounmap((void *)hc.base, hc.base_len);
}

static int xhci_probe(struct spk_pci_device *pci)
{
    if (hc.pci)
        return -EINVAL; /* one controller is enough for this driver */
    memset(&hc, 0, sizeof(hc));
    hc.pci = pci;
    int error = spk_pci_enable_device(pci);
    if (error)
        return error;
    spk_pci_set_master(pci);
    hc.base_len = spk_pci_resource_len(pci, 0);
    hc.base = spk_ioremap(spk_pci_resource_start(pci, 0), hc.base_len);
    if (!hc.base)
        return -ENOMEM;
    hc.op = hc.base + (rd(hc.base, CAP_CAPLENGTH) & 0xff);
    hc.rt = hc.base + (rd(hc.base, CAP_RTSOFF) & ~0x1fu) + 0x0; /* interrupter 0 at +0x20 */
    hc.db = hc.base + (rd(hc.base, CAP_DBOFF) & ~0x3u);
    u32 params1 = rd(hc.base, CAP_HCSPARAMS1);
    u32 params2 = rd(hc.base, CAP_HCSPARAMS2);
    hc.max_slots = params1 & 0xff;
    hc.max_ports = params1 >> 24;
    if (hc.max_slots > MAX_SLOTS)
        hc.max_slots = MAX_SLOTS;
    if (hc.max_ports > MAX_PORTS)
        hc.max_ports = MAX_PORTS;
    hc.csz = (rd(hc.base, CAP_HCCPARAMS1) & (1u << 2)) ? 64 : 32;
    hc.scratch_count = ((params2 >> 21) & 0x1f) << 5 | params2 >> 27;

    hc.command_lock = spk_mutex_alloc();
    hc.topology_lock = spk_mutex_alloc();
    hc.command_done = spk_completion_alloc();
    for (unsigned int i = 1; i <= hc.max_ports; i++)
        hc.port_done[i] = spk_completion_alloc();
    if (!hc.command_lock || !hc.topology_lock || !hc.command_done)
        goto nomem;

    /* Halt and reset. */
    wr(hc.op, OP_USBCMD, rd(hc.op, OP_USBCMD) & ~CMD_RS);
    if (wait_bits(hc.op, OP_USBSTS, STS_HCH, STS_HCH, 100))
        goto timeout;
    wr(hc.op, OP_USBCMD, CMD_HCRST);
    if (wait_bits(hc.op, OP_USBCMD, CMD_HCRST, 0, 1000) ||
        wait_bits(hc.op, OP_USBSTS, STS_CNR, 0, 1000))
        goto timeout;
    wr(hc.op, OP_CONFIG, hc.max_slots);

    hc.dcbaa = dma_alloc(8 * (MAX_SLOTS + 1), 64, &hc.dcbaa_dma);
    if (!hc.dcbaa)
        goto nomem;
    if (hc.scratch_count) {
        if (hc.scratch_count > 64)
            goto nomem;
        hc.scratch_array = dma_alloc(8 * hc.scratch_count, 64, &hc.scratch_array_dma);
        if (!hc.scratch_array)
            goto nomem;
        for (unsigned int i = 0; i < hc.scratch_count; i++) {
            u64 page;
            scratch_pages[i] = dma_alloc(4096, 4096, &page);
            if (!scratch_pages[i])
                goto nomem;
            hc.scratch_array[i] = page;
        }
        hc.dcbaa[0] = hc.scratch_array_dma;
    }
    wr64(hc.op, OP_DCBAAP, hc.dcbaa_dma);

    if (ring_init(&hc.command))
        goto nomem;
    wr64(hc.op, OP_CRCR, hc.command.dma | 1);

    hc.events = dma_alloc(RING_TRBS * sizeof(struct trb), 64, &hc.events_dma);
    hc.erst = dma_alloc(64, 64, &hc.erst_dma);
    if (!hc.events || !hc.erst)
        goto nomem;
    hc.event_cycle = 1;
    hc.erst[0] = hc.events_dma;
    hc.erst[1] = RING_TRBS;
    wr(hc.rt, IR_ERSTSZ, 1);
    wr64(hc.rt, IR_ERDP, hc.events_dma);
    wr64(hc.rt, IR_ERSTBA, hc.erst_dma);
    wr(hc.rt, IR_IMOD, 0);

    if (spk_pci_enable_msi(pci, xhci_irq, 0, 0, 0) == 0) {
        hc.irq_mode = 1;
    } else if (spk_request_irq(pci, xhci_irq, 0) == 0) {
        hc.irq_mode = 2;
    } else {
        spk_pr_err("usb: no interrupt available for the xHCI controller");
        controller_free();
        hc.pci = 0;
        return -ENODEV;
    }
    wr(hc.rt, IR_IMAN, IMAN_IE | IMAN_IP);
    wr(hc.op, OP_USBCMD, CMD_RS | CMD_INTE);
    if (wait_bits(hc.op, OP_USBSTS, STS_HCH, 0, 100))
        goto timeout;
    spk_printk("usb: xHCI %u slots %u ports, %u-byte contexts, %s interrupts\n", hc.max_slots,
               hc.max_ports, hc.csz, hc.irq_mode == 1 ? "MSI" : "INTx");

    /* Enumerate what is already connected; later changes arrive as events. */
    spk_msleep(20);
    spk_mutex_lock(hc.topology_lock);
    for (unsigned int port = 1; port <= hc.max_ports; port++)
        if (rd(hc.op, OP_PORTSC(port)) & PORT_CCS)
            check_port(port);
    spk_mutex_unlock(hc.topology_lock);
    return 0;
nomem:
    spk_pr_err("usb: out of memory during controller setup");
    controller_free();
    hc.pci = 0;
    return -ENOMEM;
timeout:
    spk_pr_err("usb: controller did not respond to reset/run");
    controller_free();
    hc.pci = 0;
    return -ETIMEDOUT;
}

static void xhci_remove(struct spk_pci_device *pci)
{
    if (pci != hc.pci)
        return;
    spk_mutex_lock(hc.topology_lock);
    hc.shutting_down = 1;
    for (unsigned int port = 1; port <= hc.max_ports; port++)
        detach(port);
    spk_mutex_unlock(hc.topology_lock);
    wr(hc.op, OP_USBCMD, 0);
    wait_bits(hc.op, OP_USBSTS, STS_HCH, STS_HCH, 100);
    controller_free();
    hc.pci = 0;
}

static const struct spk_pci_device_id xhci_ids[] = {
    /* Any vendor: PCI class 0x0c0330 (serial bus, USB, xHCI). */
    {SPK_PCI_ANY_ID, SPK_PCI_ANY_ID, SPK_PCI_ANY_ID, SPK_PCI_ANY_ID, 0x0c0330, 0xffffff, 0},
    {0, 0, 0, 0, 0, 0, 0},
};

static const struct spk_pci_driver xhci_driver = {
    .name = "xhci_hcd",
    .id_table = xhci_ids,
    .probe = xhci_probe,
    .remove = xhci_remove,
};

static int usb_init(void)
{
    if (spk_hal_version() < 1) {
        spk_pr_err("usb: kernel HAL too old");
        return -ENODEV;
    }
    return spk_pci_driver_register(&xhci_driver);
}

static void usb_exit(void)
{
    spk_pci_driver_unregister(&xhci_driver);
}

SPK_MODULE("usb", "1.0", "GPL", usb_init, usb_exit);
