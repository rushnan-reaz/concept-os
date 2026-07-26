use crate::i2c::I2C_Channel;
use bthermo_api::ThermoError;
use userlib::sys_get_timer;

/**
 * TMP102
 *
 * NOTE: this board actually carries a TMP102, not a TMP117 (this module was
 * originally written for the TMP117; the struct/module name was kept to avoid
 * a wider rename, but the register interpretation below targets the TMP102).
 *
 * The TMP117 and TMP102 are TI parts that share the same I2C address-pin
 * strapping table (ADD0: GND=0x48, V+=0x49, SDA=0x4A, SCL=0x4B), so the
 * address is kept as-is here — verify against the board if readings still
 * fail. Unlike the TMP117, the TMP102 has **no device-ID register at 0x0F**,
 * so the old ID check always failed here (root cause of TempNotConnected).
 * The TMP102's temperature register is a 12-bit value left-justified in the
 * 16-bit read (hence the `>> 4`), at 0.0625 °C/LSB — not the TMP117's
 * 0.0078125 °C/LSB (16x finer). The TMP102's config register layout also
 * differs from the TMP117's; rather than guess at its bit-level semantics,
 * `init_hardware` no longer writes it and relies on the TMP102's power-on
 * default, which is already continuous-conversion mode.
 *
 * Datasheet: https://www.ti.com/lit/ds/symlink/tmp102.pdf
 */
const TMP102_ADDR: u8 = 0x49;

const TMP102_REG_TEMPERATURE: u8 = 0x00;

const TMP102_RESOLUTION: f32 = 0.0625_f32;
const UPDATE_MS: u64 = 1000;

pub struct TMP117 {
    last_update: u64,
    last_temp: f32,
}

impl TMP117 {
    pub fn new() -> Self {
        Self {
            last_update: 0,
            last_temp: 0.0,
        }
    }
    pub fn init_hardware(&mut self, i2c: &mut I2C_Channel) -> Result<(), ThermoError> {
        // The TMP102 has no device-ID register; confirm the sensor is present
        // by reading the temperature register once instead. It powers up
        // already in continuous-conversion mode, so no config write is needed.
        let mut probe: [u8; 2] = [0; 2];
        i2c.i2c_mem_read(TMP102_ADDR, TMP102_REG_TEMPERATURE, &mut probe)
            .map_err(|_| ThermoError::TempNotConnected)
    }

    pub fn read_temperature(&mut self, i2c: &mut I2C_Channel) -> Result<f32, ThermoError> {
        // Avoid reading too often. The temperature updates
        // every 1 second
        let now = sys_get_timer().now;
        if now - self.last_update > UPDATE_MS {
            let mut raw_data: [u8; 2] = [0; 2];
            // Read data from the sensor
            i2c.i2c_mem_read(TMP102_ADDR, TMP102_REG_TEMPERATURE, &mut raw_data)
                .map_err(|_| ThermoError::TempNotConnected)?;
            // Convert it: 12-bit value left-justified in the 16-bit register
            let raw_temp = i16::from_be_bytes(raw_data) >> 4;
            // Store it
            self.last_temp = (raw_temp as f32) * TMP102_RESOLUTION;
            self.last_update = now;
        }
        return Ok(self.last_temp);
    }
}
