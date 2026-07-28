use bthermo_api::ThermoError;
use rcc_api::RCC;
use stm32l476rg::device as device;
use userlib::*;

const RUNS_PER_US: u32 = 80;
const TIMED_LOOP_US: u32 = 1000;

macro_rules! timed_loop {
    ($condition:expr) => {{
        let mut success: bool = false;
        for _ in 0..(RUNS_PER_US * TIMED_LOOP_US) {
            // Prevent the cycle for being optimized out
            cortex_m::asm::nop();
            if $condition {
                success = true;
                break;
            }
        }
        if !success {
            return Err(());
        }
    }};
}

// ===== TEMPORARY DIAGNOSTIC (fix_i2c_sensor_bus_task.md Phase 1) — remove in Phase 3 =====
// Same as `timed_loop!`, but logs the ISR state under `$tag` before returning Err.
macro_rules! timed_loop_log {
    ($self:expr, $tag:expr, $condition:expr) => {{
        let mut success: bool = false;
        for _ in 0..(RUNS_PER_US * TIMED_LOOP_US) {
            cortex_m::asm::nop();
            if $condition {
                success = true;
                break;
            }
        }
        if !success {
            $self.log_i2c_state($tag);
            return Err(());
        }
    }};
}
// ===== END DIAGNOSTIC =====

/**
 * Pinout:
 *      A5 -> PC0 -> I2C3_SCL
 *      A4 -> PC1 -> I2C3_SDA
 */
#[allow(non_camel_case_types)]
pub struct I2C_Channel<'a> {
    gpioc: &'a device::gpioc::RegisterBlock,
    i2c1: &'a device::i2c1::RegisterBlock, // field name kept; points at I2C3
}

impl<'a> I2C_Channel<'a> {
    pub fn new() -> Self {
        Self {
            gpioc: unsafe { &*device::GPIOC::PTR },
            i2c1: unsafe { &*device::I2C3::PTR },
        }
    }
    pub fn init_hardware(&mut self, rcc: &mut RCC) -> Result<(), ThermoError> {
        // Enable GPIOC before touching its registers, then clear any hung bus
        // (e.g. DS3231 holding SDA low after a reset mid-transaction) via GPIO
        // bit-banging BEFORE the pins are switched to AF/I2C.
        rcc.enable_clock(rcc_api::Peripheral::GPIOC).unwrap_lite();
        rcc.leave_reset(rcc_api::Peripheral::GPIOC).unwrap_lite();
        self.clear_bus();
        self.init_gpio(rcc);
        self.init_i2c(rcc);
        Ok(())
    }

    /// Manually clear a hung I2C bus (e.g. DS3231 holding SDA low after a
    /// reset mid-byte). Must run before the pins are switched to AF4 — the
    /// I2C peripheral cannot drive a valid STOP onto an already-stuck bus.
    /// PC0 = SCL, PC1 = SDA.
    fn clear_bus(&mut self) {
        self.gpioc
            .pupdr
            .modify(|_, w| w.pupdr0().pull_up().pupdr1().pull_up());
        self.gpioc
            .otyper
            .modify(|_, w| w.ot0().open_drain().ot1().open_drain());
        self.gpioc
            .moder
            .modify(|_, w| w.moder0().output().moder1().output());
        // Release both lines (open-drain -> high via pull-ups)
        self.gpioc.bsrr.write(|w| w.bs0().set_bit().bs1().set_bit());
        Self::bus_delay();

        // If SDA is held low, clock SCL up to 9 times to let the slave finish.
        let mut tries = 9;
        while self.gpioc.idr.read().idr1().bit_is_clear() && tries > 0 {
            self.gpioc.bsrr.write(|w| w.br0().set_bit()); // SCL low
            Self::bus_delay();
            self.gpioc.bsrr.write(|w| w.bs0().set_bit()); // SCL release high
            Self::bus_delay();
            tries -= 1;
        }

        // Manual STOP: SDA low while SCL high, then SDA high.
        self.gpioc.bsrr.write(|w| w.br1().set_bit());
        Self::bus_delay();
        self.gpioc.bsrr.write(|w| w.bs0().set_bit());
        Self::bus_delay();
        self.gpioc.bsrr.write(|w| w.bs1().set_bit());
        Self::bus_delay();
    }

    /// ~5 µs busy delay at 80 MHz for bit-banged bus clearing.
    fn bus_delay() {
        for _ in 0..(RUNS_PER_US * 5) {
            cortex_m::asm::nop();
        }
    }
    fn init_gpio(&mut self, rcc: &mut RCC) {
        // Turn on clock and leave reset for GPIOC (PC0=SCL, PC1=SDA)
        rcc.enable_clock(rcc_api::Peripheral::GPIOC).unwrap_lite();
        rcc.leave_reset(rcc_api::Peripheral::GPIOC).unwrap_lite();
        // Select alternate function for PC0, PC1
        self.gpioc
            .moder
            .modify(|_, w| w.moder0().alternate().moder1().alternate());
        // Setup alternate function AF4 (I2C3) — pins 0/1 use AFRL, not AFRH
        self.gpioc.afrl.modify(|_, w| w.afrl0().af4().afrl1().af4());
        // Setup pins in open drain (critical for I2C)
        self.gpioc.otyper.modify(|_, w| w.ot0().open_drain().ot1().open_drain());
        // Enable internal pull-ups. Safe to enable even if the board also has
        // external pull-ups.
        self.gpioc.pupdr.modify(|_, w| w.pupdr0().pull_up().pupdr1().pull_up());
    }
    fn init_i2c(&mut self, rcc: &mut RCC) {
        // Turn on I2C3 and leave reset
        rcc.enable_clock(rcc_api::Peripheral::I2C3).unwrap();
        rcc.leave_reset(rcc_api::Peripheral::I2C3).unwrap();

        // Turn off peripheral
        self.i2c1.cr1.modify(|_, w| w.pe().disabled());

        // Set timing (400Khz fast mode). The previous constant (0x10F0143C)
        // was calibrated for ~16MHz I2CCLK, but I2C3SEL defaults to PCLK1
        // (80MHz) and nothing in this codebase ever selects a different I2C3
        // kernel clock — a ~5x mismatch, making every SCL timing interval
        // ~5x shorter than intended. This value is derived for I2CCLK=80MHz
        // targeting genuine 400kHz Fast Mode (PRESC=3, SCLL=29, SCLH=19,
        // SCLDEL=4, SDADEL=0) — verify against a logic analyzer capture of
        // actual SCL frequency, not blindly trusted.
        self.i2c1.timingr.write(|w| unsafe { w.bits(0x3040131D) });
        
        // Set own address at 0
        self.i2c1
            .oar1
            .modify(|_, w| w.oa1en().enabled().oa1().bits(0));
        // Enable the AUTOEND by default, and enable NACK
        self.i2c1
            .cr2
            .modify(|_, w| w.autoend().set_bit().nack().set_bit());
        // Generalcall and NoStretch mode
        self.i2c1
            .cr1
            .modify(|_, w| w.gcen().enabled().nostretch().enabled());
        // Enable I2C
        self.i2c1.cr1.modify(|_, w| w.pe().enabled());

        // STM32 I2Cv2 errata: if SDA reads low at the instant PE is set (e.g.
        // right after the GPIO->AF4 mode transition on these open-drain
        // pins), the peripheral can latch BUSY permanently, silently
        // refusing to generate a START on the first real transaction until
        // something forces a STOP. Clear it here, at init time, instead of
        // letting the first caller (thermo/rtc init) absorb a ~23ms timeout
        // and a spurious failure.
        if self.i2c1.isr.read().busy().bit_is_set() {
            self.recover_after_failure();
        }
    }

    pub fn i2c_mem_read(
        &mut self,
        device_address: u8,
        mem_address: u8,
        data: &mut [u8],
    ) -> Result<(), ()> {
        // Max data length
        if data.len() > u8::MAX as usize {
            panic!("Too much data in a single packet");
        }
        // Request memory (a complete, auto-ended write transaction — see
        // _i2c_select_register). The bus is idle by the time this returns,
        // so the read below is a fresh START, not a repeated-START.
        self._i2c_select_register(device_address, mem_address)?;

        // Configure reception
        self.i2c1.cr2.modify(|_, w| {
            w.sadd()
                .bits((device_address << 1 | 1) as u16)
                .nbytes()
                .bits(data.len() as u8)
                .autoend()
                .automatic()
                .rd_wrn()
                .read()
                .start()
                .start()
        });

        // Wait for data
        let mut curr_pos: usize = 0;
        while curr_pos < data.len() {
            // TEMPORARY DIAGNOSTIC: timed_loop_log! instead of timed_loop! — remove in Phase 3
            timed_loop_log!(self, "mem_read_rxne", self.i2c1.isr.read().rxne().bit_is_set());
            let byte = (self.i2c1.rxdr.read().bits() & 0xFF) as u8;
            data[curr_pos] = byte;
            curr_pos += 1;
        }
        Ok(())
    }

    pub fn i2c_mem_write(
        &mut self,
        device_address: u8,
        mem_address: u8,
        data: &[u8],
    ) -> Result<(), ()> {
        // Max data length
        if data.len() > u8::MAX as usize {
            panic!("Too much data in a single packet");
        }
        // Configure reception
        self.i2c1.cr2.modify(|_, w| {
            w.sadd()
                .bits((device_address << 1 | 1) as u16)
                .nbytes()
                .bits((data.len() + 1) as u8)
                .autoend()
                .automatic()
                .rd_wrn()
                .write()
                .start()
                .start()
        });

        // Start by sending the register address
        // Wait to be ready
        // TEMPORARY DIAGNOSTIC: timed_loop_log! instead of timed_loop! — remove in Phase 3
        timed_loop_log!(self, "mem_write_txis_addr", self.i2c1.isr.read().txis().is_empty());
        // Put address on the tx reg
        self.i2c1.txdr.write(|w| w.txdata().bits(mem_address));

        let mut curr_pos: usize = 0;
        while curr_pos < data.len() {
            // Wait to be ready
            timed_loop_log!(self, "mem_write_txis_data", self.i2c1.isr.read().txis().is_empty());
            // Put address on the tx reg
            self.i2c1.txdr.write(|w| w.txdata().bits(data[curr_pos]));
            curr_pos += 1;
        }
        Ok(())
    }

    fn _i2c_select_register(
        &mut self,
        device_address: u8,
        mem_address: u8,
    ) -> Result<(), ()> {
        // Rewritten as a complete, self-contained, AUTOEND=automatic write
        // transaction (address + the single register-pointer byte, then a
        // hardware-generated STOP) — the same proven pattern already used by
        // i2c_mem_write, instead of software-AUTOEND + a later
        // software-triggered repeated-START. The repeated-START approach
        // reliably produced an immediate hardware auto-STOP right after the
        // address phase (before the register-pointer byte ever transmitted)
        // across every variation tried; TMP102 does not require a strict
        // repeated-START between register-select and the read, so the
        // caller (i2c_mem_read) issues its own fresh START afterward instead.
        self.i2c1.cr2.modify(|_, w| {
            w.sadd()
                .bits((device_address << 1 | 0) as u16)
                .nbytes()
                .bits(1) // The memory address
                .autoend()
                .automatic()
                .rd_wrn()
                .write()
                .start()
                .start()
        });
        self.wait_or_recover(|isr| isr.txis().bit_is_set())?;
        // Put address on the tx reg
        self.i2c1.txdr.write(|w| w.txdata().bits(mem_address));
        // With AUTOEND=automatic, hardware generates the STOP itself once
        // this single byte is transferred. Wait for it so the bus is known
        // idle before the caller issues its own fresh START.
        self.wait_or_recover(|isr| isr.stopf().bit_is_set())?;
        self.i2c1.icr.write(|w| w.stopcf().clear());
        Ok(())
    }

    /// Busy-wait (same ~4ms budget as `timed_loop!`) for `condition` on the
    /// ISR register, bailing out early if NACKF is observed. On NACK or
    /// timeout, force a STOP and clear NACKF/STOPF so the bus is never left
    /// held — used only for the software-AUTOEND register-select phase.
    fn wait_or_recover(
        &mut self,
        condition: impl Fn(&device::i2c1::isr::R) -> bool,
    ) -> Result<(), ()> {
        for _ in 0..(RUNS_PER_US * TIMED_LOOP_US) {
            cortex_m::asm::nop();
            let isr = self.i2c1.isr.read();
            if isr.nackf().bit_is_set() {
                // TEMPORARY DIAGNOSTIC line — remove in Phase 3
                self.log_i2c_state("select_register_nack");
                // ES0250 §2.20.9 workaround (see pe_toggle_recover) instead
                // of forcing a manual STOP.
                self.pe_toggle_recover();
                return Err(());
            }
            if condition(&isr) {
                return Ok(());
            }
        }
        // Timed out without ever seeing the condition or a NACK.
        // TEMPORARY DIAGNOSTIC line — remove in Phase 3
        self.log_i2c_state("select_register_timeout");
        self.recover_after_failure();
        Err(())
    }

    // ===== TEMPORARY DIAGNOSTIC (fix_i2c_sensor_bus_task.md Phase 1) — remove in Phase 3 =====
    fn log_i2c_state(&self, tag: &str) {
        let isr = self.i2c1.isr.read();
        sys_log!(
            "[I2C {}] nackf={} berr={} arlo={} txis={} rxne={} tc={} stopf={} busy={}",
            tag,
            isr.nackf().bit_is_set(),
            isr.berr().bit_is_set(),
            isr.arlo().bit_is_set(),
            isr.txis().bit_is_set(),
            isr.rxne().bit_is_set(),
            isr.tc().bit_is_set(),
            isr.stopf().bit_is_set(),
            isr.busy().bit_is_set(),
        );
    }
    // ===== END DIAGNOSTIC =====

    /// Force a STOP condition and clear NACKF/STOPF so a failed/timed-out
    /// transaction never leaves the bus held for the next transaction.
    fn recover_after_failure(&mut self) {
        self.i2c1.cr2.modify(|_, w| w.stop().stop());
        for _ in 0..(RUNS_PER_US * TIMED_LOOP_US) {
            cortex_m::asm::nop();
            if self.i2c1.isr.read().stopf().bit_is_set() {
                break;
            }
        }
        self.i2c1
            .icr
            .write(|w| w.stopcf().clear().nackcf().clear());
    }

    /// ES0250 (STM32L471/475/476/486 errata) §2.20.9 "Transmission stalled
    /// after first byte transfer": if NACKF is observed together with RXNE
    /// right after the first byte of a transfer, the interface can stall
    /// (or — matching what we observe here — latch a NACKF that forces our
    /// own recovery path to fire even though the bus itself shows a real
    /// ACK). ST's documented workaround is not a bus-level STOP; it's a
    /// full peripheral disable/re-enable via CR1.PE.
    fn pe_toggle_recover(&mut self) {
        self.i2c1.cr1.modify(|_, w| w.pe().disabled());
        self.i2c1.cr1.modify(|_, w| w.pe().enabled());
    }
}
