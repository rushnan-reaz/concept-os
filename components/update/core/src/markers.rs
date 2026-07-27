// GPIO phase markers for update-pipeline profiling (STM32L476, GPIOB).
// Edges are hardened against -Oz + LTO reordering with a compiler_fence + dsb pair.

use rcc_api::{Peripheral, RCC};

const GPIOB_BASE: u32 = 0x4800_0400;
const MODER_OFF: u32 = 0x00;
const BSRR_OFF: u32 = 0x18;

pub const RECV: u32 = 2;     // PB2
pub const INSTALL: u32 = 1;  // PB1
pub const RELOC: u32 = 15;   // PB15

#[inline(always)]
fn reg(off: u32) -> *mut u32 {
    (GPIOB_BASE + off) as *mut u32
}

/// Enable the GPIOB clock and set PB1/PB2/PB15 as push-pull outputs.
/// Call once, after `kipc::activate_task()`.
pub fn init() {
    // Idempotent: other components (uart3, bthermo) also enable GPIOB.
    RCC::new().enable_clock(Peripheral::GPIOB).ok();
    unsafe {
        let mut m = core::ptr::read_volatile(reg(MODER_OFF));
        for p in [RECV, INSTALL, RELOC] {
            m &= !(0b11 << (2 * p)); // clear mode bits
            m |= 0b01 << (2 * p);    // 0b01 = general-purpose output
        }
        core::ptr::write_volatile(reg(MODER_OFF), m);
    }
}

#[inline(always)]
pub fn set(pin: u32) {
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    unsafe { core::ptr::write_volatile(reg(BSRR_OFF), 1 << pin); }
    cortex_m::asm::dsb();
}

#[inline(always)]
pub fn clear(pin: u32) {
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    unsafe { core::ptr::write_volatile(reg(BSRR_OFF), 1 << (pin + 16)); }
    cortex_m::asm::dsb();
}
