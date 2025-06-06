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

use crate::event::{Event, send_event};

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

struct SensorReadings {
    temperature: f32,
    humidity: f32,
    co2: f32,
    etoh: f32,
    air_quality: AirQualityIndex,
    ens160_validity: ValidityFlag,
}

async fn read_sensor_data(
    aht21: &mut Aht20<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    ens160: &mut Ens160<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>, Delay>,
    eco2_median: &mut MovingMedian<f32, 5>,
    etoh_median: &mut MovingMedian<f32, 5>,
) -> Result<SensorReadings, &'static str> {
    let (hum, temp) = aht21.read().await.map_err(|_| "Failed to read AHT21 sensor")?;
    let (temp, rh) = (temp.celsius(), hum.rh());

    ens160.initialize().await.map_err(|_| "Failed to initialize ENS160")?;

    let status = ens160.get_status().await.map_err(|_| "Failed to get ENS160 status")?;
    info!("ENS160 status: {}", Debug2Format(&status));

    ens160
        .set_temp_rh_comp(temp, rh as u16)
        .await
        .map_err(|_| "Failed to set temperature and humidity compensation")?;
    Timer::after_millis(100).await;

    let eco2 = ens160.get_eco2().await.map_err(|_| "Failed to get eCO2")?;
    eco2_median.add_value(eco2.get_value() as f32);

    let etoh = ens160.get_etoh().await.map_err(|_| "Failed to get ethanol")?;
    etoh_median.add_value(etoh as f32);

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

#[embassy_executor::task]
pub async fn sensor_task(
    aht21: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    ens160: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    _ens160_int: Input<'static>,
) {
    // Initialize AHT21 and calibrate it
    let mut aht21 = match initialize_aht21(aht21).await {
        Some(sensor) => sensor,
        None => {
            info!("Failed to initialize AHT21");
            return;
        }
    };

    // Initialize ENS160
    let mut ens160 = Ens160::new(ens160, Delay);

    // define moving median for eCO2 and Ethanol
    let mut eco2_median = MovingMedian::<f32, 5>::new();
    let mut etoh_median = MovingMedian::<f32, 5>::new();

    // In the loop, we read the AHT20 sensor and then use the ENS160 to get air quality data.
    loop {
        match read_sensor_data(&mut aht21, &mut ens160, &mut eco2_median, &mut etoh_median).await {
            Ok(readings) => {
                send_event(Event::SensorData {
                    temperature: readings.temperature,
                    humidity: readings.humidity,
                    co2: readings.co2 as u16,
                    etoh: readings.etoh as u16,
                    air_quality: readings.air_quality,
                    ens160_validity: readings.ens160_validity,
                })
                .await;
            }
            Err(e) => {
                info!("Sensor reading failed: {}", e);
            }
        }

        Timer::after_secs(60).await;
    }
}
