//! VBUS monitoring task
use defmt::info;
use embassy_rp::gpio::Input;
use embassy_time::Timer;

use crate::event::{Event, send_event};

#[embassy_executor::task]
pub async fn vbus_monitor_task(mut vbus: Input<'static>) {
    Timer::after_millis(100).await; // Initial debounce delay
    info!("VBUS monitor task initialized successfully");

    loop {
        let is_charging = vbus.is_high();
        send_event(Event::BatteryCharging(is_charging)).await;

        vbus.wait_for_rising_edge().await;
        // Small delay to debounce
        Timer::after_millis(200).await;
    }
}
