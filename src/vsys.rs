//! VSYS voltage measurement task

use defmt::{Format, info};
use embassy_futures::select::{Either, select};
use embassy_rp::adc::{Adc, Async, Channel};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Timer};
use moving_median::MovingMedian;

use crate::event::{Event, send_event};

/// Signal for triggering state updates
pub static VSYS: Signal<CriticalSectionRawMutex, VsysCommand> = Signal::new();

/// Triggers a display update with the provided command
pub fn send_vsys_command(command: VsysCommand) {
    VSYS.signal(command);
}

/// Waits for next indicator state change signal
async fn wait_for_vsys_command() -> VsysCommand {
    VSYS.wait().await
}

/// Command to trigger a voltage measurement
#[derive(PartialEq, Eq, Format)]
pub enum VsysCommand {
    /// Trigger a voltage measurement
    MakeMeasurement,
}

#[embassy_executor::task]
pub async fn vsys_voltage_task(mut adc: Adc<'static, Async>, mut channel: Channel<'static>) {
    let mut voltage_median = MovingMedian::<f32, 5>::new();

    info!("VSYS voltage task initialized successfully");

    loop {
        match select(wait_for_vsys_command(), Timer::after(Duration::from_secs(300))).await {
            Either::First(command) => {
                info!("VSYS command received: {}", command);
                if command == VsysCommand::MakeMeasurement {
                    // trigger 5 measurements
                    for _ in 0..5 {
                        let voltage = read_voltage(&mut adc, &mut channel).await;
                        info!("VSYS voltage measurement: {}V", voltage);
                        voltage_median.add_value(voltage);
                        Timer::after(Duration::from_millis(20)).await; // small delay between measurements
                    }
                }
            }
            Either::Second(()) => {
                info!("VSYS periodic measurement triggered");
                let voltage = read_voltage(&mut adc, &mut channel).await;
                voltage_median.add_value(voltage);
            }
        }

        let battery_percentage = voltage_to_percentage(voltage_median.median());
        send_event(Event::BatteryLevel(battery_percentage)).await;
    }
}

/// Reads ADC value and converts it to voltage
async fn read_voltage(adc: &mut Adc<'_, Async>, channel: &mut Channel<'_>) -> f32 {
    let adc_value = f32::from(adc.read(channel).await.unwrap_or_default());
    adc_value * 3.3 * 3.0 / 4096.0
}

/// Converts voltage to battery percentage
fn voltage_to_percentage(voltage: f32) -> u8 {
    const MIN_VOLTAGE: f32 = 2.8; // 0% battery
    const MAX_VOLTAGE: f32 = 4.0; // 100% battery

    let percentage = if voltage >= MAX_VOLTAGE {
        100.0
    } else if voltage <= MIN_VOLTAGE {
        0.0
    } else {
        ((voltage - MIN_VOLTAGE) / (MAX_VOLTAGE - MIN_VOLTAGE)) * 100.0
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percentage_u8 = percentage as u8;
    percentage_u8
}
