// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

//! Generic I2Cv2 bus-recovery check for the live-update handover path.
//!
//! On a live (reboot-free) component update, a task inheriting its
//! predecessor's state via state transfer skips full hardware
//! initialization -- it assumes the peripheral it inherited is already in a
//! valid configuration. If an I2C transaction was mid-flight at the exact
//! moment the kernel swapped tasks, the bus can be left stuck (a slave
//! holding SDA low waiting for more clock pulses it will never get, or the
//! I2C peripheral's own BUSY flag latched) with no recovery path, since a
//! normal boot's bus-clearing logic never runs on this path.
//!
//! This crate is deliberately generic over *which* I2C peripheral and *which*
//! pins own it: GPIO and I2Cv2 register layouts are identical across every
//! port/instance on this MCU family (only the base address differs), but
//! the PAC generates a distinct Rust type per instance with no shared trait
//! to genericize over via the type system. Operating on raw base addresses
//! instead sidesteps that: any component that owns an I2C peripheral can
//! call this with its own base addresses and pin numbers, independently --
//! this crate does not, and architecturally cannot, reach across component
//! boundaries to recover a bus it doesn't itself own (each component only
//! has a peripheral grant for its own hardware).
//!
//! Register offsets below are the standard STM32L4 GPIO and I2Cv2 layouts
//! (identical across GPIOA..GPIOH and I2C1..I2C3 on this family).

const GPIO_MODER: u32 = 0x00;
const GPIO_OTYPER: u32 = 0x04;
const GPIO_IDR: u32 = 0x10;
const GPIO_BSRR: u32 = 0x18;
const GPIO_PUPDR: u32 = 0x0C;

const I2C_ISR: u32 = 0x18;
const I2C_ICR: u32 = 0x1C;
const I2C_CR2: u32 = 0x04;

const I2C_ISR_BUSY_BIT: u32 = 1 << 15;
const I2C_ISR_STOPF_BIT: u32 = 1 << 5;
const I2C_ICR_STOPCF_BIT: u32 = 1 << 5;
const I2C_ICR_NACKCF_BIT: u32 = 1 << 4;
const I2C_CR2_STOP_BIT: u32 = 1 << 14;

const RUNS_PER_US: u32 = 80;

#[inline(always)]
unsafe fn read(addr: u32) -> u32 {
    core::ptr::read_volatile(addr as *const u32)
}

#[inline(always)]
unsafe fn write(addr: u32, val: u32) {
    core::ptr::write_volatile(addr as *mut u32, val)
}

fn bus_delay() {
    for _ in 0..(RUNS_PER_US * 5) {
        cortex_m::asm::nop();
    }
}

/// Check whether the I2C bus at `gpio_base` (pins `scl_pin`/`sda_pin`) /
/// `i2c_base` is stuck -- either SDA held low or the peripheral's BUSY flag
/// latched -- and recover it in place if so. Does nothing (no register
/// writes at all) if the bus is already healthy.
///
/// `gpio_base`: base address of the GPIO port the SCL/SDA pins live on
/// (e.g. GPIOC = `0x4800_0800` on STM32L476).
/// `i2c_base`: base address of the I2C peripheral instance
/// (e.g. I2C3 = `0x4000_5C00` on STM32L476).
/// `scl_pin`/`sda_pin`: pin numbers (0..=15) on `gpio_base`.
///
/// # Safety
/// Caller must ensure `gpio_base` is a valid, clocked GPIO port register
/// block, `i2c_base` is a valid, clocked I2C peripheral register block, and
/// that the calling component has a peripheral grant covering both -- this
/// function performs no ownership or grant checking of its own.
pub unsafe fn recover_i2c_bus_if_stuck(
    gpio_base: u32,
    i2c_base: u32,
    scl_pin: u8,
    sda_pin: u8,
) {
    let sda_low = (read(gpio_base + GPIO_IDR) & (1 << sda_pin)) == 0;
    let busy = (read(i2c_base + I2C_ISR) & I2C_ISR_BUSY_BIT) != 0;
    if !sda_low && !busy {
        return;
    }

    // Drop SCL/SDA to plain GPIO (open-drain output, pulled up) -- the I2C
    // peripheral can't drive a valid STOP onto a bus it doesn't understand
    // the state of.
    let pupdr_mask = (0b11 << (scl_pin * 2)) | (0b11 << (sda_pin * 2));
    let pupdr_pullup = (0b01 << (scl_pin * 2)) | (0b01 << (sda_pin * 2));
    write(
        gpio_base + GPIO_PUPDR,
        (read(gpio_base + GPIO_PUPDR) & !pupdr_mask) | pupdr_pullup,
    );
    // OTYPER: open-drain (bit set) on both pins.
    write(
        gpio_base + GPIO_OTYPER,
        read(gpio_base + GPIO_OTYPER) | (1 << scl_pin) | (1 << sda_pin),
    );
    // MODER: general-purpose output (01) on both pins.
    let moder_mask = (0b11 << (scl_pin * 2)) | (0b11 << (sda_pin * 2));
    let moder_output = (0b01 << (scl_pin * 2)) | (0b01 << (sda_pin * 2));
    write(
        gpio_base + GPIO_MODER,
        (read(gpio_base + GPIO_MODER) & !moder_mask) | moder_output,
    );

    // Release both lines (open-drain -> high via pull-ups).
    write(gpio_base + GPIO_BSRR, (1 << scl_pin) | (1 << sda_pin));
    bus_delay();

    // If SDA is still held low, clock SCL up to 9 times -- the maximum
    // length of a single byte transfer (8 data bits + 1 ACK) -- to let a
    // stuck slave finish what it thinks is the current transaction and
    // release the bus on its own.
    let mut tries = 9;
    while (read(gpio_base + GPIO_IDR) & (1 << sda_pin)) == 0 && tries > 0 {
        write(gpio_base + GPIO_BSRR, 1 << (scl_pin + 16)); // SCL low
        bus_delay();
        write(gpio_base + GPIO_BSRR, 1 << scl_pin); // SCL release high
        bus_delay();
        tries -= 1;
    }

    // Manual STOP: SDA low while SCL high, then SDA high.
    write(gpio_base + GPIO_BSRR, 1 << (sda_pin + 16));
    bus_delay();
    write(gpio_base + GPIO_BSRR, 1 << scl_pin);
    bus_delay();
    write(gpio_base + GPIO_BSRR, 1 << sda_pin);
    bus_delay();

    // Hand the pins back to the I2C peripheral (alternate-function mode,
    // value 0b10). Caller is responsible for AFRL/AFRH (the specific AF
    // number differs by peripheral/pin combination), so only MODER is
    // restored here.
    write(
        gpio_base + GPIO_MODER,
        (read(gpio_base + GPIO_MODER) & !moder_mask)
            | (0b10 << (scl_pin * 2))
            | (0b10 << (sda_pin * 2)),
    );

    // If the peripheral's own BUSY flag is still latched, the wire-level fix
    // alone wasn't enough -- force one more STOP through the peripheral
    // itself and clear its error flags.
    if (read(i2c_base + I2C_ISR) & I2C_ISR_BUSY_BIT) != 0 {
        write(i2c_base + I2C_CR2, read(i2c_base + I2C_CR2) | I2C_CR2_STOP_BIT);
        for _ in 0..(RUNS_PER_US * 1000) {
            cortex_m::asm::nop();
            if (read(i2c_base + I2C_ISR) & I2C_ISR_STOPF_BIT) != 0 {
                break;
            }
        }
        write(
            i2c_base + I2C_ICR,
            I2C_ICR_STOPCF_BIT | I2C_ICR_NACKCF_BIT,
        );
    }
}
