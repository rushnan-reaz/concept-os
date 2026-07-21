// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel context-switch marker for delta profiling.
//!
//! Installs a minimal kernel `EventsTable` whose only non-stub hook is
//! `context_switch`, which drives **PC7** HIGH while the `update` task is the
//! current task and LOW otherwise. This gives a logic-analyzer view of "the
//! update component is scheduled" alongside the per-phase markers PC0..PC4 that
//! the update component itself drives.
//!
//! Runs pre-kernel (privileged, no MPU yet), so it configures GPIOC directly.

use kern::profiling::{configure_events_table, EventsTable};
use stm32l476rg::device;

/// Numeric component id of the `update` component (see
/// `components/update/core/Component.toml` and the live `info` dump).
const UPDATE_COMPONENT_ID: u16 = 5;

/// Configure PC7 as a very-high-speed push-pull output and install the table.
pub fn configure_profiling() {
    // Enable the GPIOC clock (AHB2ENR.GPIOCEN, bit 2).
    let rcc = unsafe { &*device::RCC::ptr() };
    rcc.ahb2enr.modify(|_, w| w.gpiocen().set_bit());

    let gpioc = unsafe { &*device::GPIOC::PTR };
    gpioc.ospeedr.modify(|_, w| w.ospeedr7().very_high_speed());
    gpioc.otyper.modify(|_, w| w.ot7().push_pull());
    gpioc.moder.modify(|_, w| w.moder7().output());
    // Start LOW.
    gpioc.bsrr.write(|w| unsafe { w.bits(1 << (7 + 16)) });

    configure_events_table(&EVENT_TABLE);
}

fn noop_u32(_: u32) {}
fn noop() {}

fn context_switch(component_id: u16) {
    let gpioc = unsafe { &*device::GPIOC::PTR };
    if component_id == UPDATE_COMPONENT_ID {
        gpioc.bsrr.write(|w| unsafe { w.bits(1 << 7) }); // PC7 high
    } else {
        gpioc.bsrr.write(|w| unsafe { w.bits(1 << (7 + 16)) }); // PC7 low
    }
}

static EVENT_TABLE: EventsTable = EventsTable {
    syscall_enter: noop_u32,
    syscall_exit: noop,
    secondary_syscall_enter: noop,
    secondary_syscall_exit: noop,
    isr_enter: noop,
    isr_exit: noop,
    timer_isr_enter: noop,
    timer_isr_exit: noop,
    context_switch,
};
