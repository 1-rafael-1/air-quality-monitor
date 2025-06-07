//! Sensor task for reading data from AHT21 and ENS160 sensors.
use aht20_async::Aht20;
use defmt::{Debug2Format, info};
use defmt_rtt as _;
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
    data::{AirQualityIndex, ValidityFlag},
};
use moving_median::MovingMedian;
use panic_probe as _;

use crate::{
    event::{Event, send_event},
    watchdog::trigger_watchdog_reset,
};

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

/// Struct to hold sensor readings
struct SensorReadings {
    /// Temperature in degrees Celsius
    temperature: f32,
    /// Humidity in percentage
    humidity: f32,
    /// eCO2 level in ppm
    co2: f32,
    /// Ethanol (TVOC) level in ppb
    etoh: f32,
    /// Air quality index data
    air_quality: AirQualityIndex,
    /// ENS160 validity flag for CO2 and TVOC
    ens160_validity: ValidityFlag,
}

/// Read data from AHT21 and ENS160 sensors
async fn read_sensor_data(
    aht21: &mut Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    eco2_median: &mut MovingMedian<f32, 5>,
    etoh_median: &mut MovingMedian<f32, 5>,
) -> Result<SensorReadings, &'static str> {
    let (hum, temp) = aht21.read().await.map_err(|_| "Failed to read AHT21 sensor")?;
    let (temp, rh) = (temp.celsius(), hum.rh());

    let status = ens160.get_status().await.map_err(|_| "Failed to get ENS160 status")?;

    match status.validity_flag() {
        ValidityFlag::InitialStartupPhase => {
            info!("ENS160 still in InitialStartupPhase - sensor warming up");
        }
        other => {
            info!("ENS160 validity flag: {}", Debug2Format(&other));
        }
    }
    info!("ENS160 status: {}", Debug2Format(&status));

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    ens160
        .set_temp_rh_comp(temp, rh as u16)
        .await
        .map_err(|_| "Failed to set temperature and humidity compensation")?;
    Timer::after_millis(100).await;

    let eco2 = ens160.get_eco2().await.map_err(|_| "Failed to get eCO2")?;
    eco2_median.add_value(f32::from(eco2.get_value()));

    let etoh = ens160.get_etoh().await.map_err(|_| "Failed to get ethanol")?;
    etoh_median.add_value(f32::from(etoh));

    let aq = ens160
        .get_airquality_index()
        .await
        .map_err(|_| "Failed to get Air Quality Index")?;

    let readings = SensorReadings {
        temperature: temp,
        humidity: rh,
        co2: eco2_median.median(),
        etoh: etoh_median.median(),
        air_quality: aq,
        ens160_validity: status.validity_flag(),
    };

    info!(
        "Air Quality Index: {}, eCO2: {} ppm, Ethanol: {} ppb",
        Debug2Format(&aq),
        readings.co2,
        readings.etoh
    );

    Ok(readings)
}

/// Sensor task for reading data from AHT21 and ENS160 sensors.
#[embassy_executor::task]
pub async fn sensor_task(
    aht21: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    ens160: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    #[allow(clippy::used_underscore_binding)] _ens160_int: Input<'static>,
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

    let mut eco2_median = MovingMedian::<f32, 5>::new();
    let mut etoh_median = MovingMedian::<f32, 5>::new();

    info!("Sensor task initialized successfully");

    loop {
        match read_sensor_data(&mut aht21, &mut ens160, &mut eco2_median, &mut etoh_median).await {
            Ok(readings) => {
                send_event(Event::SensorData {
                    temperature: readings.temperature,
                    humidity: readings.humidity,
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    co2: readings.co2 as u16,
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    etoh: readings.etoh as u16,
                    air_quality: readings.air_quality,
                    ens160_validity: readings.ens160_validity,
                })
                .await;
            }
            Err(e) => {
                info!("Sensor reading failed (continuing): {}", e);
                // Continue in the loop - all errors in the loop are considered transient
            }
        }

        Timer::after_secs(60).await;
    }
}
