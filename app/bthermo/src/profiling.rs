// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Kernel instrumentation for Task B1/B2 (MEASUREMENT_PROGRAM.md).
//
// Wires task_update_begin/end and flash_erase_begin/end to PB0/PB7.
// context_switch is a no-op here: delta-performance's update component
// already drives PC2-PC7 for its own delta-phase markers (see
// components/update/core/src/markers.rs), so this file must not touch
// GPIOC at all to avoid a pin conflict with that instrumentation.
use kern::profiling::EventsTable;
use stm32l476rg::device;

static EVENT_TABLE: EventsTable = EventsTable {
    syscall_enter: |_| (),
    syscall_exit: || (),
    secondary_syscall_enter: || (),
    secondary_syscall_exit: || (),
    isr_enter: || (),
    isr_exit: || (),
    timer_isr_enter: || (),
    timer_isr_exit: || (),
    context_switch: |_| (),
    task_update_begin,
    task_update_end,
    flash_erase_begin,
    flash_erase_end,
};

pub fn configure_profiling() {
    let rcc = unsafe { &*device::RCC::PTR };
    rcc.ahb2enr.modify(|r, w| unsafe { w.bits(r.bits() | (1 << 1)) }); // GPIOB

    // PB0: component-level unavailability window (Task B1). High from
    // Task::begin_update() (old instance torn down) to Task::end_update()
    // (new instance's component_id/generation restored, becomes
    // schedulable again). Deterministic, kernel-driven -- not dependent on
    // the scheduler's context_switch hook, which has blind spots (e.g. idle
    // re-entry) that make it unsuitable for this measurement.
    let gpiob = unsafe { &*device::GPIOB::PTR };
    gpiob.moder.modify(|_, w| w.moder0().output());
    gpiob.otyper.modify(|_, w| w.ot0().push_pull());
    gpiob.ospeedr.modify(|_, w| w.ospeedr0().high_speed());

    // PB7: system-level unavailability window (Task B2). High for the
    // duration of the actual hardware flash page erase call
    // (FlashInterface::erase_timed -> native_methods.erase), during which
    // the flash controller blocks bus access for the whole MCU -- nothing
    // at all can be scheduled, not just the calling (storage) task.
    gpiob.moder.modify(|_, w| w.moder7().output());
    gpiob.otyper.modify(|_, w| w.ot7().push_pull());
    gpiob.ospeedr.modify(|_, w| w.ospeedr7().high_speed());

    kern::profiling::configure_events_table(&EVENT_TABLE);
}

fn task_update_begin(_old_component_id: u16) {
    let gpiob = unsafe { &*device::GPIOB::PTR };
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    gpiob.bsrr.write(|w| unsafe { w.bits(1 << 0) }); // PB0 high
    cortex_m::asm::dsb();
}

fn task_update_end(_new_component_id: u16) {
    let gpiob = unsafe { &*device::GPIOB::PTR };
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    gpiob.bsrr.write(|w| unsafe { w.bits(1 << (0 + 16)) }); // PB0 low
    cortex_m::asm::dsb();
}

fn flash_erase_begin() {
    let gpiob = unsafe { &*device::GPIOB::PTR };
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    gpiob.bsrr.write(|w| unsafe { w.bits(1 << 7) }); // PB7 high
    cortex_m::asm::dsb();
}

fn flash_erase_end() {
    let gpiob = unsafe { &*device::GPIOB::PTR };
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    gpiob.bsrr.write(|w| unsafe { w.bits(1 << (7 + 16)) }); // PB7 low
    cortex_m::asm::dsb();
}
