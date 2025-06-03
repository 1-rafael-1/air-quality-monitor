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
    gpio::{Input, Pull},
    i2c::{Async, Config as I2cConfig, I2c, InterruptHandler},
    peripherals::{I2C0, PIN_18},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Delay, Timer};
use ens160_aq::{
    Ens160,
    data::{InterruptPinConfig, OperationMode, ValidityFlag},
};
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

    // I2C setup
    let sda = p.PIN_16;
    let scl = p.PIN_17;
    let i2c0 = p.I2C0;
    let i2c = I2c::new_async(i2c0, scl, sda, Irqs, I2cConfig::default());
    static I2C_BUS: StaticCell<Mutex<NoopRawMutex, I2c<'static, I2C0, Async>>> = StaticCell::new();
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));
    let i2c_device_aht21 = I2cDevice::new(i2c_bus);
    let i2c_device_ens160 = I2cDevice::new(i2c_bus);

    // Initialize AHT20 and calibrate it
    let mut aht21 = Aht20::new(i2c_device_aht21, Delay).await.unwrap();
    info!("calibrate aht21");
    let _ = aht21.calibrate().await.unwrap();
    Timer::after_millis(5000).await;
    info!("done calibrating");

    // Initialize ENS160
    let mut ens160 = Ens160::new(i2c_device_ens160, Delay);
    let mut ens160_int = Input::new(p.PIN_18, Pull::Up);

    // In the loop, we read the AHT20 sensor and then use the ENS160 to get air quality data.
    loop {
        let (temp, rh) = {
            let (hum, temp) = aht21.read().await.unwrap();
            info!("Temperature: {}°C, Humidity: {}%", temp.celsius(), hum.rh());
            (temp.celsius(), hum.rh())
        };

        {
            match ens160.initialize().await {
                Ok(_) => {}
                Err(e) => {
                    info!("Failed to initialize ENS160: {}", Debug2Format(&e));
                    return;
                }
            };
            // ens160.clear_command().await.unwrap();
            ens160.set_operation_mode(OperationMode::Standard).await.unwrap();

            let mut status = ens160.get_status().await.unwrap();
            while !matches!(status.validity_flag(), ValidityFlag::NormalOperation) {
                info!("ENS160 is not in normal operation mode, waiting...");
                Timer::after_millis(1000).await;
                status = ens160.get_status().await.unwrap();
                info!("ENS160 status: {}", Debug2Format(&status));
            }

            ens160.set_temp_rh_comp(temp, rh as u16).await.unwrap();

            info!("ENS160 status: {}", Debug2Format(&status));

            let config = InterruptPinConfig::builder()
                .active_low()
                .enable_interrupt()
                .on_new_data()
                // .on_new_group_data()
                .build();

            info!("Configuring ENS160 interrupt pin: {}", config);

            ens160.config_interrupt_pin(config).await.unwrap();

            ens160.config_interrupt_pin(0x23).await.unwrap();
            Timer::after_millis(20).await;

            let status = ens160.get_status().await.unwrap();
            info!("ENS160 status: {}", Debug2Format(&status));

            // wait for ENS160 to have data ready
            info!("is low: {}", ens160_int.is_low());
            ens160_int.wait_for_low().await;
            info!("is low: {}", ens160_int.is_low());

            let status = ens160.get_status().await.unwrap();
            info!("ENS160 status: {}", Debug2Format(&status));

            // Timer::after_millis(1000).await;

            let eco2 = ens160.get_eco2().await.unwrap();
            let aq = ens160.get_airquality_index().await.unwrap();
            let h = ens160.get_etoh().await.unwrap();
            info!(
                "Air Quality Index: {}, eCO2: {} ppm, Ethanol: {} ppb",
                Debug2Format(&aq),
                eco2.get_value(),
                h,
            );
            ens160.set_operation_mode(OperationMode::Sleep).await.unwrap();
        } // drop ens160, but keep i2c_bus

        Timer::after_secs(10).await;
    }
}
