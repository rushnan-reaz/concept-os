use crate::i2c::I2C_Channel;
use bthermo_api::ThermoError;
use userlib::sys_get_timer;

/**
 * TMP102 digital temperature sensor driver.
 *
 * Talks to the sensor over I2C at its fixed 7-bit address. Runs in the
 * sensor's default 12-bit continuous-conversion mode (0.0625 degC per LSB
 * of the raw reading). There is no dedicated device-ID register on this
 * part the way there is on TMP117, so this driver treats a successful
 * register read as confirmation the device is present and responding,
 * rather than checking an identity value.
 *
 * Reference: https://www.ti.com/lit/ds/symlink/tmp102.pdf
 */
const TMP102_ADDR: u8 = 0x49;
const TMP102_REG_TEMP: u8 = 0x00;
const TMP102_REG_CFG: u8 = 0x01;
const TMP102_LSB_DEGREES: f32 = 0.0625_f32;
const READ_INTERVAL_MS: u64 = 750;

/// Raw 16-bit big-endian temperature word -> degrees Celsius, per the
/// sensor's default 12-bit format (top 12 bits of the word are significant,
/// each LSB worth `TMP102_LSB_DEGREES`).
fn raw_word_to_celsius(raw_be: [u8; 2]) -> f32 {
    let raw = i16::from_be_bytes(raw_be);
    (raw >> 4) as f32 * TMP102_LSB_DEGREES
}

pub struct TMP102 {
    last_read_at: u64,
    last_celsius: f32,
}

impl TMP102 {
    pub fn new() -> Self {
        Self {
            last_read_at: 0,
            last_celsius: 0.0,
        }
    }

    pub fn init_hardware(&mut self, i2c: &mut I2C_Channel) -> Result<(), ThermoError> {
        self.probe_presence(i2c)?;
        self.apply_default_config(i2c)
    }

    fn probe_presence(&self, i2c: &mut I2C_Channel) -> Result<(), ThermoError> {
        let mut scratch: [u8; 2] = [0; 2];
        i2c.i2c_mem_read(TMP102_ADDR, TMP102_REG_TEMP, &mut scratch)
            .map_err(|_| ThermoError::TempNotConnected)
    }

    fn apply_default_config(&self, i2c: &mut I2C_Channel) -> Result<(), ThermoError> {
        // Explicitly re-assert the power-on-default config (12-bit,
        // continuous conversion) rather than assuming it survived any
        // preceding bus reset.
        const DEFAULT_CFG: [u8; 2] = [0x60, 0xA8];
        i2c.i2c_mem_write(TMP102_ADDR, TMP102_REG_CFG, &DEFAULT_CFG)
            .map_err(|_| ThermoError::TempNotConnected)
    }

    fn due_for_read(&self, now: u64) -> bool {
        now - self.last_read_at > READ_INTERVAL_MS
    }

    fn fetch_raw(&self, i2c: &mut I2C_Channel) -> Result<[u8; 2], ThermoError> {
        let mut raw: [u8; 2] = [0; 2];
        i2c.i2c_mem_read(TMP102_ADDR, TMP102_REG_TEMP, &mut raw)
            .map_err(|_| ThermoError::TempNotConnected)?;
        Ok(raw)
    }

    pub fn read_temperature(&mut self, i2c: &mut I2C_Channel) -> Result<f32, ThermoError> {
        let now = sys_get_timer().now;
        if self.due_for_read(now) {
            let raw = self.fetch_raw(i2c)?;
            self.last_celsius = raw_word_to_celsius(raw);
            self.last_read_at = now;
        }
        Ok(self.last_celsius)
    }
}
