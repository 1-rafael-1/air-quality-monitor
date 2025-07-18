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
    data::{AirQualityIndex, InterruptPinConfig},
};
use heapless::Vec;
use moving_median::MovingMedian;
use panic_probe as _;

use crate::{
    event::{Event, send_event},
    humidity_calibrator::HumidityCalibrator,
    watchdog::{TaskId, report_task_failure, report_task_success},
};

/// Temperature offset for AHT21 sensor in degrees Celsius
static AHT21_TEMPERATURE_OFFSET: f32 = -3.5;

/// Warmup time for ENS160 sensor in seconds
const WARMUP_TIME: u64 = 180;

/// Read interval for continuous operation (5 minutes)
const READ_INTERVAL: u64 = 300;

/// Number of readings for ENS160 median calculation
const ENS160_MEDIAN_READINGS: usize = 3;

/// Initialize the AHT21 sensor
async fn initialize_aht21(
    aht21_device: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
) -> Option<Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>> {
    let mut aht21 = Aht20::new(aht21_device, Delay).await.ok()?;
    Timer::after_millis(100).await;
    info!("calibrate aht21");
    aht21.calibrate().await.ok()?;
    info!("AHT21 calibration successful");
    Timer::after_millis(1000).await;
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
        return None;
    }
    info!("ENS160 initialized successfully");

    Some(ens160)
}

/// Struct to hold AHT21 sensor readings
struct Aht21Readings {
    /// Raw temperature in degrees Celsius (for ENS160 compensation)
    raw_temperature: f32,
    /// Display temperature in degrees Celsius (with offset applied)
    display_temperature: f32,
    /// Raw humidity in percentage (uncalibrated)
    raw_humidity: f32,
    /// Calibrated humidity in percentage
    calibrated_humidity: f32,
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
async fn read_aht21(
    aht21: &mut Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    humidity_calibrator: &mut HumidityCalibrator,
) -> Result<Aht21Readings, &'static str> {
    let (hum, temp) = aht21.read().await.map_err(|_| "Failed to read AHT21 sensor")?;
    let raw_temp = temp.celsius();
    let raw_rh = hum.rh();

    // Add measurement to calibrator for learning (this also detects rapid changes)
    humidity_calibrator.add_measurement(raw_temp, raw_rh);

    // Apply calibration (this preserves rapid changes while applying offset corrections)
    let calibrated_rh = humidity_calibrator.calibrate_humidity(raw_temp, raw_rh);

    let readings = Aht21Readings {
        raw_temperature: raw_temp,
        display_temperature: raw_temp + AHT21_TEMPERATURE_OFFSET,
        raw_humidity: raw_rh,
        calibrated_humidity: calibrated_rh,
    };

    let (is_calibrated, baseline_offset, statistical_offset, sample_count, in_rapid_change, long_term_count) =
        humidity_calibrator.get_calibration_info();
    let calibration_status = if !is_calibrated {
        "ESTABLISHING_BASELINE"
    } else if in_rapid_change {
        if humidity_calibrator.baseline_shifted {
            "BASELINE_SHIFT"
        } else {
            "RAPID_CHANGE"
        }
    } else {
        "HYBRID_DRIFT_CORRECTION"
    };

    info!(
        "Temperature: {}°C (raw: {}°C), Humidity: {}% -> {}% (raw->cal), Calibration: {} (baseline offset: {}, statistical offset: {}, samples: {}, long-term count: {})",
        readings.display_temperature,
        readings.raw_temperature,
        readings.raw_humidity,
        readings.calibrated_humidity,
        calibration_status,
        baseline_offset,
        statistical_offset,
        sample_count,
        long_term_count
    );

    Ok(readings)
}

/// Read data from ENS160 sensor
/// Uses moving median of 3 readings taken, using interrupt to ensure complete data
/// Note: Temperature and humidity compensation should be set separately using `set_ens160_compensation`
async fn read_ens160(
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    int: &mut Input<'static>,
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
        let _ = co2_aqi_pairs.push((co2_value, aq));
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

/// Set temperature and humidity compensation on ENS160 sensor
/// Uses raw temperature (without offset correction) for accurate sensor compensation
async fn set_ens160_compensation(
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    temp: f32,
    rh: f32,
) -> Result<(), &'static str> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    ens160
        .set_temp_rh_comp(temp, rh as u16)
        .await
        .map_err(|_| "Failed to set temperature and humidity compensation")?;
    Timer::after_millis(100).await;
    Ok(())
}

/// Initialize both sensors and configure them for operation
async fn initialize_sensors(
    aht21_device: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    ens160_device: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    _ens160_int: &mut Input<'static>,
) -> Result<
    (
        Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
        Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    ),
    &'static str,
> {
    let Some(aht21) = initialize_aht21(aht21_device).await else {
        return Err("Failed to initialize AHT21");
    };

    let Some(mut ens160) = initialize_ens160(ens160_device).await else {
        return Err("Failed to initialize ENS160");
    };

    // Configure ENS160 interrupt pin
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
            info!("Failed to configure ENS160 interrupt pin: {}", Debug2Format(&e));
            return Err("Failed to configure ENS160 interrupt pin");
        }
    }

    // ENS160 is initialized in Standard mode and remains in continuous operation
    // for reliable measurements and proper calibration
    info!("ENS160 configured for continuous operation in Standard mode");

    Ok((aht21, ens160))
}

/// Execute one iteration of the sensor reading loop
/// ENS160 operates continuously in Standard mode for reliable measurements
async fn handle_sensor_iteration(
    aht21: &mut Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    ens160_int: &mut Input<'static>,
    prev_temp: &mut f32,
    prev_humidity: &mut f32,
    humidity_calibrator: &mut HumidityCalibrator,
) -> bool {
    // Read AHT21 data first to get current environmental conditions
    let aht21_result = read_aht21(aht21, humidity_calibrator).await;
    if let Ok(ref aht21_readings) = aht21_result {
        *prev_temp = aht21_readings.raw_temperature; // Use raw temperature for ENS160 compensation
        *prev_humidity = aht21_readings.calibrated_humidity; // Use calibrated humidity
    }

    // Set temperature and humidity compensation using latest readings
    if let Err(e) = set_ens160_compensation(ens160, *prev_temp, *prev_humidity).await {
        info!("ENS160 compensation setting failed: {}", e);
        return false; // Indicate failure
    }

    let ens160_result = read_ens160(ens160, ens160_int).await;

    // Process readings
    match (ens160_result, aht21_result) {
        (Ok(ens160_readings), Ok(aht21_readings)) => {
            send_event(Event::SensorData {
                temperature: aht21_readings.display_temperature, // Use display temperature for UI
                raw_temperature: aht21_readings.raw_temperature, // Send raw temperature
                humidity: aht21_readings.calibrated_humidity,    // Use calibrated humidity for UI
                raw_humidity: aht21_readings.raw_humidity,       // Send raw humidity
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                co2: ens160_readings.co2 as u16,
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                etoh: ens160_readings.etoh as u16,
                air_quality: ens160_readings.air_quality,
            })
            .await;

            info!("Sensor task: successful");
            true // Indicate success
        }
        (Err(ens160_err), Err(aht21_err)) => {
            info!("Both sensors failed - ENS160: {}, AHT21: {}", ens160_err, aht21_err);
            false // Indicate failure
        }
        (Err(ens160_err), Ok(_)) => {
            info!("ENS160 reading failed: {}", ens160_err);
            false // Indicate failure
        }
        (Ok(_), Err(aht21_err)) => {
            info!("AHT21 reading failed: {}", aht21_err);
            false // Indicate failure
        }
    }
}

#[embassy_executor::task]
pub async fn sensor_task(
    aht21: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    ens160: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    mut ens160_int: Input<'static>,
) {
    let task_id = TaskId::Sensor;

    // Initialize both sensors
    let (mut aht21, mut ens160) = match initialize_sensors(aht21, ens160, &mut ens160_int).await {
        Ok(sensors) => sensors,
        Err(e) => {
            info!("Sensor initialization failed: {}", e);
            report_task_failure(task_id).await;
            return;
        }
    };

    // Store previous AHT21 readings for ENS160 compensation
    let mut prev_temp = 25.0; // Default raw temperature (without offset)
    let mut prev_humidity = 50.0; // Default humidity

    // Initialize humidity calibrator
    let mut humidity_calibrator = HumidityCalibrator::new();

    info!("Sensor task initialized successfully with humidity calibration");
    report_task_success(task_id).await;

    // Wait for ENS160 warmup period before starting readings
    info!("Waiting for ENS160 warmup period of {} seconds", WARMUP_TIME);
    Timer::after_secs(WARMUP_TIME).await;

    loop {
        // Execute one iteration of the sensor reading loop
        let success = handle_sensor_iteration(
            &mut aht21,
            &mut ens160,
            &mut ens160_int,
            &mut prev_temp,
            &mut prev_humidity,
            &mut humidity_calibrator,
        )
        .await;

        if success {
            report_task_success(task_id).await;
        } else {
            report_task_failure(task_id).await;
        }

        // Wait for the next reading interval (5 minutes)
        Timer::after_secs(READ_INTERVAL).await;
    }
}
