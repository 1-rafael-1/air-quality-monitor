//! RP2350 based air quality monitor firmware
//!
//! Uses ens160 for air quality, AHT21 for temperature and humidity, and SSD1306 for display.

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_rp::{
    adc::InterruptHandler as AdcInterruptHandler,
    bind_interrupts,
    block::ImageDef,
    clocks::{ClockConfig, CoreVoltage},
    config::Config,
    gpio::{Input, Pull},
    i2c::{Async, Config as I2cConfig, I2c, InterruptHandler},
    peripherals::I2C0,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use panic_probe as _;
use static_cell::StaticCell;

mod display;
mod event;
mod orchestrate;
mod sensor;
mod vbus;
mod vsys;
mod watchdog;

// Firmware image type for bootloader
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: ImageDef = ImageDef::secure_exe();

bind_interrupts!(struct Irqs {
        I2C0_IRQ => InterruptHandler<I2C0>;
        ADC_IRQ_FIFO => AdcInterruptHandler;
    }
);

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    #[allow(clippy::unwrap_used)]
    // Reduce the clock speed to conserve power
    let mut config = Config::new(ClockConfig::system_freq(18_000_000).unwrap());
    config.clocks.core_voltage = CoreVoltage::V0_90;
    let p = embassy_rp::init(config);

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
    let ens160_int = Input::new(p.PIN_18, Pull::Up);

    // Initialize VBUS monitoring
    let vbus = Input::new(p.PIN_24, Pull::None);

    #[allow(clippy::unwrap_used)]
    spawner
        .spawn(sensor::sensor_task(i2c_device_aht21, i2c_device_ens160, ens160_int))
        .unwrap();
    #[allow(clippy::unwrap_used)]
    spawner.spawn(display::display_task(i2c_device_ssd1306)).unwrap();
    #[allow(clippy::unwrap_used)]
    spawner.spawn(display::mode_switch_task()).unwrap();
    #[allow(clippy::unwrap_used)]
    spawner.spawn(watchdog::watchdog_task(p.WATCHDOG)).unwrap();
    #[allow(clippy::unwrap_used)]
    spawner.spawn(orchestrate::orchestrate_task()).unwrap();
    #[allow(clippy::unwrap_used)]
    spawner.spawn(vbus::vbus_monitor_task(vbus)).unwrap();
    #[allow(clippy::unwrap_used)]
    spawner.spawn(vsys::vsys_voltage_task(p.ADC, p.PIN_29)).unwrap();
}
