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

/// Maximum number of humidity calibration data points to store
const HUMIDITY_CALIBRATION_SAMPLES: usize = 50;

/// Minimum samples required before applying calibration
const MIN_CALIBRATION_SAMPLES: usize = 10;

/// Outlier threshold (standard deviations)
const OUTLIER_THRESHOLD: f32 = 2.5;

/// Calibration learning rate (how quickly to adapt to new patterns)
const CALIBRATION_LEARNING_RATE: f32 = 0.1;

/// Simple sqrt implementation for embedded use
#[allow(clippy::manual_midpoint)]
fn sqrt_f32(x: f32) -> f32 {
    if x < 0.0 {
        return 0.0;
    }
    if x == 0.0 {
        return 0.0;
    }

    // Newton's method for square root
    let mut result = x / 2.0;
    for _ in 0..10 {
        result = (result + x / result) / 2.0;
    }
    result
}

/// Humidity calibration data point
#[derive(Clone, Copy)]
struct HumidityDataPoint {
    /// Temperature measurement in Celsius
    temperature: f32,
    /// Raw humidity measurement in percentage
    raw_humidity: f32,
    /// Simple timestamp in hours since start (for future use)
    _timestamp_hours: u32,
}

/// Statistical humidity calibration system
struct HumidityCalibrator {
    /// Historical data points for statistical analysis
    data_points: Vec<HumidityDataPoint, HUMIDITY_CALIBRATION_SAMPLES>,
    /// Current calibration offset
    humidity_offset: f32,
    /// Runtime hours counter
    runtime_hours: u32,
    /// Number of samples used in current offset calculation
    offset_sample_count: u32,
}

impl HumidityCalibrator {
    /// Create a new humidity calibrator
    const fn new() -> Self {
        Self {
            data_points: Vec::new(),
            humidity_offset: 0.0,
            runtime_hours: 0,
            offset_sample_count: 0,
        }
    }

    /// Update runtime hours (call this periodically)
    const fn update_runtime(&mut self, hours: u32) {
        self.runtime_hours = hours;
    }

    /// Expected indoor humidity based on temperature (empirical model)
    /// Indoor environments typically maintain 30-60% RH, with seasonal variations
    fn expected_indoor_humidity(temperature_c: f32) -> f32 {
        // Empirical model for indoor humidity based on temperature
        // Cooler indoor temps tend to have higher relative humidity
        // Warmer indoor temps tend to have lower relative humidity
        let base_humidity = 45.0; // Base indoor humidity target
        let temp_coefficient = -0.5; // RH decreases as temperature increases
        let seasonal_variation = 5.0; // Account for seasonal HVAC differences

        let expected = base_humidity + (25.0 - temperature_c) * temp_coefficient;

        // Clamp to reasonable indoor range with seasonal variation
        expected.clamp(30.0 - seasonal_variation, 60.0 + seasonal_variation)
    }

    /// Calculate statistical measures for stored data
    #[allow(clippy::cast_precision_loss)]
    fn calculate_statistics(&self) -> Option<(f32, f32)> {
        if self.data_points.len() < MIN_CALIBRATION_SAMPLES {
            return None;
        }

        let mut humidity_errors = Vec::<f32, HUMIDITY_CALIBRATION_SAMPLES>::new();

        for point in &self.data_points {
            let expected = Self::expected_indoor_humidity(point.temperature);
            let error = point.raw_humidity - expected;
            let _ = humidity_errors.push(error);
        }

        let mean_error = humidity_errors.iter().sum::<f32>() / humidity_errors.len() as f32;

        let variance = humidity_errors
            .iter()
            .map(|&e| (e - mean_error) * (e - mean_error))
            .sum::<f32>()
            / humidity_errors.len() as f32;
        let std_dev = sqrt_f32(variance);

        Some((mean_error, std_dev))
    }

    /// Detect if a reading is an outlier
    fn is_outlier(&self, temperature: f32, raw_humidity: f32) -> bool {
        if let Some((mean_error, std_dev)) = self.calculate_statistics() {
            let expected = Self::expected_indoor_humidity(temperature);
            let current_error = raw_humidity - expected;
            let z_score = (current_error - mean_error).abs() / std_dev.max(1.0);
            z_score > OUTLIER_THRESHOLD
        } else {
            false // Not enough data to determine outliers
        }
    }

    /// Add a new humidity measurement for calibration
    fn add_measurement(&mut self, temperature: f32, raw_humidity: f32) {
        // Don't add obvious outliers to the calibration dataset
        if !self.is_outlier(temperature, raw_humidity) {
            let data_point = HumidityDataPoint {
                temperature,
                raw_humidity,
                _timestamp_hours: self.runtime_hours,
            };

            // Add to circular buffer
            if self.data_points.len() >= HUMIDITY_CALIBRATION_SAMPLES {
                // Remove oldest sample
                self.data_points.remove(0);
            }
            let _ = self.data_points.push(data_point);

            // Update calibration offset with exponential moving average
            self.update_calibration_offset();
        }
    }

    /// Update the calibration offset based on current data
    fn update_calibration_offset(&mut self) {
        if let Some((mean_error, _)) = self.calculate_statistics() {
            // Use exponential moving average to adapt the offset
            if self.offset_sample_count == 0 {
                self.humidity_offset = -mean_error; // Negative because we want to correct the error
                self.offset_sample_count = 1;
            } else {
                // Exponential moving average
                self.humidity_offset = self.humidity_offset * (1.0 - CALIBRATION_LEARNING_RATE)
                    + (-mean_error) * CALIBRATION_LEARNING_RATE;
            }
        }
    }

    /// Apply calibration to a humidity reading
    fn calibrate_humidity(&self, _temperature: f32, raw_humidity: f32) -> f32 {
        if self.data_points.len() < MIN_CALIBRATION_SAMPLES {
            // Not enough data for calibration, return raw value
            return raw_humidity;
        }

        // Apply the calibration offset
        let calibrated = raw_humidity + self.humidity_offset;

        // Sanity check: clamp to physically reasonable humidity range
        calibrated.clamp(0.0, 100.0)
    }

    /// Get calibration status information
    fn get_calibration_info(&self) -> (bool, f32, usize) {
        let is_calibrated = self.data_points.len() >= MIN_CALIBRATION_SAMPLES;
        (is_calibrated, self.humidity_offset, self.data_points.len())
    }
}

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

    // Add measurement to calibrator for learning
    humidity_calibrator.add_measurement(raw_temp, raw_rh);

    // Apply calibration
    let calibrated_rh = humidity_calibrator.calibrate_humidity(raw_temp, raw_rh);

    let readings = Aht21Readings {
        raw_temperature: raw_temp,
        display_temperature: raw_temp + AHT21_TEMPERATURE_OFFSET,
        raw_humidity: raw_rh,
        calibrated_humidity: calibrated_rh,
    };

    let (is_calibrated, offset, sample_count) = humidity_calibrator.get_calibration_info();

    info!(
        "Temperature: {}°C (raw: {}°C), Humidity: {}% -> {}% (raw->cal), Calibration: {} (offset: {}, samples: {})",
        readings.display_temperature,
        readings.raw_temperature,
        readings.raw_humidity,
        readings.calibrated_humidity,
        if is_calibrated { "ACTIVE" } else { "LEARNING" },
        offset,
        sample_count
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

// Note: ENS160 operates in continuous Standard mode for reliable measurements.
// Sleep/wake cycles have been removed as they disrupt sensor stability and calibration,
// leading to unreliable readings after wake cycles. Continuous operation provides
// consistent and accurate air quality measurements.

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
                humidity: aht21_readings.calibrated_humidity,    // Use calibrated humidity for UI
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
    let mut runtime_hours = 0u32;

    info!("Sensor task initialized successfully with humidity calibration");
    report_task_success(task_id).await;

    // Wait for ENS160 warmup period before starting readings
    info!("Waiting for ENS160 warmup period of {} seconds", WARMUP_TIME);
    Timer::after_secs(WARMUP_TIME).await;

    loop {
        // Update runtime hours for calibrator (approximately)
        #[allow(clippy::cast_possible_truncation)]
        {
            runtime_hours += 1; // Increment by 1 hour equivalent per reading cycle
        }
        humidity_calibrator.update_runtime(runtime_hours);

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
