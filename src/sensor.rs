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
use embassy_time::{Delay, Instant, Timer};
use ens160_aq::{
    Ens160,
    data::{AirQualityIndex, InterruptPinConfig, OperationMode, ValidityFlag},
};
use heapless::Vec;
use moving_median::MovingMedian;
use panic_probe as _;

use crate::{
    event::{Event, send_event},
    system_state::SYSTEM_STATE,
    watchdog::{TaskId, report_task_failure, report_task_success},
};

/// Temperature offset for AHT21 sensor in degrees Celsius
static AHT21_TEMPERATURE_OFFSET: f32 = -2.3;

/// Warmup time for ENS160 sensor in seconds
const WARMUP_TIME: u64 = 180;

/// Idle time for ENS160 sensor in seconds to conserve power (3 minutes)
const IDLE_TIME: u64 = 180;

/// Initial calibration time for ENS160 sensor in seconds (25 hours)
const INITIAL_CALIBRATION_TIME: u64 = 25 * 60 * 60;

/// Time between readings during initial calibration period (30 seconds)
const CALIBRATION_READ_INTERVAL: u64 = 30;

/// Number of readings for ENS160 median calculation
const ENS160_MEDIAN_READINGS: usize = 3;

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
async fn read_aht21(
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

/// Put ENS160 to idle for power conservation
async fn idle_ens160(
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
) -> Result<(), &'static str> {
    info!("Putting ENS160 sensor to idle mode");
    // first check the status, if ens160 is in Initial Startup Phase, we must not put it to sleep
    let status = ens160
        .get_status()
        .await
        .map_err(|_| "Failed to get ENS160 status before idle mode")?;

    // Check if the sensor is in a valid state to put to idle mode
    // The ENS160 datasheet specifies that `InitialStartupPhase` occurs during the first 25 hours of operation.
    // Only after this initial calibration period should the sensor be put into idle mode for power management.
    if matches!(status.validity_flag(), ValidityFlag::InitialStartupPhase)
        || matches!(status.validity_flag(), ValidityFlag::NormalOperation)
    {
        info!("ENS160 is in normal operation - putting to idle mode");
    } else {
        info!("ENS160 is not in normal operation - cannot put to idle mode");
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
async fn wake_ens160(
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    int: &mut Input<'static>,
) -> Result<(), &'static str> {
    info!("Waking up ENS160 sensor");
    ens160
        .set_operation_mode(OperationMode::Standard)
        .await
        .map_err(|_| "Failed to set ENS160 to Standard mode")?;

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

/// Check if ENS160 is calibrated and ready for idle/wake cycles
/// The ENS160 sensor requires a >24-hour calibration period after initial startup.
/// During this period, the sensor must remain in continuous operation. Only after this initial
/// calibration is the sensor ready for idle/wake power management cycles.
async fn is_ens160_calibrated(
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
) -> Result<bool, &'static str> {
    let calibration_state = {
        let state = SYSTEM_STATE.lock().await;
        state.get_ens160_calibration_state()
    };

    // First check if already marked as calibrated
    if calibration_state.is_calibrated {
        return Ok(true);
    }

    // If we have a calibration start time, check if 25 hours have elapsed
    if let Some(start_time) = calibration_state.calibration_start_time {
        let elapsed = Instant::now().as_secs() - start_time;
        // If 25 hours have passed since calibration started, mark as calibrated
        if elapsed >= INITIAL_CALIBRATION_TIME {
            {
                let mut state = SYSTEM_STATE.lock().await;
                state.mark_ens160_calibrated();
            }
            info!("ENS160 calibration period completed ({} hours)", elapsed / 3600);
            return Ok(true);
        }
        info!(
            "ENS160 calibration in progress: {}/{} hours",
            elapsed / 3600,
            INITIAL_CALIBRATION_TIME / 3600
        );
        return Ok(false);
    }

    // Only now check sensor status
    let status = ens160.get_status().await.map_err(|_| "Failed to get ENS160 status")?;

    // If sensor reports InitialStartupPhase, we need calibration
    if matches!(status.validity_flag(), ValidityFlag::InitialStartupPhase) {
        // Start calibration tracking
        {
            let mut state = SYSTEM_STATE.lock().await;
            state.start_ens160_calibration(Instant::now().as_secs());
        }
        info!("ENS160 is in InitialStartupPhase - starting 25-hour calibration period");
        return Ok(false); // Not ready for idle cycles
    }

    // Default: assume we need calibration
    {
        let mut state = SYSTEM_STATE.lock().await;
        state.start_ens160_calibration(Instant::now().as_secs());
    }
    info!("ENS160 calibration state uncertain - starting calibration period");
    Ok(false)
}

/// Set temperature and humidity compensation on ENS160 sensor
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
    ens160_int: &mut Input<'static>,
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

    // Run the wake sequence for ENS160 sensor once at startup
    if let Err(e) = wake_ens160(&mut ens160, ens160_int).await {
        info!("ENS160 wake failed: {}", e);
        return Err("ENS160 wake failed");
    }

    Ok((aht21, ens160))
}

/// Execute one iteration of the sensor reading loop
async fn handle_sensor_iteration(
    aht21: &mut Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    ens160_int: &mut Input<'static>,
    prev_temp: &mut f32,
    prev_humidity: &mut f32,
    ready_for_idle_cycles: bool,
) -> bool {
    // Read AHT21 data, the ens160 should be as cool as it gets now so at this point we get the most accurate temperature and humidity
    let aht21_result = read_aht21(aht21).await;
    if let Ok(ref aht21_readings) = aht21_result {
        *prev_temp = aht21_readings.temperature;
        *prev_humidity = aht21_readings.humidity;
    }

    // Wake up ENS160 sensor, if it is ready for idle/wake cycles
    if ready_for_idle_cycles {
        if let Err(e) = wake_ens160(ens160, ens160_int).await {
            info!("ENS160 wake failed: {}", e);
            return false; // Indicate failure
        }
    }

    Timer::after_secs(WARMUP_TIME).await;

    // Set temperature and humidity compensation
    if let Err(e) = set_ens160_compensation(ens160, *prev_temp, *prev_humidity).await {
        info!("ENS160 compensation setting failed: {}", e);
        return false; // Indicate failure
    }

    let ens160_result = read_ens160(ens160, ens160_int).await;

    // Send ENS160 to idle mode if it is ready for idle/wake cycles
    if ready_for_idle_cycles {
        // Put ENS160 to sleep for power conservation immediately after reading
        if let Err(e) = idle_ens160(ens160).await {
            info!("ENS160 sleep failed: {}", e);
            return false; // Indicate failure
        }
    }

    // Process readings
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
    let mut prev_temp = 25.0; // Default temperature
    let mut prev_humidity = 50.0; // Default humidity

    info!("Sensor task initialized successfully");
    report_task_success(task_id).await;

    loop {
        // Check calibration status first
        let ready_for_idle_cycles = match is_ens160_calibrated(&mut ens160).await {
            Ok(ready) => ready,
            Err(e) => {
                info!("Failed to check calibration status: {}", e);
                report_task_failure(task_id).await;
                Timer::after_secs(CALIBRATION_READ_INTERVAL).await;
                continue;
            }
        };

        // Execute one iteration of the sensor reading loop
        let success = handle_sensor_iteration(
            &mut aht21,
            &mut ens160,
            &mut ens160_int,
            &mut prev_temp,
            &mut prev_humidity,
            ready_for_idle_cycles,
        )
        .await;

        if success {
            report_task_success(task_id).await;
        } else {
            report_task_failure(task_id).await;
        }

        Timer::after_secs(IDLE_TIME).await;
    }
}
