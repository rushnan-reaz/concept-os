// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! GPIO phase markers for delta-update profiling.
//!
//! Drives Port C pins HIGH for the duration of each delta phase so an
//! external logic analyzer can time them. Writes use `BSRR` (a single atomic
//! store, ~12.5 ns at 80 MHz) so a marker never distorts the measured path.
//!
//! Logical phase indices 0..5 map to physical pins via `PHASE_PIN` below --
//! NOT a straight 1:1 PC0..PC5 mapping. PC0/PC1 are bthermo's I2C3 SCL/SDA
//! (see components/bthermo/core/src/i2c.rs): if both components are active
//! concurrently (a live update while the sensor loop runs), the two GPIO
//! configs directly contend for the same physical pins and whichever inits
//! last silently wins, making phases 0/1 unreliable to capture. Phases 0
//! (header pull) and 1 (find base) are remapped to PC5/PC6 (free, no other
//! component in this app uses them) to avoid that collision entirely; phases
//! 2 (masked base-CRC) and 3 (reconstruct+install) keep PC2/PC3. Phases 4 and
//! 5 originally bracketed the pre-single-write-redesign "install" and
//! "scratch flash allocation" steps (now folded into phase 3) and went
//! unused for a time; they have since been revived as the baseline-comparable
//! INSTALL-equivalent (phase 4, PC4: brackets only the `load_component()`
//! call, matching bthermo-performance's own INSTALL marker exactly) and
//! RELOC_total-equivalent (phase 5, PC7: brackets Stage 3's relocate+flush
//! span in `reconstruct_into_final`, matching bthermo-performance's RELOC
//! marker) -- see `delta.rs` for both.
//!
//! Gated behind the `profiling` feature — production builds compile the no-op
//! stubs and pull in none of the GPIO/rcc dependencies.

#[cfg(feature = "profiling")]
mod imp {
    use core::sync::atomic::{compiler_fence, Ordering};
    use rcc_api::{Peripheral, RCC};
    use stm32l476rg::device;

    /// Logical phase index (0..=5) -> physical GPIOC pin number.
    const PHASE_PIN: [u8; 6] = [5, 6, 2, 3, 4, 7];

    /// One-time GPIOC setup: enable clock (via the rcc component) and configure
    /// the marker pins as very-high-speed push-pull outputs, starting LOW.
    /// Idempotent.
    pub fn markers_init() {
        let mut rcc = RCC::new();
        let _ = rcc.enable_clock(Peripheral::GPIOC);
        let _ = rcc.leave_reset(Peripheral::GPIOC);

        let gpioc = unsafe { &*device::GPIOC::PTR };
        gpioc.ospeedr.modify(|_, w| {
            w.ospeedr2()
                .very_high_speed()
                .ospeedr3()
                .very_high_speed()
                .ospeedr4()
                .very_high_speed()
                .ospeedr5()
                .very_high_speed()
                .ospeedr6()
                .very_high_speed()
                .ospeedr7()
                .very_high_speed()
        });
        gpioc.otyper.modify(|_, w| {
            w.ot2()
                .push_pull()
                .ot3()
                .push_pull()
                .ot4()
                .push_pull()
                .ot5()
                .push_pull()
                .ot6()
                .push_pull()
                .ot7()
                .push_pull()
        });
        gpioc.moder.modify(|_, w| {
            w.moder2()
                .output()
                .moder3()
                .output()
                .moder4()
                .output()
                .moder5()
                .output()
                .moder6()
                .output()
                .moder7()
                .output()
        });
        // Drive all marker pins LOW to start (reset bits = high half of BSRR).
        let mask: u32 = PHASE_PIN.iter().fold(0, |m, &p| m | (1 << p));
        gpioc.bsrr.write(|w| unsafe { w.bits(mask << 16) });
    }

    /// Drive marker for phase `n` (0..=5) HIGH -- single atomic BSRR set, then
    /// a barrier so the phase's work cannot be reordered *before* the pin
    /// goes high.
    ///
    /// The BSRR store has no data dependency on the bracketed work, so without
    /// these barriers `-Oz` + LTO is free to hoist/sink the set and clear until
    /// they run back-to-back, collapsing the measured pulse.
    #[inline(always)]
    pub fn mark_set(n: u8) {
        let pin = PHASE_PIN[n as usize];
        let gpioc = unsafe { &*device::GPIOC::PTR };
        gpioc.bsrr.write(|w| unsafe { w.bits(1u32 << pin) });
        compiler_fence(Ordering::SeqCst);
        cortex_m::asm::dsb();
    }

    /// Drive marker for phase `n` (0..=5) LOW -- barrier first so the phase's
    /// work cannot be reordered *after* the pin goes low, then the single
    /// atomic BSRR reset.
    #[inline(always)]
    pub fn mark_clear(n: u8) {
        compiler_fence(Ordering::SeqCst);
        cortex_m::asm::dsb();
        let pin = PHASE_PIN[n as usize];
        let gpioc = unsafe { &*device::GPIOC::PTR };
        gpioc.bsrr.write(|w| unsafe { w.bits(1u32 << (pin as u32 + 16)) });
        compiler_fence(Ordering::SeqCst);
    }
}

#[cfg(not(feature = "profiling"))]
mod imp {
    #[inline(always)]
    pub fn markers_init() {}
    #[inline(always)]
    pub fn mark_set(_pin: u8) {}
    #[inline(always)]
    pub fn mark_clear(_pin: u8) {}
}

pub use imp::{mark_clear, mark_set, markers_init};

/// RAII bracket: HIGH on construction, LOW on drop — so a phase pin is cleared
/// even on a `?` early-return (base-not-found, CRC-mismatch, etc.). Compiles to
/// nothing when `profiling` is off (the set/clear calls are no-op stubs).
pub struct Marker(#[allow(dead_code)] u8);

impl Marker {
    #[inline(always)]
    pub fn new(pin: u8) -> Self {
        mark_set(pin);
        Marker(pin)
    }
}

impl Drop for Marker {
    #[inline(always)]
    fn drop(&mut self) {
        mark_clear(self.0);
    }
}
