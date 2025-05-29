#![no_std]
#![no_main]

use aht20_async::Aht20;
use defmt::{Debug2Format, info};
use defmt_rtt as _;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_rp::{
    bind_interrupts,
    block::ImageDef,
    config::Config,
    i2c::{Async, Config as I2cConfig, I2c, InterruptHandler},
    peripherals::I2C0,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Delay, Timer};
use ens160_aq::Ens160;
use panic_probe as _;
use static_cell::StaticCell;

// Firmware image type for bootloader
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: ImageDef = ImageDef::secure_exe();

bind_interrupts!(struct Irqs {
        I2C0_IRQ => InterruptHandler<I2C0>;
    }
);

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Config::default());

    // we init I2C permanently
    let sda = p.PIN_16;
    let scl = p.PIN_17;
    let i2c0 = p.I2C0;
    let i2c = I2c::new_async(i2c0, scl, sda, Irqs, I2cConfig::default());
    static I2C_BUS: StaticCell<Mutex<NoopRawMutex, I2c<'static, I2C0, Async>>> = StaticCell::new();
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));

    // Initialize AHT20 and calibrate it
    {
        let i2c_device = I2cDevice::new(i2c_bus);
        let delay = Delay;
        let mut aht21 = Aht20::new(i2c_device, delay).await.unwrap();
        info!("calibrate aht21");
        let _ = aht21.calibrate().await.unwrap();
        Timer::after_millis(5000).await;
        info!("done calibrating");
    }

    // In the loop, we read the AHT20 sensor and then use the ENS160 to get air quality data.
    loop {
        let (temp, rh) = {
            let i2c_device = I2cDevice::new(i2c_bus);

            let delay = Delay;
            let mut aht21 = Aht20::new(i2c_device, delay).await.unwrap();

            let (hum, temp) = aht21.read().await.unwrap();
            // let _ = aht21.calibrate().await.unwrap();
            info!("Temperature: {}°C, Humidity: {}%", temp.celsius(), hum.rh());
            (temp.celsius(), hum.rh())
        }; // drop aht21, but keep i2c_bus

        {
            let i2c_device = I2cDevice::new(i2c_bus);
            let delay = Delay;
            let mut ens160 = Ens160::new(i2c_device, delay);
            Timer::after_millis(100).await;
            match ens160.initialize().await {
                Ok(_) => {}
                Err(e) => {
                    info!("Failed to initialize ENS160: {}", Debug2Format(&e));
                    return;
                }
            }
            ens160.set_temp_rh_comp(temp, rh as u16).await.unwrap();
            let eco2 = ens160.get_eco2().await.unwrap();
            let aq = ens160.get_airquality_index().await.unwrap();
            let h = ens160.get_etoh().await.unwrap();
            info!(
                "Air Quality Index: {}, eCO2: {} ppm, Ethanol: {} ppb",
                Debug2Format(&aq),
                eco2.get_value(),
                h,
            );
        } // drop ens160, but keep i2c_bus

        Timer::after_secs(10).await;
    }
}
