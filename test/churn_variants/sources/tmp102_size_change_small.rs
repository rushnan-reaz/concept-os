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

pub struct TMP102 {
    last_update: u64,
    last_temp: f32,
}

impl TMP102 {
    pub fn new() -> Self {
        Self {
            last_update: 0,
            last_temp: 0.0,
        }
    }
    pub fn init_hardware(&mut self, i2c: &mut I2C_Channel, rcc: &mut RCC) -> Result<(), ThermoError> {
        led2_init(rcc);
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
            led2_toggle();
        }
        return Ok(self.last_temp);
    }
}
