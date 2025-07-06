//! Events and system channel for sending and receiving events

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use ens160_aq::data::AirQualityIndex;

/// System event channel for sending and receiving events
pub static EVENT_CHANNEL: Channel<CriticalSectionRawMutex, Event, EVENT_CHANNEL_CAPACITY> = Channel::new();
/// The capacity of the event channel
const EVENT_CHANNEL_CAPACITY: usize = 10;

/// Sends an event to the system channel
pub async fn send_event(event: Event) {
    EVENT_CHANNEL.sender().send(event).await;
}

/// Receives the next event from the system channel
pub async fn receive_event() -> Event {
    EVENT_CHANNEL.receiver().receive().await
}

/// The event type used in the system, representing various system events
#[derive(Debug, Clone, Copy)]
pub enum Event {
    /// Sensor data event containing readings from the sensors
    SensorData {
        /// Temperature in degrees Celsius (display value with offset)
        temperature: f32,
        /// Raw temperature in degrees Celsius (without offset)
        raw_temperature: f32,
        /// Humidity in percentage (calibrated)
        humidity: f32,
        /// Raw humidity in percentage (uncalibrated)
        raw_humidity: f32,
        /// CO2 level in ppm
        co2: u16,
        /// TVOC level in ppb
        etoh: u16,
        /// Air quality index data
        air_quality: AirQualityIndex,
    },
    /// Battery charging state event (true = charging, false = not charging)
    BatteryCharging,
    /// Battery level event (0-100 percentage)
    BatteryLevel(u8),
    /// Display mode toggle request
    ToggleDisplayMode,
}
