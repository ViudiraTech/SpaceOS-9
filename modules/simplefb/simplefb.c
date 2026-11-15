/*
 *
 *       modules/simplefb/simplefb.c
 *       DRM/KMS driver for the firmware framebuffer (Linux simpledrm)
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

/*
 * Exposes the framebuffer the bootloader set up (UEFI GOP via Limine) as a
 * full DRM device, so the system has a working display with no GPU driver.
 * The DRM core (HAL display class) provides atomic/legacy KMS, dumb
 * buffers, damage clips and simulated vblank; this module only presents
 * damaged rectangles of a shadow framebuffer to the scanout memory.
 *
 * Design follows Linux simpledrm / drm_format_helper:
 *  - shadow buffering: clients draw into system memory; the scanout is
 *    write-combined (or uncached) memory that is never read back;
 *  - formats: the native scanout format plus emulated XRGB8888/ARGB8888,
 *    converted line by line while copying;
 *  - only the damaged clips are touched.
 * Converters work on 2 or 4 pixels at a time and assemble 64-bit stores so
 * the scanout sees few, full, sequential write transactions.
 */

#include <spk/module.h>

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;

#define ENODEV 19
#define EINVAL 22

typedef void (*line_fn)(u8 *dst, const u8 *src, unsigned int pixels);

struct native {
    u32 fourcc; /* 0 when only the generic packer can describe it */
    unsigned int cpp;
};

static struct {
    struct spk_sysfb fb;
    struct native native;
    struct spk_display *display;
    u32 formats[4];
    unsigned int format_count;
    u64 frames, pixels;
} sf;

/* ---- line converters ---------------------------------------------------- */

static void line_copy4(u8 *dst, const u8 *src, unsigned int pixels)
{
    memcpy(dst, src, (unsigned long)pixels * 4);
}

static void line_copy3(u8 *dst, const u8 *src, unsigned int pixels)
{
    memcpy(dst, src, (unsigned long)pixels * 3);
}

static void line_copy2(u8 *dst, const u8 *src, unsigned int pixels)
{
    memcpy(dst, src, (unsigned long)pixels * 2);
}

/* Modules build with -fno-builtin, so plain memcpy() is a call; the
 * builtin form compiles to a single (unaligned-safe) load or store. */
#define bcopy_small(d, s, n) __builtin_memcpy((d), (s), (n))

static u32 load32(const u8 *p)
{
    u32 v;
    bcopy_small(&v, p, 4);
    return v;
}

static void store32(u8 *p, u32 v) { bcopy_small(p, &v, 4); }
static void store64(u8 *p, u64 v) { bcopy_small(p, &v, 8); }

/* XRGB8888 -> XBGR8888: swap R and B, two pixels per 64-bit store. */
static u32 swap_rb(u32 p) { return (p & 0xff00ff00u) | (p >> 16 & 0xffu) | (p & 0xffu) << 16; }

static void line_xrgb_to_xbgr(u8 *dst, const u8 *src, unsigned int pixels)
{
    unsigned int i = 0;
    for (; i + 2 <= pixels; i += 2)
        store64(dst + 4 * i, (u64)swap_rb(load32(src + 4 * i)) |
                                 (u64)swap_rb(load32(src + 4 * i + 4)) << 32);
    for (; i < pixels; i++)
        store32(dst + 4 * i, swap_rb(load32(src + 4 * i)));
}

/* XRGB8888 -> RGB565, four pixels per 64-bit store. */
static u64 to565(u32 p) { return (p >> 8 & 0xf800u) | (p >> 5 & 0x07e0u) | (p >> 3 & 0x001fu); }

static void line_xrgb_to_rgb565(u8 *dst, const u8 *src, unsigned int pixels)
{
    unsigned int i = 0;
    for (; i + 4 <= pixels; i += 4)
        store64(dst + 2 * i, to565(load32(src + 4 * i)) | to565(load32(src + 4 * i + 4)) << 16 |
                                 to565(load32(src + 4 * i + 8)) << 32 |
                                 to565(load32(src + 4 * i + 12)) << 48);
    for (; i < pixels; i++) {
        u16 v = (u16)to565(load32(src + 4 * i));
        bcopy_small(dst + 2 * i, &v, 2);
    }
}

/* XRGB8888 -> RGB888 (memory B,G,R): four pixels become three words. */
static void line_xrgb_to_rgb888(u8 *dst, const u8 *src, unsigned int pixels)
{
    unsigned int i = 0;
    for (; i + 4 <= pixels; i += 4) {
        u32 a = load32(src + 4 * i) & 0xffffffu, b = load32(src + 4 * i + 4) & 0xffffffu;
        u32 c = load32(src + 4 * i + 8) & 0xffffffu, d = load32(src + 4 * i + 12) & 0xffffffu;
        store32(dst + 3 * i, a | b << 24);
        store32(dst + 3 * i + 4, b >> 8 | c << 16);
        store32(dst + 3 * i + 8, c >> 16 | d << 8);
    }
    for (; i < pixels; i++) {
        u32 p = load32(src + 4 * i);
        dst[3 * i] = (u8)p, dst[3 * i + 1] = (u8)(p >> 8), dst[3 * i + 2] = (u8)(p >> 16);
    }
}

/* XRGB8888 -> BGR888 (memory R,G,B). */
static void line_xrgb_to_bgr888(u8 *dst, const u8 *src, unsigned int pixels)
{
    unsigned int i = 0;
    for (; i + 4 <= pixels; i += 4) {
        u32 a = swap_rb(load32(src + 4 * i)) & 0xffffffu;
        u32 b = swap_rb(load32(src + 4 * i + 4)) & 0xffffffu;
        u32 c = swap_rb(load32(src + 4 * i + 8)) & 0xffffffu;
        u32 d = swap_rb(load32(src + 4 * i + 12)) & 0xffffffu;
        store32(dst + 3 * i, a | b << 24);
        store32(dst + 3 * i + 4, b >> 8 | c << 16);
        store32(dst + 3 * i + 8, c >> 16 | d << 8);
    }
    for (; i < pixels; i++) {
        u32 p = load32(src + 4 * i);
        dst[3 * i] = (u8)(p >> 16), dst[3 * i + 1] = (u8)(p >> 8), dst[3 * i + 2] = (u8)p;
    }
}

/* XRGB8888 -> XRGB2101010: replicate the top bits into the new low bits. */
static u32 to2101010(u32 p)
{
    u32 r = p >> 16 & 0xff, g = p >> 8 & 0xff, b = p & 0xff;
    r = r << 2 | r >> 6, g = g << 2 | g >> 6, b = b << 2 | b >> 6;
    return r << 20 | g << 10 | b;
}

static void line_xrgb_to_2101010(u8 *dst, const u8 *src, unsigned int pixels)
{
    unsigned int i = 0;
    for (; i + 2 <= pixels; i += 2)
        store64(dst + 4 * i, (u64)to2101010(load32(src + 4 * i)) |
                                 (u64)to2101010(load32(src + 4 * i + 4)) << 32);
    for (; i < pixels; i++)
        store32(dst + 4 * i, to2101010(load32(src + 4 * i)));
}

/* RGB565 -> XRGB8888 with bit replication. */
static u32 from565(u32 v)
{
    u32 r = v >> 11 & 0x1f, g = v >> 5 & 0x3f, b = v & 0x1f;
    return (r << 3 | r >> 2) << 16 | (g << 2 | g >> 4) << 8 | (b << 3 | b >> 2);
}

static void line_rgb565_to_xrgb(u8 *dst, const u8 *src, unsigned int pixels)
{
    unsigned int i = 0;
    for (; i + 2 <= pixels; i += 2) {
        u32 pair = load32(src + 2 * i);
        store64(dst + 4 * i, (u64)from565(pair & 0xffff) | (u64)from565(pair >> 16) << 32);
    }
    for (; i < pixels; i++) {
        u16 v;
        bcopy_small(&v, src + 2 * i, 2);
        store32(dst + 4 * i, from565(v));
    }
}

/* Any 16/24/32-bpp mask layout: pack each channel from XRGB8888. */
static u32 pack_generic(u32 p)
{
    const struct spk_sysfb *f = &sf.fb;
    u32 r = p >> 16 & 0xff, g = p >> 8 & 0xff, b = p & 0xff;
    return (r >> (8 - f->red_size)) << f->red_shift | (g >> (8 - f->green_size)) << f->green_shift |
           (b >> (8 - f->blue_size)) << f->blue_shift;
}

static void line_generic(u8 *dst, const u8 *src, unsigned int pixels)
{
    unsigned int cpp = sf.native.cpp;
    for (unsigned int i = 0; i < pixels; i++) {
        u32 v = pack_generic(load32(src + 4 * i));
        if (cpp == 4)
            store32(dst + 4 * i, v);
        else if (cpp == 3)
            dst[3 * i] = (u8)v, dst[3 * i + 1] = (u8)(v >> 8), dst[3 * i + 2] = (u8)(v >> 16);
        else
            bcopy_small(dst + 2 * i, &v, 2);
    }
}

static line_fn converter(u32 src_format)
{
    u32 dst = sf.native.fourcc;
    if (src_format == dst && dst)
        return sf.native.cpp == 4 ? line_copy4 : sf.native.cpp == 3 ? line_copy3 : line_copy2;
    if (src_format == SPK_FORMAT_RGB565)
        return dst == SPK_FORMAT_XRGB8888 ? line_rgb565_to_xrgb : 0;
    if (src_format != SPK_FORMAT_XRGB8888 && src_format != SPK_FORMAT_ARGB8888)
        return 0;
    switch (dst) {
    case SPK_FORMAT_XRGB8888:
        return line_copy4;
    case SPK_FORMAT_XBGR8888:
        return line_xrgb_to_xbgr;
    case SPK_FORMAT_RGB565:
        return line_xrgb_to_rgb565;
    case SPK_FORMAT_RGB888:
        return line_xrgb_to_rgb888;
    case SPK_FORMAT_BGR888:
        return line_xrgb_to_bgr888;
    case SPK_FORMAT_XRGB2101010:
        return line_xrgb_to_2101010;
    default:
        return line_generic;
    }
}

/* ---- display callbacks ----------------------------------------------------- */

static int sf_check(void *priv, unsigned int format)
{
    (void)priv;
    return converter(format) ? 0 : -EINVAL;
}

static int sf_enable(void *priv)
{
    (void)priv;
    return 0;
}

static void sf_disable(void *priv)
{
    (void)priv;
    /* The console repaints the whole screen when it takes it back. */
}

static unsigned int src_cpp(u32 format)
{
    return format == SPK_FORMAT_RGB565 ? 2 : format == SPK_FORMAT_RGB888 || format == SPK_FORMAT_BGR888 ? 3 : 4;
}

static void sf_update(void *priv, const struct spk_display_fb *fb, const struct spk_rect *src,
                      const struct spk_rect *clips, unsigned int count)
{
    (void)priv;
    line_fn line = converter(fb->format);
    if (!line)
        return;
    unsigned int scpp = src_cpp(fb->format), dcpp = sf.native.cpp;
    u8 *scanout = sf.fb.virt;
    for (unsigned int n = 0; n < count; n++) {
        struct spk_rect c = clips[n];
        /* Screen coordinates; clamp defensively to the scanout. */
        int x = c.x1 - src->x1, y = c.y1 - src->y1;
        int w = c.x2 - c.x1, h = c.y2 - c.y1;
        if (x < 0 || y < 0 || w <= 0 || h <= 0)
            continue;
        if ((unsigned int)(x + w) > sf.fb.width)
            w = (int)sf.fb.width - x;
        if ((unsigned int)(y + h) > sf.fb.height)
            h = (int)sf.fb.height - y;
        if (w <= 0 || h <= 0)
            continue;
        const u8 *s = (const u8 *)fb->pixels + (unsigned long)c.y1 * fb->pitch + (unsigned long)c.x1 * scpp;
        u8 *d = scanout + (unsigned long)y * sf.fb.pitch + (unsigned long)x * dcpp;
        for (int row = 0; row < h; row++) {
            line(d, s, (unsigned int)w);
            s += fb->pitch;
            d += sf.fb.pitch;
        }
        sf.pixels += (u64)w * (u64)h;
    }
    sf.frames++;
}

static void sf_lastclose(void *priv)
{
    (void)priv;
}

static const struct spk_display_ops ops = {
    .check = sf_check,
    .enable = sf_enable,
    .disable = sf_disable,
    .update = sf_update,
    .lastclose = sf_lastclose,
};

/* ---- probe -------------------------------------------------------------------- */

static int masks(unsigned int rs, unsigned int rsh, unsigned int gs, unsigned int gsh,
                 unsigned int bs, unsigned int bsh)
{
    const struct spk_sysfb *f = &sf.fb;
    return f->red_size == rs && f->red_shift == rsh && f->green_size == gs &&
           f->green_shift == gsh && f->blue_size == bs && f->blue_shift == bsh;
}

static int detect_native(void)
{
    const struct spk_sysfb *f = &sf.fb;
    sf.native.cpp = f->bpp / 8;
    sf.native.fourcc = 0;
    if (f->bpp == 32 && masks(8, 16, 8, 8, 8, 0))
        sf.native.fourcc = SPK_FORMAT_XRGB8888;
    else if (f->bpp == 32 && masks(8, 0, 8, 8, 8, 16))
        sf.native.fourcc = SPK_FORMAT_XBGR8888;
    else if (f->bpp == 32 && masks(10, 20, 10, 10, 10, 0))
        sf.native.fourcc = SPK_FORMAT_XRGB2101010;
    else if (f->bpp == 24 && masks(8, 16, 8, 8, 8, 0))
        sf.native.fourcc = SPK_FORMAT_RGB888;
    else if (f->bpp == 24 && masks(8, 0, 8, 8, 8, 16))
        sf.native.fourcc = SPK_FORMAT_BGR888;
    else if (f->bpp == 16 && masks(5, 11, 6, 5, 5, 0))
        sf.native.fourcc = SPK_FORMAT_RGB565;
    else if (f->bpp != 16 && f->bpp != 24 && f->bpp != 32)
        return -ENODEV;
    if (f->red_size > 8 && !sf.native.fourcc)
        return -ENODEV; /* the generic packer handles at most 8-bit channels */
    return 0;
}

static int simplefb_init(void)
{
    if (spk_hal_version() < 2)
        return -ENODEV;
    int error = spk_sysfb_get(&sf.fb);
    if (error)
        return error; /* -EBUSY: a native GPU driver owns the display */
    if (!sf.fb.virt || sf.fb.width == 0 || sf.fb.height == 0)
        return -ENODEV;
    error = detect_native();
    if (error)
        return error;
    /* Native format first (best for clients that can produce it), then the
     * universally supported XRGB8888 and ARGB8888 via conversion. */
    sf.format_count = 0;
    if (sf.native.fourcc && sf.native.fourcc != SPK_FORMAT_XRGB8888)
        sf.formats[sf.format_count++] = sf.native.fourcc;
    sf.formats[sf.format_count++] = SPK_FORMAT_XRGB8888;
    sf.formats[sf.format_count++] = SPK_FORMAT_ARGB8888;
    if (sf.native.fourcc == SPK_FORMAT_XRGB8888)
        sf.formats[sf.format_count++] = SPK_FORMAT_RGB565;
    struct spk_display_mode mode = {
        .width = sf.fb.width,
        .height = sf.fb.height,
        .refresh_hz = 60,
        /* 96 DPI when the firmware gives no physical size. */
        .width_mm = sf.fb.width * 254 / 960,
        .height_mm = sf.fb.height * 254 / 960,
    };
    sf.display = spk_display_register("simpledrm", "DRM driver for the firmware framebuffer", &mode,
                                      sf.formats, sf.format_count,
                                      SPK_DISPLAY_PREFER_SHADOW | SPK_DISPLAY_TAKES_CONSOLE, &ops, 0);
    if (!sf.display)
        return -ENODEV;
    spk_printk("simplefb: %ux%u %u bpp pitch %u, native %s\n", sf.fb.width, sf.fb.height,
               sf.fb.bpp, sf.fb.pitch, sf.native.fourcc ? "fourcc" : "mask-packed");
    return 0;
}

static void simplefb_exit(void)
{
    if (sf.display)
        spk_display_unregister(sf.display);
    sf.display = 0;
}

SPK_MODULE("simplefb", "1.0", "GPL", simplefb_init, simplefb_exit);
