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
    peripherals::I2C0,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Delay, Timer};
use ens160_aq::{
    Ens160,
    data::{InterruptPinConfig, OperationMode, ValidityFlag},
};
use panic_probe as _;
use static_cell::StaticCell;

mod display;
mod sensor;
mod watchdog;

// Firmware image type for bootloader
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: ImageDef = ImageDef::secure_exe();

bind_interrupts!(struct Irqs {
        I2C0_IRQ => InterruptHandler<I2C0>;
    }
);

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Config::default());

    // I2C setup
    let sda = p.PIN_16;
    let scl = p.PIN_17;
    let i2c0 = p.I2C0;
    let i2c = I2c::new_async(i2c0, scl, sda, Irqs, I2cConfig::default());
    static I2C_BUS: StaticCell<Mutex<NoopRawMutex, I2c<'static, I2C0, Async>>> = StaticCell::new();
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));

    // Initialize the I2C devices
    let i2c_device_aht21 = I2cDevice::new(i2c_bus);
    let i2c_device_ens160 = I2cDevice::new(i2c_bus);
    let i2c_device_ssd1306 = I2cDevice::new(i2c_bus);

    // Initialize the interrupt pin for ENS160
    let mut ens160_int = Input::new(p.PIN_18, Pull::Up);

    spawner
        .spawn(sensor::sensor_task(i2c_device_aht21, i2c_device_ens160, ens160_int))
        .unwrap();
    spawner.spawn(display::display_task(i2c_device_ssd1306)).unwrap();
}
