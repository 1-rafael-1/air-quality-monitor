//! VSYS voltage measurement task

use defmt::{Format, error, info};
use embassy_futures::select::{Either, select};
use embassy_rp::adc::{Adc, Async, Channel, Error};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Timer, with_timeout};
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

/// Interval for periodic voltage measurements
static INTERVAL: Duration = Duration::from_secs(300);

/// Median window size for voltage measurements
const MEDIAN_WINDOW_SIZE: usize = 3;

/// Command to trigger a voltage measurement
#[derive(PartialEq, Eq, Format)]
pub enum VsysCommand {
    /// Trigger a voltage measurement
    MakeMeasurement,
}

#[embassy_executor::task]
pub async fn vsys_voltage_task(mut adc: Adc<'static, Async>, mut channel: Channel<'static>) {
    let mut voltage_median = MovingMedian::<f32, MEDIAN_WINDOW_SIZE>::new();
    info!("VSYS voltage task initialized successfully");

    loop {
        let meas_target_cnt = match select(wait_for_vsys_command(), Timer::after(INTERVAL)).await {
            Either::First(command) => {
                info!("VSYS command received: {}", command);
                MEDIAN_WINDOW_SIZE
            }
            Either::Second(()) => {
                info!("VSYS periodic measurement triggered");
                1
            }
        };

        let mut meas_is_cnt: usize = 0;
        while meas_is_cnt < meas_target_cnt {
            match read_voltage(&mut adc, &mut channel).await {
                Ok(value) => {
                    info!("VSYS voltage measurement: {}V", value);
                    voltage_median.add_value(value);
                    meas_is_cnt += 1;
                }
                Err(e) => {
                    error!("Could not read voltage: {}", e);
                }
            }
            Timer::after(Duration::from_millis(20)).await; // small delay between measurements
        }
        let battery_percentage = voltage_to_percentage(voltage_median.median());
        send_event(Event::BatteryLevel(battery_percentage)).await;
    }
}

/// Reads ADC value and converts it to voltage
async fn read_voltage(adc: &mut Adc<'_, Async>, channel: &mut Channel<'_>) -> Result<f32, Error> {
    match with_timeout(Duration::from_millis(200), adc.read(channel)).await {
        Ok(Ok(adc_value)) => {
            info!("ADC value: {}", adc_value);
            if adc_value == 0 {
                error!("ADC value is zero, indicating a possible read error");
                return Err(Error::ConversionFailed);
            }
            Ok(adc_value_to_voltage(adc_value))
        }
        Ok(Err(e)) => {
            error!("ADC read error: {}", e);
            Err(e)
        }
        Err(_) => {
            error!("ADC read timeout");
            Err(Error::ConversionFailed)
        }
    }
}

/// Converts ADC value to voltage
fn adc_value_to_voltage(adc_value: u16) -> f32 {
    // Convert ADC value to voltage (assuming 3.3V reference and 12-bit resolution)
    const ADC_REF_VOLTAGE: f32 = 3.3;
    const VOLTAGED_DIVIDER: f32 = 3.0;
    const ADC_MAX_VALUE: f32 = 4096.0; // 12-bit ADC
    f32::from(adc_value) * VOLTAGED_DIVIDER * (ADC_REF_VOLTAGE / ADC_MAX_VALUE)
}

/// Converts voltage to battery percentage
fn voltage_to_percentage(voltage: f32) -> u8 {
    const MIN_VOLTAGE: f32 = 2.8; // 0% battery
    const MAX_VOLTAGE: f32 = 4.2; // 100% battery

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
