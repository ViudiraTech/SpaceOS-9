/*
 *
 *       src/arch/x86_64/apic/mod.rs
 *       x86 interrupt controllers: LAPIC, I/O APIC, legacy PIC and timer
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! x86 interrupt controllers: LAPIC, I/O APIC, legacy PIC and timer.

mod ioapic;
mod local;
mod pic;
pub mod timer;

use crate::{
    boot,
    hardware::acpi::{Acpi, Madt, MadtEntry},
    irq::{self, Handler, IrqError},
    sync::SpinLock,
};
use alloc::vec::Vec;
pub use ioapic::{Polarity, Trigger};

const FIRST_DYNAMIC: u8 = 0x40;
const LAST_DYNAMIC: u8 = 0xef;

#[derive(Clone, Copy, Debug)]
pub struct Route {
    pub gsi: u32,
    pub vector: u8,
}

struct Allocations {
    used: [bool; 256],
    routes: Vec<Route>,
}
static ALLOCATIONS: SpinLock<Allocations> = SpinLock::new(Allocations {
    used: [false; 256],
    routes: Vec::new(),
});

pub fn init_bsp() -> Result<(), IrqError> {
    let rsdp = boot::rsdp_address().ok_or(IrqError::NoController)?;
    let acpi = Acpi::from_rsdp(rsdp).map_err(|_| IrqError::Invalid)?;
    let madt = acpi
        .madt()
        .map_err(|_| IrqError::Invalid)?
        .ok_or(IrqError::NoController)?;
    pic::mask_all();
    let apic_id = local::init_bsp(madt.local_apic_address())?;
    configure_lint(&madt, apic_id)?;
    let ioapics = ioapic::init(&madt)?;
    for entry in madt.entries() {
        if let MadtEntry::NmiSource { gsi, flags } = entry.map_err(|_| IrqError::Invalid)? {
            let (polarity, trigger) = decode_flags(flags)?;
            ioapic::route(gsi, ioapic::Delivery::Nmi, apic_id, polarity, trigger)?;
            ioapic::set_mask(gsi, false)?;
        }
    }
    crate::kinfo!(
        "APIC: {} id={} IOAPICs={}",
        if local::is_x2apic() {
            "x2APIC"
        } else {
            "xAPIC"
        },
        apic_id,
        ioapics
    );
    Ok(())
}

fn configure_lint(madt: &Madt, apic_id: u32) -> Result<(), IrqError> {
    let mut uid = None;
    for entry in madt.entries() {
        match entry.map_err(|_| IrqError::Invalid)? {
            MadtEntry::LocalApic {
                uid: id,
                apic_id: apic,
                enabled: true,
            } if u32::from(apic) == apic_id => uid = Some(u32::from(id)),
            MadtEntry::X2Apic {
                uid: id,
                apic_id: apic,
                enabled: true,
            } if apic == apic_id => uid = Some(id),
            _ => {}
        }
    }
    for entry in madt.entries() {
        match entry.map_err(|_| IrqError::Invalid)? {
            MadtEntry::LocalApicNmi {
                uid: id,
                lint,
                flags,
            } if id == 0xff || uid == Some(u32::from(id)) => local::set_lint_nmi(lint, flags)?,
            MadtEntry::X2ApicNmi {
                uid: id,
                lint,
                flags,
            } if id == u32::MAX || uid == Some(id) => local::set_lint_nmi(lint, flags)?,
            _ => {}
        }
    }
    Ok(())
}

fn decode_flags(flags: u16) -> Result<(Polarity, Trigger), IrqError> {
    if flags & !0xf != 0 {
        return Err(IrqError::Invalid);
    }
    let polarity = match flags & 3 {
        0 | 1 => Polarity::High,
        3 => Polarity::Low,
        _ => return Err(IrqError::Invalid),
    };
    let trigger = match (flags >> 2) & 3 {
        0 | 1 => Trigger::Edge,
        3 => Trigger::Level,
        _ => return Err(IrqError::Invalid),
    };
    Ok((polarity, trigger))
}

fn allocate(handler: Handler) -> Result<u8, IrqError> {
    let mut state = ALLOCATIONS.lock();
    let vector = (FIRST_DYNAMIC..=LAST_DYNAMIC)
        .find(|v| !state.used[*v as usize])
        .ok_or(IrqError::NoVector)?;
    irq::register(u32::from(vector), handler)?;
    state.used[vector as usize] = true;
    Ok(vector)
}

fn release(vector: u8) -> Result<(), IrqError> {
    let mut state = ALLOCATIONS.lock();
    if !(FIRST_DYNAMIC..=LAST_DYNAMIC).contains(&vector) || !state.used[vector as usize] {
        return Err(IrqError::Invalid);
    }
    irq::unregister(u32::from(vector))?;
    state.used[vector as usize] = false;
    Ok(())
}

pub fn request_gsi(
    gsi: u32,
    polarity: Polarity,
    trigger: Trigger,
    handler: Handler,
) -> Result<Route, IrqError> {
    if !local::ready() {
        return Err(IrqError::NoController);
    }
    let mut state = ALLOCATIONS.lock();
    if state.routes.iter().any(|route| route.gsi == gsi) {
        return Err(IrqError::InUse);
    }
    let vector = (FIRST_DYNAMIC..=LAST_DYNAMIC)
        .find(|v| !state.used[*v as usize])
        .ok_or(IrqError::NoVector)?;
    irq::register(u32::from(vector), handler)?;
    if let Err(error) = ioapic::route(
        gsi,
        ioapic::Delivery::Fixed(vector),
        local::id(),
        polarity,
        trigger,
    ) {
        let _ = irq::unregister(u32::from(vector));
        return Err(error);
    }
    if let Err(error) = ioapic::set_mask(gsi, false) {
        let _ = irq::unregister(u32::from(vector));
        return Err(error);
    }
    state.used[vector as usize] = true;
    let route = Route { gsi, vector };
    state.routes.push(route);
    Ok(route)
}

pub fn request_isa(source: u8, handler: Handler) -> Result<Route, IrqError> {
    if source >= 16 {
        return Err(IrqError::Invalid);
    }
    let rsdp = boot::rsdp_address().ok_or(IrqError::NoController)?;
    let acpi = Acpi::from_rsdp(rsdp).map_err(|_| IrqError::Invalid)?;
    let madt = acpi
        .madt()
        .map_err(|_| IrqError::Invalid)?
        .ok_or(IrqError::NoController)?;
    let mut override_data = None;
    for entry in madt.entries() {
        if let MadtEntry::InterruptOverride {
            bus,
            source: irq,
            gsi,
            flags,
        } = entry.map_err(|_| IrqError::Invalid)?
            && irq == source
            && (bus != 0 || override_data.replace((gsi, flags)).is_some())
        {
            return Err(IrqError::Invalid);
        }
    }
    let (gsi, flags) = override_data.unwrap_or((u32::from(source), 0));
    let (polarity, trigger) = decode_flags(flags)?;
    request_gsi(gsi, polarity, trigger, handler)
}

pub fn set_mask(route: Route, masked: bool) -> Result<(), IrqError> {
    let state = ALLOCATIONS.lock();
    if !state
        .routes
        .iter()
        .any(|item| item.gsi == route.gsi && item.vector == route.vector)
    {
        return Err(IrqError::Invalid);
    }
    ioapic::set_mask(route.gsi, masked)
}

/// Mask and release only after all CPUs are quiesced for this route.
pub fn release_route(route: Route) -> Result<(), IrqError> {
    let mut state = ALLOCATIONS.lock();
    let index = state
        .routes
        .iter()
        .position(|item| item.gsi == route.gsi && item.vector == route.vector)
        .ok_or(IrqError::Invalid)?;
    ioapic::set_mask(route.gsi, true)?;
    irq::unregister(u32::from(route.vector))?;
    state.used[route.vector as usize] = false;
    state.routes.swap_remove(index);
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct MsiMessage {
    pub address: u64,
    pub data: u32,
    pub vector: u8,
}

pub fn request_msi(handler: Handler) -> Result<MsiMessage, IrqError> {
    if !local::ready() {
        return Err(IrqError::NoController);
    }
    let destination = local::id();
    if destination > 255 {
        return Err(IrqError::Unsupported);
    }
    let vector = allocate(handler)?;
    Ok(MsiMessage {
        address: 0xfee0_0000 | (u64::from(destination) << 12),
        data: u32::from(vector),
        vector,
    })
}

pub fn release_msi(message: MsiMessage) -> Result<(), IrqError> {
    release(message.vector)
}
pub fn send_ipi(destination: u32) -> Result<(), IrqError> {
    local::send_fixed(destination, local::IPI_VECTOR)
}
pub fn send_fixed_ipi(destination: u32, vector: u8) -> Result<(), IrqError> {
    local::send_fixed(destination, vector)
}
pub fn send_nmi(destination: u32) -> Result<(), IrqError> {
    local::send_nmi(destination)
}
pub fn self_ipi() -> Result<(), IrqError> {
    local::self_ipi(local::IPI_VECTOR)
}
pub fn local_id() -> Option<u32> {
    local::ready().then(local::id)
}
pub fn controller_count() -> usize {
    ioapic::count()
}

pub fn dispatch(vector: u8) {
    if vector == local::SPURIOUS_VECTOR {
        return;
    }
    if vector == local::ERROR_VECTOR {
        let _ = local::error_status();
    } else {
        irq::dispatch(u32::from(vector));
    }
    local::eoi();
}

/// Local setup only; never reprogram shared I/O APIC routes on an AP.
pub fn init_cpu() -> Result<(), IrqError> {
    if !local::ready() {
        return Err(IrqError::NoController);
    }
    local::configure_cpu();
    let rsdp = boot::rsdp_address().ok_or(IrqError::NoController)?;
    let acpi = Acpi::from_rsdp(rsdp).map_err(|_| IrqError::Invalid)?;
    let madt = acpi
        .madt()
        .map_err(|_| IrqError::Invalid)?
        .ok_or(IrqError::NoController)?;
    configure_lint(&madt, local::id())
}
