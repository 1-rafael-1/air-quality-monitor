//! Sensor task for reading data from AHT21 and ENS160 sensors.
use aht20_async::Aht20;
use defmt::{Debug2Format, info};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::{
    gpio::Input,
    i2c::{Async, I2c},
    peripherals::I2C0,
};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_time::{Delay, Timer};
use ens160_aq::{
    Ens160,
    data::{AirQualityIndex, InterruptPinConfig, OperationMode, ValidityFlag},
};
use heapless::Vec;
use moving_median::MovingMedian;
use panic_probe as _;

use crate::{
    event::{Event, send_event},
    watchdog::trigger_watchdog_reset,
};

/// Temperature offset for AHT21 sensor in degrees Celsius
static AHT21_TEMPERATURE_OFFSET: f32 = -3.5;

/// Warmup time for ENS160 sensor in seconds
const WARMUP_TIME: u64 = 180;

/// Idle time for ENS160 sensor in seconds to conserve power
const IDLE_TIME: u64 = 120;

/// Number of readings for ENS160 median calculation
const ENS160_MEDIAN_READINGS: usize = 3;

/// Interval between ENS160 readings for median calculation (in seconds)
const ENS160_READ_INTERVAL: u64 = 10;

/// Initialize the AHT21 sensor
async fn initialize_aht21(
    aht21_device: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
) -> Option<Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>> {
    let mut aht21 = Aht20::new(aht21_device, Delay).await.ok()?;
    info!("calibrate aht21");
    aht21.calibrate().await.ok()?;
    info!("AHT21 calibration successful");
    Timer::after_millis(1000).await;
    info!("done calibrating");
    Some(aht21)
}

/// Initialize the ENS160 sensor
async fn initialize_ens160(
    ens160_device: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
) -> Option<Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>> {
    let mut ens160 = Ens160::new(ens160_device, Delay);

    if let Err(e) = ens160.initialize().await {
        info!(
            "Failed to initialize ENS160: {} - triggering system reset",
            Debug2Format(&e)
        );
        trigger_watchdog_reset();
        return None;
    }
    info!("ENS160 initialized successfully");

    Some(ens160)
}

/// Wrapper for `ValidityFlag` to add `PartialEq`
#[derive(Debug, Copy, Clone)]
pub struct ValidityFlagWrapper(pub ValidityFlag);

impl PartialEq for ValidityFlagWrapper {
    fn eq(&self, other: &Self) -> bool {
        // Compare based on the discriminants since ValidityFlag doesn't implement PartialEq
        core::mem::discriminant(&self.0) == core::mem::discriminant(&other.0)
    }
}

impl From<ValidityFlag> for ValidityFlagWrapper {
    fn from(flag: ValidityFlag) -> Self {
        Self(flag)
    }
}

/// Struct to hold AHT21 sensor readings
struct Aht21Readings {
    /// Temperature in degrees Celsius
    temperature: f32,
    /// Humidity in percentage
    humidity: f32,
}

/// Struct to hold ENS160 sensor readings
struct Ens160Readings {
    /// eCO2 level in ppm
    co2: f32,
    /// Ethanol (TVOC) level in ppb
    etoh: f32,
    /// Air quality index data
    air_quality: AirQualityIndex,
}

/// Read data from AHT21 sensor
async fn read_aht21_data(
    aht21: &mut Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
) -> Result<Aht21Readings, &'static str> {
    let (hum, temp) = aht21.read().await.map_err(|_| "Failed to read AHT21 sensor")?;
    let (temp, rh) = (temp.celsius() + AHT21_TEMPERATURE_OFFSET, hum.rh());

    let readings = Aht21Readings {
        temperature: temp,
        humidity: rh,
    };

    info!(
        "Temperature: {}°C, Humidity: {}%",
        readings.temperature, readings.humidity
    );

    Ok(readings)
}

/// Read data from ENS160 sensor with temperature and humidity compensation
/// Uses moving median of 3 readings taken 10 seconds apart, using interrupt to ensure complete data
async fn read_ens160_data(
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    int: &mut Input<'static>,
    temp: f32,
    rh: f32,
) -> Result<Ens160Readings, &'static str> {
    let mut co2_median = MovingMedian::<f32, ENS160_MEDIAN_READINGS>::new();
    let mut etoh_median = MovingMedian::<f32, ENS160_MEDIAN_READINGS>::new();
    let mut co2_aqi_pairs: Vec<(f32, AirQualityIndex), ENS160_MEDIAN_READINGS> = Vec::new();

    for i in 0..ENS160_MEDIAN_READINGS {
        info!("ENS160 reading {} of {}", i + 1, ENS160_MEDIAN_READINGS);

        // Wait for interrupt to ensure sensor has new data ready
        int.wait_for_low().await;
        info!("ENS160 interrupt received - data ready");

        let status = ens160.get_status().await.map_err(|_| "Failed to get ENS160 status")?;
        info!("ENS160 status: {}", Debug2Format(&status));

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        ens160
            .set_temp_rh_comp(temp, rh as u16)
            .await
            .map_err(|_| "Failed to set temperature and humidity compensation")?;
        Timer::after_millis(100).await;

        let eco2 = ens160.get_eco2().await.map_err(|_| "Failed to get eCO2")?;
        let etoh = ens160.get_etoh().await.map_err(|_| "Failed to get ethanol")?;
        let aq = ens160
            .get_airquality_index()
            .await
            .map_err(|_| "Failed to get Air Quality Index")?;

        let co2_value = f32::from(eco2.get_value());
        let etoh_value = f32::from(etoh);

        info!(
            "Reading {}: Air Quality Index: {}, eCO2: {} ppm, Ethanol: {} ppb",
            i + 1,
            Debug2Format(&aq),
            co2_value,
            etoh_value
        );

        co2_median.add_value(co2_value);
        etoh_median.add_value(etoh_value);
        let _ = co2_aqi_pairs.push((co2_value, aq)); // Store CO2-AQI pair

        // Wait 10 seconds before next reading (except for the last one)
        if i < ENS160_MEDIAN_READINGS - 1 {
            info!("Waiting {} seconds before next ENS160 reading", ENS160_READ_INTERVAL);
            Timer::after_secs(ENS160_READ_INTERVAL).await;
        }
    }

    let median_co2 = co2_median.median();

    // Find the AQI that corresponds to the CO2 value closest to the median
    let air_quality = co2_aqi_pairs
        .iter()
        .min_by(|(co2_a, _), (co2_b, _)| {
            let diff_a = (co2_a - median_co2).abs();
            let diff_b = (co2_b - median_co2).abs();
            diff_a.partial_cmp(&diff_b).unwrap_or(core::cmp::Ordering::Equal)
        })
        .map(|(_, aqi)| *aqi)
        .ok_or("No CO2-AQI pairs available")?;

    let readings = Ens160Readings {
        co2: median_co2,
        etoh: etoh_median.median(),
        air_quality,
    };

    info!(
        "ENS160 median results - Air Quality Index: {}, eCO2: {} ppm, Ethanol: {} ppb",
        Debug2Format(&readings.air_quality),
        readings.co2,
        readings.etoh
    );

    Ok(readings)
}

/// Put ENS160 to idle for power conservation
async fn ens160_idle(
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
) -> Result<(), &'static str> {
    info!("Putting ENS160 sensor to idle mode");
    // first check the status, if ens160 is in Initial Startup Phase, we must not put it to sleep
    let status = ens160
        .get_status()
        .await
        .map_err(|_| "Failed to get ENS160 status before idle mode")?;

    if matches!(status.validity_flag(), ValidityFlag::InitialStartupPhase) {
        info!("ENS160 is still in Initial Startup Phase - cannot put to idle mode");
        return Ok(());
    }

    ens160
        .set_operation_mode(OperationMode::Idle)
        .await
        .map_err(|_| "Failed to set ENS160 to Idle mode")?;
    info!("ENS160 set to Idle mode");

    Ok(())
}

/// Set ENS160 to Standard mode and wait for it to be ready.
async fn ens160_wake(
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    int: &mut Input<'static>,
) -> Result<(), &'static str> {
    info!("Waking up ENS160 sensor");
    ens160
        .set_operation_mode(OperationMode::Standard)
        .await
        .map_err(|_| "Failed to set ENS160 to Standard mode")?;

    Timer::after_secs(WARMUP_TIME).await;

    // Read the status
    let status = ens160
        .get_status()
        .await
        .map_err(|_| "Failed to get ENS160 status after setting to Standard mode")?;
    info!("ENS160 status: {}", Debug2Format(&status));

    // get measurements to ensure sensor is ready, flushing bad first readings
    let _ = ens160
        .get_measurements()
        .await
        .map_err(|_| "Failed to get ENS160 measurements after waking up")?;

    // first wait for the interrupt to be triggered, indicating the sensor has data ready, so that we can read current status
    info!("currently is interrupt active:{}", int.is_low());
    int.wait_for_low().await;
    info!("ENS160 sensor is awake and ready for measurements");

    Ok(())
}

/// Sensor task for reading data from AHT21 and ENS160 sensors.
#[embassy_executor::task]
pub async fn sensor_task(
    aht21: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    ens160: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    #[allow(clippy::used_underscore_binding)] mut ens160_int: Input<'static>,
) {
    let Some(mut aht21) = initialize_aht21(aht21).await else {
        info!("Failed to initialize AHT21 - triggering system reset");
        trigger_watchdog_reset();
        return;
    };

    let Some(mut ens160) = initialize_ens160(ens160).await else {
        info!("Failed to initialize ENS160 - triggering system reset");
        trigger_watchdog_reset();
        return;
    };

    match ens160
        .config_interrupt_pin(
            InterruptPinConfig::builder()
                .push_pull()
                .on_new_data()
                .enable_interrupt()
                .build(),
        )
        .await
    {
        Ok(val) => {
            info!("ENS160 interrupt pin configured successfully to {}", val);
        }
        Err(e) => {
            info!(
                "Failed to configure ENS160 interrupt pin: {} - triggering system reset",
                Debug2Format(&e)
            );
            trigger_watchdog_reset();
            return;
        }
    }

    // Store previous AHT21 readings for ENS160 compensation
    let mut prev_temp = 25.0; // Default temperature
    let mut prev_humidity = 50.0; // Default humidity

    info!("Sensor task initialized successfully");

    loop {
        // Read AHT21 data after cooling period to get accurate readings
        let aht21_result = read_aht21_data(&mut aht21).await;

        // Update stored values for ENS160 compensation if AHT21 reading was successful
        if let Ok(ref aht21_readings) = aht21_result {
            prev_temp = aht21_readings.temperature;
            prev_humidity = aht21_readings.humidity;
        }

        // Wake up ENS160 sensor and wait for warmup
        if let Err(e) = ens160_wake(&mut ens160, &mut ens160_int).await {
            info!("ENS160 wake failed (continuing): {}", e);
            // Continue in the loop - all errors in the loop are considered transient
        }

        // Read ENS160 data using current AHT21 readings for compensation
        let ens160_result = read_ens160_data(&mut ens160, &mut ens160_int, prev_temp, prev_humidity).await;

        // Put ENS160 to sleep for power conservation immediately after reading
        if let Err(e) = ens160_idle(&mut ens160).await {
            info!("ENS160 sleep failed (continuing): {}", e);
        }

        // Combine readings and send event if both sensors read successfully
        match (ens160_result, aht21_result) {
            (Ok(ens160_readings), Ok(aht21_readings)) => {
                send_event(Event::SensorData {
                    temperature: aht21_readings.temperature,
                    humidity: aht21_readings.humidity,
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    co2: ens160_readings.co2 as u16,
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    etoh: ens160_readings.etoh as u16,
                    air_quality: ens160_readings.air_quality,
                })
                .await;
            }
            (Err(ens160_err), Err(aht21_err)) => {
                info!("Both sensors failed - ENS160: {}, AHT21: {}", ens160_err, aht21_err);
            }
            (Err(ens160_err), Ok(_)) => {
                info!("ENS160 reading failed (continuing): {}", ens160_err);
            }
            (Ok(_), Err(aht21_err)) => {
                info!("AHT21 reading failed (continuing): {}", aht21_err);
            }
        }

        Timer::after_secs(IDLE_TIME).await;
    }
}
