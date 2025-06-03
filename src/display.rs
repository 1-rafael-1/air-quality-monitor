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

#[embassy_executor::task]
pub async fn display_task(ssd1306: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>) {}
