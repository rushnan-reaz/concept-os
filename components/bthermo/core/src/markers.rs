// GPIO migration marker for bthermo (STM32L476, GPIOB, PB14).
// Same barrier-hardened toggling as the update component.

use rcc_api::{Peripheral, RCC};

const GPIOB_BASE: u32 = 0x4800_0400;
const MODER_OFF: u32 = 0x00;
const BSRR_OFF: u32 = 0x18;

pub const MIGRATE: u32 = 14; // PB14

#[inline(always)]
fn reg(off: u32) -> *mut u32 {
    (GPIOB_BASE + off) as *mut u32
}

/// Enable GPIOB clock and set PB14 as a push-pull output.
/// Must be called at the very top of `main`, before `hl::get_state`.
pub fn init() {
    RCC::new().enable_clock(Peripheral::GPIOB).ok();
    unsafe {
        let mut m = core::ptr::read_volatile(reg(MODER_OFF));
        m &= !(0b11 << (2 * MIGRATE));
        m |= 0b01 << (2 * MIGRATE);
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
