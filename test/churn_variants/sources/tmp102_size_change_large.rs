use crate::i2c::I2C_Channel;
use bthermo_api::ThermoError;
use rcc_api::RCC;
use stm32l476rg::device;
use userlib::sys_get_timer;

/**
 * TMP102
 *
 * Low-power digital temperature sensor, I2C, default 12-bit resolution
 * (0.0625 degC/LSB). No device-ID register (unlike TMP117) -- presence is
 * confirmed via a successful register read instead.
 *
 * Datasheet: https://www.ti.com/lit/ds/symlink/tmp102.pdf
 */
const TMP102_ADDR: u8 = 0x49;

const TMP102_REG_TEMPERATURE: u8 = 0x00;
const TMP102_REG_CONFIGURATION: u8 = 0x01;

const TMP102_RESOLUTION: f32 = 0.0625_f32;
const UPDATE_MS: u64 = 1000;

/// Above this temperature the LED blinks fast instead of slow, as a crude
/// visual over-temperature indicator with no dependency on the RTC/output
/// components.
const HOT_THRESHOLD_C: f32 = 28.0_f32;
const BLINK_SLOW_MS: u64 = 1000;
const BLINK_FAST_MS: u64 = 250;

/// LD2 (green user LED on the NUCLEO-L476RG), PA5.
const LED2_PIN: u32 = 5;

fn led2_init(rcc: &mut RCC) {
    let _ = rcc.enable_clock(rcc_api::Peripheral::GPIOA);
    let gpioa = unsafe { &*device::GPIOA::PTR };
    gpioa.moder.modify(|_, w| w.moder5().output());
}

fn led2_toggle() {
    let gpioa = unsafe { &*device::GPIOA::PTR };
    let odr = gpioa.odr.read().bits();
    if odr & (1 << LED2_PIN) != 0 {
        gpioa.bsrr.write(|w| unsafe { w.bits(1 << (LED2_PIN + 16)) });
    } else {
        gpioa.bsrr.write(|w| unsafe { w.bits(1 << LED2_PIN) });
    }
}

fn led2_set(on: bool) {
    let gpioa = unsafe { &*device::GPIOA::PTR };
    if on {
        gpioa.bsrr.write(|w| unsafe { w.bits(1 << LED2_PIN) });
    } else {
        gpioa.bsrr.write(|w| unsafe { w.bits(1 << (LED2_PIN + 16)) });
    }
}

/// Drives LD2 in a blink pattern whose rate depends on the latest
/// temperature reading, independent of the sensor's own sample cadence.
struct BlinkController {
    last_toggle: u64,
}

impl BlinkController {
    fn new() -> Self {
        Self { last_toggle: 0 }
    }

    fn tick(&mut self, now: u64, latest_temp: f32) {
        let interval = if latest_temp > HOT_THRESHOLD_C {
            BLINK_FAST_MS
        } else {
            BLINK_SLOW_MS
        };
        if now - self.last_toggle > interval {
            led2_toggle();
            self.last_toggle = now;
        }
    }
}

/// Tracks the minimum, maximum, and a simple running average of every
/// temperature observed since boot.
struct TemperatureStats {
    min: f32,
    max: f32,
    sum: f32,
    count: u32,
    seeded: bool,
}

impl TemperatureStats {
    fn new() -> Self {
        Self {
            min: 0.0,
            max: 0.0,
            sum: 0.0,
            count: 0,
            seeded: false,
        }
    }

    fn observe(&mut self, sample: f32) {
        if !self.seeded {
            self.min = sample;
            self.max = sample;
            self.seeded = true;
        } else {
            if sample < self.min {
                self.min = sample;
            }
            if sample > self.max {
                self.max = sample;
            }
        }
        self.sum += sample;
        self.count += 1;
    }

    fn range(&self) -> f32 {
        self.max - self.min
    }

    fn average(&self) -> f32 {
        if self.count == 0 {
            0.0
        } else {
            self.sum / self.count as f32
        }
    }
}

pub struct TMP102 {
    last_update: u64,
    last_temp: f32,
    stats: TemperatureStats,
    blink: BlinkController,
}

impl TMP102 {
    pub fn new() -> Self {
        Self {
            last_update: 0,
            last_temp: 0.0,
            stats: TemperatureStats::new(),
            blink: BlinkController::new(),
        }
    }
    pub fn init_hardware(&mut self, i2c: &mut I2C_Channel, rcc: &mut RCC) -> Result<(), ThermoError> {
        led2_init(rcc);
        led2_set(false);
        // TMP102 has no device-ID register (unlike TMP117) -- confirm
        // presence with a benign read of the temperature register instead.
        let mut probe: [u8; 2] = [0; 2];
        i2c.i2c_mem_read(TMP102_ADDR, TMP102_REG_TEMPERATURE, &mut probe)
            .map_err(|_| ThermoError::TempNotConnected)?;
        // Default power-on config (12-bit, continuous conversion) is already
        // usable as-is; write it back explicitly so behavior doesn't depend
        // on power-on defaults surviving a bus reset.
        let config_bytes: [u8; 2] = [0x60, 0xA0];
        i2c.i2c_mem_write(TMP102_ADDR, TMP102_REG_CONFIGURATION, &config_bytes)
            .map_err(|_| ThermoError::TempNotConnected)
    }

    pub fn read_temperature(&mut self, i2c: &mut I2C_Channel) -> Result<f32, ThermoError> {
        // Avoid reading too often. The temperature updates every ~1 second
        // in the default conversion cycle.
        let now = sys_get_timer().now;
        if now - self.last_update > UPDATE_MS {
            let mut raw_data: [u8; 2] = [0; 2];
            i2c.i2c_mem_read(TMP102_ADDR, TMP102_REG_TEMPERATURE, &mut raw_data)
                .map_err(|_| ThermoError::TempNotConnected)?;
            let raw_temp = i16::from_be_bytes(raw_data);
            self.last_temp = ((raw_temp >> 4) as f32) * TMP102_RESOLUTION;
            self.last_update = now;
            self.stats.observe(self.last_temp);
        }
        self.blink.tick(now, self.last_temp);
        return Ok(self.last_temp);
    }

    /// Range (max - min) of every temperature observed since boot.
    pub fn temperature_range(&self) -> f32 {
        self.stats.range()
    }

    /// Running average of every temperature observed since boot.
    pub fn temperature_average(&self) -> f32 {
        self.stats.average()
    }
}
