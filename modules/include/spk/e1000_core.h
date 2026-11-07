/*
 *
 *       modules/include/spk/e1000_core.h
 *       Shared 8254x/82574 implementation for the e1000 and e1000e modules
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 * Included by exactly one .c per module; every symbol is static, so the two
 * drivers can coexist in one kernel.
 */

#ifndef SPK_E1000_CORE_H
#define SPK_E1000_CORE_H

#include "spk/module.h"
#include "spk/e1000_regs.h"

#ifndef E1000_DRIVER_LABEL
#define E1000_DRIVER_LABEL "e1000"
#endif
#ifndef E1000_USE_MSIX
#define E1000_USE_MSIX 0
#endif
#ifndef E1000_DISABLE_NS
#define E1000_DISABLE_NS 0
#endif
/* The 82574 (e1000e) moved the EERD fields: DONE is bit 1 and the address
 * starts at bit 2, unlike the 8254x layout. */
#ifndef E1000_EERD_82574_LAYOUT
#define E1000_EERD_82574_LAYOUT 0
#endif

#if E1000_EERD_82574_LAYOUT
#define E1000_EERD_START_BIT 0x1u
#define E1000_EERD_DONE_BIT 0x2u
#define E1000_EERD_ADDR_POS 2u
#else
#define E1000_EERD_START_BIT E1000_EERD_START
#define E1000_EERD_DONE_BIT E1000_EERD_DONE
#define E1000_EERD_ADDR_POS E1000_EERD_ADDR_SHIFT
#endif

/*
 *
 *       modules/e1000/e1000.c
 *       8254x (82540EM) gigabit Ethernet driver
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 * Modelled on drivers/net/ethernet/intel/e1000/e1000_main.c and e1000_hw.c:
 * legacy 16-byte descriptor rings, EERD NVM access, one NAPI instance, a
 * two-second watchdog that keeps the link and TCTL_EN alive.
 */

#define E1000_VENDOR 0x8086u
#define E1000_DEV_82540EM 0x100eu
#define E1000_DEV_82540EP 0x1016u
#define E1000_DEV_82541 0x100fu
#define E1000_DEV_82541GI 0x1013u
#define E1000_DEV_82547 0x1019u

#define ETH_ZLEN 60u
#define ETH_DATA_LEN 1500u
#define IFF_PROMISC 0x100u
#define IFF_ALLMULTI 0x200u
#define MAX_ADAPTERS 8

struct e1000_adapter {
    struct spk_pci_device *pci;
    struct spk_netdev *netdev;
    struct spk_napi *napi;
    void *mmio;
    unsigned char mac[6];
    struct e1000_tx_desc *tx_ring;
    struct e1000_rx_desc *rx_ring;
    unsigned char *tx_buffers;
    unsigned char *rx_buffers;
    unsigned long long tx_ring_dma;
    unsigned long long rx_ring_dma;
    unsigned long long tx_buffers_dma;
    unsigned long long rx_buffers_dma;
    unsigned int tx_head;
    unsigned int tx_tail;
    unsigned int rx_clean;
    unsigned int opened;
    unsigned int carrier;
    unsigned int irq_mode; /* 0 none, 1 legacy, 2 MSI, 3 MSI-X */
    unsigned long long watchdog;
    unsigned int tx_timeouts;
};

static struct e1000_adapter *e1000_adapters[MAX_ADAPTERS];

static unsigned int e1000_read(struct e1000_adapter *adapter, unsigned int reg)
{
    return spk_readl((const volatile void *)((unsigned char *)adapter->mmio + reg));
}

static void e1000_write(struct e1000_adapter *adapter, unsigned int reg,
                        unsigned int value)
{
    spk_writel(value, (volatile void *)((unsigned char *)adapter->mmio + reg));
}

static int e1000_read_eeprom_word(struct e1000_adapter *adapter,
                                  unsigned int address, unsigned int *value)
{
    int timeout;
    e1000_write(adapter, E1000_EERD,
                (address << E1000_EERD_ADDR_POS) | E1000_EERD_START_BIT);
    for (timeout = 0; timeout < 200; timeout++) {
        unsigned int result = e1000_read(adapter, E1000_EERD);
        if (result & E1000_EERD_DONE_BIT) {
            *value = (result >> E1000_EERD_DATA_SHIFT) & 0xffffu;
            return 0;
        }
        spk_udelay(50);
    }
    return -5;
}

static int e1000_read_mac(struct e1000_adapter *adapter)
{
    unsigned int index;
    for (index = 0; index < 3; index++) {
        unsigned int word;
        if (e1000_read_eeprom_word(adapter, index, &word) != 0)
            return -5;
        adapter->mac[index * 2] = (unsigned char)(word & 0xff);
        adapter->mac[index * 2 + 1] = (unsigned char)(word >> 8);
    }
    if ((adapter->mac[0] & 1) != 0)
        return -5;
    for (index = 0; index < 6; index++) {
        if (adapter->mac[index] != 0)
            return 0;
    }
    return -5;
}

static void e1000_set_rar(struct e1000_adapter *adapter)
{
    unsigned int low = (unsigned int)adapter->mac[0] |
                       ((unsigned int)adapter->mac[1] << 8) |
                       ((unsigned int)adapter->mac[2] << 16) |
                       ((unsigned int)adapter->mac[3] << 24);
    unsigned int high = (unsigned int)adapter->mac[4] |
                        ((unsigned int)adapter->mac[5] << 8) | E1000_RAH_AV;
    e1000_write(adapter, E1000_RA, low);
    e1000_write(adapter, E1000_RA + 4, high);
}

static int e1000_reset_hw(struct e1000_adapter *adapter)
{
    unsigned int ctrl;
    int timeout;
#if E1000_DISABLE_NS
    /* e1000e: forbid no-snoop requests before resetting. */
    ctrl = e1000_read(adapter, E1000_CTRL_EXT);
    e1000_write(adapter, E1000_CTRL_EXT, ctrl | (1u << 28));
#endif
    ctrl = e1000_read(adapter, E1000_CTRL);
    e1000_write(adapter, E1000_CTRL, ctrl | E1000_CTRL_RST);
    spk_udelay(2000);
    for (timeout = 0; timeout < 100; timeout++) {
        if ((e1000_read(adapter, E1000_CTRL) & E1000_CTRL_RST) == 0)
            break;
        spk_udelay(100);
    }
    e1000_write(adapter, E1000_IMC, 0xffffffffu);
    (void)e1000_read(adapter, E1000_ICR);
    ctrl = e1000_read(adapter, E1000_CTRL);
    e1000_write(adapter, E1000_CTRL, ctrl | E1000_CTRL_SLU);
    return 0;
}

static void e1000_init_hw(struct e1000_adapter *adapter)
{
    unsigned int index;
    for (index = 0; index < 128; index++)
        e1000_write(adapter, E1000_MTA + index * 4, 0);
    e1000_set_rar(adapter);
    e1000_write(adapter, E1000_TIPG, E1000_TIPG_DEFAULT);
    e1000_write(adapter, E1000_TCTL, E1000_TCTL_DEFAULT);
    e1000_write(adapter, E1000_RCTL,
                E1000_RCTL_EN | E1000_RCTL_BAM | E1000_RCTL_SZ_2048 |
                    E1000_RCTL_SECRC);
    (void)e1000_read(adapter, E1000_STATUS);
}

static void e1000_free_rings(struct e1000_adapter *adapter)
{
    if (adapter->tx_ring) {
        spk_dma_free_coherent(adapter->tx_ring, E1000_RING_COUNT * 16);
        adapter->tx_ring = 0;
    }
    if (adapter->rx_ring) {
        spk_dma_free_coherent(adapter->rx_ring, E1000_RING_COUNT * 16);
        adapter->rx_ring = 0;
    }
    if (adapter->tx_buffers) {
        spk_dma_free_coherent(adapter->tx_buffers,
                              E1000_RING_COUNT * E1000_RX_BUFFER_SIZE);
        adapter->tx_buffers = 0;
    }
    if (adapter->rx_buffers) {
        spk_dma_free_coherent(adapter->rx_buffers,
                              E1000_RING_COUNT * E1000_RX_BUFFER_SIZE);
        adapter->rx_buffers = 0;
    }
}

static int e1000_alloc_rings(struct e1000_adapter *adapter)
{
    unsigned int index;
    adapter->tx_ring = spk_dma_alloc_coherent(
        E1000_RING_COUNT * 16, 4096, &adapter->tx_ring_dma);
    adapter->rx_ring = spk_dma_alloc_coherent(
        E1000_RING_COUNT * 16, 4096, &adapter->rx_ring_dma);
    adapter->tx_buffers = spk_dma_alloc_coherent(
        E1000_RING_COUNT * E1000_RX_BUFFER_SIZE, 4096,
        &adapter->tx_buffers_dma);
    adapter->rx_buffers = spk_dma_alloc_coherent(
        E1000_RING_COUNT * E1000_RX_BUFFER_SIZE, 4096,
        &adapter->rx_buffers_dma);
    if (!adapter->tx_ring || !adapter->rx_ring || !adapter->tx_buffers ||
        !adapter->rx_buffers) {
        e1000_free_rings(adapter);
        return -12;
    }
    memset(adapter->tx_ring, 0, E1000_RING_COUNT * 16);
    memset(adapter->rx_ring, 0, E1000_RING_COUNT * 16);
    for (index = 0; index < E1000_RING_COUNT; index++)
        adapter->rx_ring[index].buffer_addr =
            adapter->rx_buffers_dma + index * E1000_RX_BUFFER_SIZE;
    adapter->tx_head = 0;
    adapter->tx_tail = 0;
    adapter->rx_clean = 0;
    return 0;
}

static void e1000_clean_tx(struct e1000_adapter *adapter)
{
    unsigned int freed = 0;
    while (adapter->tx_tail != adapter->tx_head) {
        struct e1000_tx_desc *descriptor = &adapter->tx_ring[adapter->tx_tail];
        if ((descriptor->upper & E1000_TXD_STAT_DD) == 0)
            break;
        descriptor->upper = 0;
        adapter->tx_tail = (adapter->tx_tail + 1) % E1000_RING_COUNT;
        freed++;
    }
    if (freed != 0)
        spk_netif_wake_queue(adapter->netdev);
}

static int e1000_clean_rx(struct e1000_adapter *adapter, int budget)
{
    int processed = 0;
    while (processed < budget) {
        unsigned int index = adapter->rx_clean;
        struct e1000_rx_desc *descriptor = &adapter->rx_ring[index];
        unsigned int length;
        struct spk_skb *skb;
        unsigned char *data;
        if ((descriptor->status & E1000_RXD_STAT_DD) == 0)
            break;
        length = descriptor->length;
        descriptor->status = 0;
        descriptor->length = 0;
        if (length >= 14 && length <= E1000_RX_BUFFER_SIZE) {
            skb = spk_skb_alloc(64, length);
            if (!skb)
                break;
            data = spk_skb_put(skb, length);
            if (!data) {
                spk_skb_free(skb);
                break;
            }
            memcpy(data, adapter->rx_buffers + index * E1000_RX_BUFFER_SIZE,
                   length);
            if (spk_eth_type_trans(skb, adapter->netdev) == 0)
                spk_skb_free(skb);
            else
                spk_netif_rx(adapter->netdev, skb);
        }
        adapter->rx_clean = (index + 1) % E1000_RING_COUNT;
        processed++;
        spk_writel((adapter->rx_clean + E1000_RING_COUNT - 1) %
                       E1000_RING_COUNT,
                   (volatile void *)((unsigned char *)adapter->mmio +
                                     E1000_RDT));
    }
    return processed;
}

static struct e1000_adapter *e1000_adapter_for_napi(struct spk_napi *napi)
{
    unsigned int index;
    for (index = 0; index < MAX_ADAPTERS; index++) {
        if (e1000_adapters[index] && e1000_adapters[index]->napi == napi)
            return e1000_adapters[index];
    }
    return 0;
}

static int e1000_poll(struct spk_napi *napi, int budget)
{
    struct e1000_adapter *adapter = e1000_adapter_for_napi(napi);
    int work;
    if (!adapter)
        return 0;
    e1000_clean_tx(adapter);
    work = e1000_clean_rx(adapter, budget);
    if (work < budget)
        spk_napi_complete(napi);
    return work;
}

static int e1000_start_xmit(struct spk_netdev *netdev, struct spk_skb *skb)
{
    struct e1000_adapter *adapter = spk_netdev_priv(netdev);
    unsigned int length = spk_skb_len(skb);
    unsigned int next = adapter->tx_head;
    struct e1000_tx_desc *descriptor;
    if (!adapter->opened) {
        spk_skb_free(skb);
        return 0;
    }
    if ((next + 1) % E1000_RING_COUNT == adapter->tx_tail) {
        spk_netif_stop_queue(netdev);
        return 1;
    }
    if (length > E1000_RX_BUFFER_SIZE) {
        spk_skb_free(skb);
        return 0;
    }
    if (length < ETH_ZLEN)
        memset(adapter->tx_buffers + next * E1000_RX_BUFFER_SIZE, 0,
               E1000_RX_BUFFER_SIZE);
    memcpy(adapter->tx_buffers + next * E1000_RX_BUFFER_SIZE,
           spk_skb_data(skb), length);
    descriptor = &adapter->tx_ring[next];
    descriptor->buffer_addr =
        adapter->tx_buffers_dma + next * E1000_RX_BUFFER_SIZE;
    descriptor->lower =
        length | E1000_TXD_CMD_EOP | E1000_TXD_CMD_IFCS | E1000_TXD_CMD_RS;
    descriptor->upper = 0;
    spk_mb();
    adapter->tx_head = (next + 1) % E1000_RING_COUNT;
    e1000_write(adapter, E1000_TDT, adapter->tx_head);
    spk_skb_free(skb);
    return 0;
}

static void e1000_intr(int irq, void *data)
{
    struct e1000_adapter *adapter = data;
    unsigned int icr;
    (void)irq;
    if (!adapter || !adapter->opened)
        return;
    icr = e1000_read(adapter, E1000_ICR);
    if (icr != 0)
        spk_napi_schedule(adapter->napi);
}

static void e1000_watchdog(void *data)
{
    struct e1000_adapter *adapter = data;
    unsigned int status, ctrl, link;
    if (!adapter || !adapter->opened)
        return;
    status = e1000_read(adapter, E1000_STATUS);
    ctrl = e1000_read(adapter, E1000_CTRL);
    link = ((status & E1000_STATUS_LU) != 0) && ((ctrl & E1000_CTRL_LRST) == 0);
    if (link != adapter->carrier) {
        adapter->carrier = link;
        if (link)
            spk_netif_carrier_on(adapter->netdev);
        else
            spk_netif_carrier_off(adapter->netdev);
    }
    if (link) {
        unsigned int tctl = e1000_read(adapter, E1000_TCTL);
        if ((tctl & E1000_TCTL_EN) == 0)
            e1000_write(adapter, E1000_TCTL, tctl | E1000_TCTL_EN);
    }
    spk_napi_schedule(adapter->napi);
    adapter->watchdog = spk_timer_add(
        adapter->irq_mode >= 2 ? 2000000 : 50000, e1000_watchdog, adapter);
}

static int e1000_request_interrupt(struct e1000_adapter *adapter)
{
#if E1000_USE_MSIX
    if (spk_pci_enable_msix(adapter->pci, 1, e1000_intr, adapter) == 0) {
        adapter->irq_mode = 3;
        return 0;
    }
#endif
    if (spk_pci_enable_msi(adapter->pci, e1000_intr, adapter, 0, 0) == 0) {
        adapter->irq_mode = 2;
        return 0;
    }
    if (spk_request_irq(adapter->pci, e1000_intr, adapter) == 0) {
        adapter->irq_mode = 1;
        return 0;
    }
    return -5;
}

static void e1000_release_interrupt(struct e1000_adapter *adapter)
{
    if (adapter->irq_mode == 2)
        spk_pci_disable_msi(adapter->pci);
    else if (adapter->irq_mode == 1)
        spk_free_irq(adapter->pci);
    else if (adapter->irq_mode == 3)
        spk_pci_disable_msix(adapter->pci);
    adapter->irq_mode = 0;
}

static int e1000_open(struct spk_netdev *netdev)
{
    struct e1000_adapter *adapter = spk_netdev_priv(netdev);
    unsigned int ctrl;
    if (adapter->opened)
        return 0;
    if (e1000_alloc_rings(adapter) != 0)
        return -12;
    e1000_reset_hw(adapter);
    ctrl = e1000_read(adapter, E1000_CTRL);
    e1000_write(adapter, E1000_CTRL, ctrl | E1000_CTRL_SLU | E1000_CTRL_ASDE);

    e1000_write(adapter, E1000_TDLEN, E1000_RING_COUNT * 16);
    e1000_write(adapter, E1000_TDBAH, (unsigned int)(adapter->tx_ring_dma >> 32));
    e1000_write(adapter, E1000_TDBAL, (unsigned int)adapter->tx_ring_dma);
    e1000_write(adapter, E1000_TDH, 0);
    e1000_write(adapter, E1000_TDT, 0);
    e1000_write(adapter, E1000_RDLEN, E1000_RING_COUNT * 16);
    e1000_write(adapter, E1000_RDBAH, (unsigned int)(adapter->rx_ring_dma >> 32));
    e1000_write(adapter, E1000_RDBAL, (unsigned int)adapter->rx_ring_dma);
    e1000_write(adapter, E1000_RDH, 0);
    e1000_write(adapter, E1000_RDT, E1000_RING_COUNT - 1);
    e1000_init_hw(adapter);

    adapter->opened = 1;
    adapter->tx_head = 0;
    adapter->tx_tail = 0;
    adapter->rx_clean = 0;
    if (e1000_request_interrupt(adapter) != 0) {
        adapter->opened = 0;
        e1000_free_rings(adapter);
        return -5;
    }
    if (adapter->irq_mode == 3) {
        /* Route RXQ0, TXQ0 and OTHER causes to MSI-X vector 0. */
        e1000_write(adapter, E1000_IVAR,
                    E1000_IVAR_INT_ALLOC_VALID |
                        (E1000_IVAR_INT_ALLOC_VALID << 8) |
                        (E1000_IVAR_INT_ALLOC_VALID << 16) |
                        E1000_IVAR_TX_INT_EVERY_WB);
    }
    e1000_write(adapter, E1000_IMS, E1000_IMS_ENABLE);
    adapter->carrier = 0;
    e1000_watchdog(adapter);
    return 0;
}

static int e1000_stop(struct spk_netdev *netdev)
{
    struct e1000_adapter *adapter = spk_netdev_priv(netdev);
    unsigned int tctl, rctl;
    if (!adapter->opened)
        return 0;
    adapter->opened = 0;
    e1000_write(adapter, E1000_IMC, 0xffffffffu);
    (void)e1000_read(adapter, E1000_ICR);
    e1000_release_interrupt(adapter);
    if (adapter->watchdog != 0) {
        spk_timer_del(adapter->watchdog);
        adapter->watchdog = 0;
    }
    tctl = e1000_read(adapter, E1000_TCTL);
    e1000_write(adapter, E1000_TCTL, tctl & ~E1000_TCTL_EN);
    rctl = e1000_read(adapter, E1000_RCTL);
    e1000_write(adapter, E1000_RCTL, rctl & ~E1000_RCTL_EN);
    e1000_free_rings(adapter);
    spk_netif_carrier_off(netdev);
    return 0;
}

static void e1000_set_rx_mode(struct spk_netdev *netdev)
{
    struct e1000_adapter *adapter = spk_netdev_priv(netdev);
    unsigned int flags = spk_netdev_get_flags(netdev);
    unsigned int rctl = e1000_read(adapter, E1000_RCTL);
    rctl &= ~(E1000_RCTL_UPE | E1000_RCTL_MPE);
    if (flags & IFF_PROMISC)
        rctl |= E1000_RCTL_UPE | E1000_RCTL_MPE;
    else if (flags & IFF_ALLMULTI)
        rctl |= E1000_RCTL_MPE;
    e1000_write(adapter, E1000_RCTL, rctl);
}

static int e1000_set_mac(struct spk_netdev *netdev, const unsigned char *address)
{
    struct e1000_adapter *adapter = spk_netdev_priv(netdev);
    memcpy(adapter->mac, address, 6);
    e1000_set_rar(adapter);
    return 0;
}

static int e1000_change_mtu(struct spk_netdev *netdev, unsigned int mtu)
{
    (void)netdev;
    if (mtu > ETH_DATA_LEN)
        return -22;
    return 0;
}

static void e1000_tx_timeout(struct spk_netdev *netdev)
{
    struct e1000_adapter *adapter = spk_netdev_priv(netdev);
    adapter->tx_timeouts++;
    spk_napi_schedule(adapter->napi);
}

static const struct spk_net_ops e1000_net_ops = {
    .open = e1000_open,
    .stop = e1000_stop,
    .start_xmit = e1000_start_xmit,
    .set_rx_mode = e1000_set_rx_mode,
    .set_mac_address = e1000_set_mac,
    .change_mtu = e1000_change_mtu,
    .tx_timeout = e1000_tx_timeout,
};

static int e1000_probe(struct spk_pci_device *pci)
{
    struct e1000_adapter *adapter;
    unsigned int slot;
    unsigned long long bar;
    adapter = spk_kzalloc(sizeof(*adapter));
    if (!adapter)
        return -12;
    adapter->pci = pci;
    for (slot = 0; slot < MAX_ADAPTERS; slot++) {
        if (!e1000_adapters[slot]) {
            e1000_adapters[slot] = adapter;
            break;
        }
    }
    if (slot == MAX_ADAPTERS)
        goto fail;
    if (spk_pci_enable_device(pci) != 0)
        goto fail;
    spk_pci_set_master(pci);
    bar = spk_pci_resource_start(pci, 0);
    if (bar == 0)
        goto fail;
    adapter->mmio = spk_ioremap(bar, E1000_REG_BAR_SIZE);
    if (!adapter->mmio)
        goto fail;
    if (e1000_reset_hw(adapter) != 0 || e1000_read_mac(adapter) != 0) {
        spk_pr_err(E1000_DRIVER_LABEL ": EEPROM read failed\n");
        goto fail;
    }
    adapter->netdev = spk_netdev_alloc("eth%d", adapter->mac, 6, adapter,
                                       &e1000_net_ops);
    if (!adapter->netdev)
        goto fail;
    adapter->napi = spk_napi_add(adapter->netdev, e1000_poll, 64);
    if (!adapter->napi)
        goto fail;
    if (spk_netdev_register(adapter->netdev) != 0)
        goto fail;
    spk_pr_info(E1000_DRIVER_LABEL ": device attached\n");
    return 0;

fail:
    if (slot < MAX_ADAPTERS)
        e1000_adapters[slot] = 0;
    if (adapter->napi && adapter->netdev)
        spk_napi_del(adapter->netdev, adapter->napi);
    if (adapter->netdev)
        spk_netdev_free(adapter->netdev);
    if (adapter->mmio)
        spk_iounmap(adapter->mmio, E1000_REG_BAR_SIZE);
    spk_kfree(adapter);
    return -5;
}

static void e1000_remove(struct spk_pci_device *pci)
{
    unsigned int slot;
    for (slot = 0; slot < MAX_ADAPTERS; slot++) {
        struct e1000_adapter *adapter = e1000_adapters[slot];
        if (!adapter || adapter->pci != pci)
            continue;
        e1000_adapters[slot] = 0;
        if (adapter->netdev) {
            spk_netdev_unregister(adapter->netdev);
            if (adapter->napi)
                spk_napi_del(adapter->netdev, adapter->napi);
            spk_netdev_free(adapter->netdev);
        }
        if (adapter->mmio)
            spk_iounmap(adapter->mmio, E1000_REG_BAR_SIZE);
        spk_kfree(adapter);
        return;
    }
}

static struct spk_pci_driver e1000_core_driver = {
    .name = 0,
    .id_table = 0,
    .probe = e1000_probe,
    .remove = e1000_remove,
};

static int e1000_core_init(const char *name,
                           const struct spk_pci_device_id *ids)
{
    int result;
    e1000_core_driver.name = name;
    e1000_core_driver.id_table = ids;
    result = spk_pci_driver_register(&e1000_core_driver);
    if (result != 0)
        spk_pr_err(E1000_DRIVER_LABEL ": PCI driver registration failed\n");
    return result;
}

static void e1000_core_exit(void)
{
    spk_pci_driver_unregister(&e1000_core_driver);
}

#endif /* SPK_E1000_CORE_H */
