//! The main orchestrator task for the system

use crate::{
    display::{DisplayCommand, send_display_command},
    event::{Event, receive_event},
    system_state::{SYSTEM_STATE, SensorData},
    watchdog::{TaskId, report_task_success},
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
            raw_temperature,
            humidity,
            raw_humidity,
            co2,
            etoh,
            air_quality,
        } => {
            // Create sensor data structure
            let sensor_data = SensorData {
                temperature,
                raw_temperature,
                humidity,
                raw_humidity,
                co2,
                etoh,
                air_quality,
            };

            // Update system state with new sensor data and CO2 history
            {
                let mut state = SYSTEM_STATE.lock().await;
                state.add_co2_measurement(co2);
                state.set_last_sensor_data(sensor_data);
            }

            // Send display command
            send_display_command(DisplayCommand::SensorData {
                temperature,
                raw_temperature,
                humidity,
                raw_humidity,
                co2,
                etoh,
                air_quality,
            })
            .await;
        }
        Event::BatteryCharging => {
            // Update system state
            {
                let mut state = SYSTEM_STATE.lock().await;
                state.set_charging(true);
            }

            send_display_command(DisplayCommand::UpdateBatteryCharging).await;
        }
        Event::BatteryLevel(level) => {
            // Update system state
            {
                let mut state = SYSTEM_STATE.lock().await;
                state.set_charging(false);
                state.set_battery_percent(level);
            }

            send_display_command(DisplayCommand::UpdateBatteryPercentage(level)).await;
        }
        Event::ToggleDisplayMode => {
            // Check if we have sensor data and toggle mode if we do
            let should_toggle_and_data = {
                let mut state = SYSTEM_STATE.lock().await;
                if state.last_sensor_data.is_some() {
                    state.toggle_display_mode();
                    (true, state.last_sensor_data.clone())
                } else {
                    (false, None)
                }
            };

            if should_toggle_and_data.0 {
                send_display_command(DisplayCommand::ToggleMode).await;
            }
        }
    }
    report_task_success(TaskId::Orchestrator).await;
}
