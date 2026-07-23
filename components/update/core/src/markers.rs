// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! GPIO phase markers for delta-update profiling.
//!
//! Drives Port C pins PC0..PC4 HIGH for the duration of each delta phase so an
//! external logic analyzer can time them. Writes use `BSRR` (a single atomic
//! store, ~12.5 ns at 80 MHz) so a marker never distorts the measured path.
//!
//! Gated behind the `profiling` feature — production builds compile the no-op
//! stubs and pull in none of the GPIO/rcc dependencies.

#[cfg(feature = "profiling")]
mod imp {
    use core::sync::atomic::{compiler_fence, Ordering};
    use rcc_api::{Peripheral, RCC};
    use stm32l476rg::device;

    /// One-time GPIOC setup: enable clock (via the rcc component) and configure
    /// PC0..PC4 as very-high-speed push-pull outputs, starting LOW. Idempotent.
    pub fn markers_init() {
        let mut rcc = RCC::new();
        let _ = rcc.enable_clock(Peripheral::GPIOC);
        let _ = rcc.leave_reset(Peripheral::GPIOC);

        let gpioc = unsafe { &*device::GPIOC::PTR };
        gpioc.ospeedr.modify(|_, w| {
            w.ospeedr0()
                .very_high_speed()
                .ospeedr1()
                .very_high_speed()
                .ospeedr2()
                .very_high_speed()
                .ospeedr3()
                .very_high_speed()
                .ospeedr4()
                .very_high_speed()
        });
        gpioc.otyper.modify(|_, w| {
            w.ot0()
                .push_pull()
                .ot1()
                .push_pull()
                .ot2()
                .push_pull()
                .ot3()
                .push_pull()
                .ot4()
                .push_pull()
        });
        gpioc.moder.modify(|_, w| {
            w.moder0()
                .output()
                .moder1()
                .output()
                .moder2()
                .output()
                .moder3()
                .output()
                .moder4()
                .output()
        });
        // Drive PC0..PC4 LOW to start (reset bits = high half of BSRR).
        gpioc.bsrr.write(|w| unsafe { w.bits(0x1F << 16) });
    }

    /// Drive marker `pin` (0..=4) HIGH — single atomic BSRR set, then a barrier
    /// so the phase's work cannot be reordered *before* the pin goes high.
    ///
    /// The BSRR store has no data dependency on the bracketed work, so without
    /// these barriers `-Oz` + LTO is free to hoist/sink the set and clear until
    /// they run back-to-back, collapsing the measured pulse.
    #[inline(always)]
    pub fn mark_set(pin: u8) {
        let gpioc = unsafe { &*device::GPIOC::PTR };
        gpioc.bsrr.write(|w| unsafe { w.bits(1u32 << pin) });
        compiler_fence(Ordering::SeqCst);
        cortex_m::asm::dsb();
    }

    /// Drive marker `pin` (0..=4) LOW — barrier first so the phase's work cannot
    /// be reordered *after* the pin goes low, then the single atomic BSRR reset.
    #[inline(always)]
    pub fn mark_clear(pin: u8) {
        compiler_fence(Ordering::SeqCst);
        cortex_m::asm::dsb();
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
