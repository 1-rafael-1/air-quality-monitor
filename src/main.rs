//! # Firmware Entry Point

#![no_std]
#![no_main]

use defmt::info;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_rp::{bind_interrupts, block::ImageDef, config::Config};
use panic_probe as _;

// Firmware image type for bootloader
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: ImageDef = ImageDef::secure_exe();

bind_interrupts!(
    pub struct Irqs {
        // UART0_IRQ => BufferedInterruptHandler<UART0>;
        // PIO0_IRQ_0 => InterruptHandler<PIO0>;
        // PIO1_IRQ_0 => InterruptHandler<PIO1>;
    }
);

/// Firmware entry point
#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Initialize system: peripherals
    let p = embassy_rp::init(Config::default());
    info!("System initialized");
}
