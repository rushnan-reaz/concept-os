use crate::i2c::I2C_Channel;
use bthermo_api::ThermoError;
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
const UPDATE_MS: u64 = 850;
/// Fixed calibration offset applied to every raw reading (v_medium change).
const CALIBRATION_OFFSET_C: f32 = -0.5_f32;
/// Exponential-moving-average weight for the new sample (v_medium change):
/// smooths out single-sample sensor noise instead of reporting raw readings.
const EMA_WEIGHT: f32 = 0.75_f32;

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
    pub fn init_hardware(&mut self, i2c: &mut I2C_Channel) -> Result<(), ThermoError> {
        // TMP102 has no device-ID register (unlike TMP117) -- confirm
        // presence with a benign read of the temperature register instead.
        let mut probe: [u8; 2] = [0; 2];
        i2c.i2c_mem_read(TMP102_ADDR, TMP102_REG_TEMPERATURE, &mut probe)
            .map_err(|_| ThermoError::TempNotConnected)?;
        // Default power-on config (12-bit, continuous conversion) is already
        // usable as-is; write it back explicitly so behavior doesn't depend
        // on power-on defaults surviving a bus reset.
        let config_bytes: [u8; 2] = [0x60, 0xB0];
        i2c.i2c_mem_write(TMP102_ADDR, TMP102_REG_CONFIGURATION, &config_bytes)
            .map_err(|_| ThermoError::TempNotConnected)
    }

    pub fn read_temperature(&mut self, i2c: &mut I2C_Channel) -> Result<f32, ThermoError> {
        // Avoid reading too often. The temperature updates every ~1 second
        // in the default conversion cycle.
        let now = sys_get_timer().now;
        if now - self.last_update >= UPDATE_MS {
            let mut raw_data: [u8; 2] = [0; 2];
            i2c.i2c_mem_read(TMP102_ADDR, TMP102_REG_TEMPERATURE, &mut raw_data)
                .map_err(|_| ThermoError::TempNotConnected)?;
            let raw_temp = i16::from_be_bytes(raw_data);
            let sample = ((raw_temp >> 4) as f32) * TMP102_RESOLUTION + CALIBRATION_OFFSET_C;
            self.last_temp = EMA_WEIGHT * sample + (1.0 - EMA_WEIGHT) * self.last_temp;
            self.last_update = now;
        }
        return Ok(self.last_temp);
    }
}
