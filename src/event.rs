use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use ens160_aq::data::{AirQualityIndex, ValidityFlag};

const EVENT_CHANNEL_CAPACITY: usize = 10;
pub static EVENT_CHANNEL: Channel<CriticalSectionRawMutex, Event, EVENT_CHANNEL_CAPACITY> = Channel::new();

/// Sends an event to the system channel
pub async fn send_event(event: Event) {
    EVENT_CHANNEL.sender().send(event).await;
}

/// Receives the next event from the system channel
pub async fn receive_event() -> Event {
    EVENT_CHANNEL.receiver().receive().await
}

#[derive(Debug, Clone, Copy)]
pub enum Event {
    /// Sensor data event containing readings from the sensors
    SensorData {
        temperature: f32,
        humidity: f32,
        co2: u16,
        etoh: u16,
        air_quality: AirQualityIndex,
        ens160_validity: ValidityFlag,
    },
    /// Battery charging state event (true = charging, false = not charging)
    BatteryCharging(bool),
    /// Battery level event (0-100 percentage)
    BatteryLevel(u8),
}
