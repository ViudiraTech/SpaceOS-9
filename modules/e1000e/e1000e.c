/*
 *
 *       modules/e1000e/e1000e.c
 *       82574L and related PCI Express gigabit Ethernet driver module
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 * The e1000e parts differ from e1000 in NVM/PHY defaults and interrupt
 * capability: MSI-X is tried first, then MSI, then INTx. The descriptor rings
 * use the legacy layout the 82574L still supports.
 */

#include "spk/module.h"

#define E1000_DRIVER_LABEL "e1000e"
#define E1000_USE_MSIX 1
#define E1000_DISABLE_NS 1
#define E1000_EERD_82574_LAYOUT 1
#include "spk/e1000_core.h"

static const struct spk_pci_device_id e1000e_ids[] = {
    { 0x8086u, 0x10d3u, 0, 0, 0, 0, 0 }, /* 82574L */
    { 0x8086u, 0x10d2u, 0, 0, 0, 0, 0 },
    { 0x8086u, 0x10eau, 0, 0, 0, 0, 0 }, /* 82577LM */
    { 0x8086u, 0x10ebu, 0, 0, 0, 0, 0 },
    { 0x8086u, 0x1502u, 0, 0, 0, 0, 0 }, /* 82579LM */
    { 0x8086u, 0x1503u, 0, 0, 0, 0, 0 },
    { 0x8086u, 0x153au, 0, 0, 0, 0, 0 }, /* I217-LM */
    { 0x8086u, 0x153bu, 0, 0, 0, 0, 0 },
    { 0, 0, 0, 0, 0, 0, 0 },
};

static int e1000e_init(void)
{
    return e1000_core_init("e1000e", e1000e_ids);
}

static void e1000e_exit(void)
{
    e1000_core_exit();
}

SPK_MODULE("e1000e", "1.0", "GPL", e1000e_init, e1000e_exit);
