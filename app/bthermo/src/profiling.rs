// Minimal kernel context-switch instrumentation for Task B1
// (MEASUREMENT_PROGRAM.md). Only wires `context_switch`, broadcasting the
// current component id on PC4-7 as a 4-bit binary value on every task
// switch. All other EventsTable hooks are no-ops.
//
// PC0/PC1 are intentionally NOT touched here — those are bthermo's I2C3
// SCL/SDA (see components/bthermo/core/src/i2c.rs). PC4-7 are otherwise
// unused in this app.
use kern::profiling::EventsTable;
use stm32l476rg::device as device;

static EVENT_TABLE: EventsTable = EventsTable {
    syscall_enter: |_| (),
    syscall_exit: || (),
    secondary_syscall_enter: || (),
    secondary_syscall_exit: || (),
    isr_enter: || (),
    isr_exit: || (),
    timer_isr_enter: || (),
    timer_isr_exit: || (),
    context_switch,
    task_update_begin,
    task_update_end,
    flash_erase_begin,
    flash_erase_end,
};

pub fn configure_profiling() {
    let rcc = unsafe { &*device::RCC::PTR };
    rcc.ahb2enr.modify(|r, w| unsafe { w.bits(r.bits() | (1 << 2) | (1 << 1)) }); // GPIOC, GPIOB

    let gpioc = unsafe { &*device::GPIOC::PTR };
    gpioc.ospeedr.modify(|_, w| {
        w.ospeedr4().high_speed()
            .ospeedr5().high_speed()
            .ospeedr6().high_speed()
            .ospeedr7().high_speed()
    });
    gpioc.otyper.modify(|_, w| {
        w.ot4().push_pull().ot5().push_pull().ot6().push_pull().ot7().push_pull()
    });
    gpioc.moder.modify(|_, w| {
        w.moder4().output().moder5().output().moder6().output().moder7().output()
    });

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
    // (Originally PB3, but that's the Nucleo board's default SWO/trace pin
    // and read constantly high even under plain-GPIO config -- moved to
    // PB7, a pin with no debug-interface association.)
    gpiob.moder.modify(|_, w| w.moder7().output());
    gpiob.otyper.modify(|_, w| w.ot7().push_pull());
    gpiob.ospeedr.modify(|_, w| w.ospeedr7().high_speed());

    kern::profiling::configure_events_table(&EVENT_TABLE);
}

fn context_switch(component_id: u16) {
    let gpioc = unsafe { &*device::GPIOC::PTR };
    // Write the lowest 4 bits of the component_id into PC4-7, preserving
    // PC0-3 (and PC0/1 = I2C3 in particular) untouched.
    let id_4bits = ((component_id & 0b1111) << 4) as u32;
    gpioc.odr.modify(|r, w| unsafe {
        w.bits((r.bits() & !(0b1111_u32 << 4)) | id_4bits)
    });
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
