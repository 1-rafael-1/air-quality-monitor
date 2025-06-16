//! The main orchestrator task for the system

use crate::{
    display::{DisplayCommand, send_display_command},
    event::{Event, receive_event},
    vsys::{VsysCommand, send_vsys_command},
};

/// Main coordination task that implements the system's event loop
#[embassy_executor::task]
pub async fn orchestrate_task() {
    loop {
        let event = receive_event().await;
        process_event(event).await;
    }
}

/// Processes the received event and sends appropriate commands to other components
async fn process_event(event: Event) {
    match event {
        Event::SensorData {
            temperature,
            humidity,
            co2,
            etoh,
            air_quality,
        } => {
            send_display_command(DisplayCommand::SensorData {
                temperature,
                humidity,
                co2,
                etoh,
                air_quality,
            })
            .await;
        }
        Event::BatteryCharging(is_charging) => {
            send_display_command(DisplayCommand::BatteryCharging(is_charging)).await;
            if !is_charging {
                send_vsys_command(VsysCommand::MakeMeasurement);
            }
        }
        Event::BatteryLevel(level) => {
            send_display_command(DisplayCommand::UpdateBatteryPercentage(level)).await;
        }
    }
}
