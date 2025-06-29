//! VSYS voltage measurement task

use defmt::{error, info};
use embassy_rp::{
    Peri,
    adc::{Adc, Async, Channel, Config, Error},
    gpio::Pull,
    peripherals::{ADC, PIN_29},
};
use embassy_time::{Duration, Timer, with_timeout};
use moving_median::MovingMedian;

use crate::{
    Irqs,
    event::{Event, send_event},
    watchdog::{TaskId, report_task_failure, report_task_success},
};

/// Interval for periodic voltage measurements
static INTERVAL: Duration = Duration::from_secs(4);

/// Voltage threshold for determining charging state (above this = charging)
const CHARGING_VOLTAGE_THRESHOLD: f32 = 4.4;

/// Median window size for voltage measurements when on battery power
const MEDIAN_WINDOW_SIZE: usize = 5;

/// Vsys voltage offset - calibrated by measuring actual voltage supplied as opposed to what we can measure on the VSYS pin
/// For whatever reason the waveshare boards have a considerably lower voltage on the VSYS pin than what is actually supplied,
/// this is true for powering from USB or battery both.
const VSYS_VOLTAGE_OFFSET: f32 = 0.27;

#[embassy_executor::task]
pub async fn vsys_voltage_task(mut p_adc: Peri<'static, ADC>, mut p_pin29: Peri<'static, PIN_29>) {
    let mut voltage_median = MovingMedian::<f32, MEDIAN_WINDOW_SIZE>::new();

    // Track previous states to only send events on changes
    let mut prev_charging_state: Option<bool> = None;
    let mut prev_battery_percentage: Option<u8> = None;

    info!("VSYS voltage task initialized successfully");

    loop {
        // Wait for periodic measurement trigger
        Timer::after(INTERVAL).await;

        let adc_peri = p_adc.reborrow();
        let pin_peri = p_pin29.reborrow();

        '_adc: {
            // Initialize ADC and channel for this measurement session
            let mut adc = Adc::new(adc_peri, Irqs, Config::default());
            let mut channel = Channel::new_pin(pin_peri, Pull::None);
            Timer::after_millis(100).await; // small delay to ensure ADC is ready

            match read_voltage(&mut adc, &mut channel).await {
                Ok(voltage) => {
                    // Determine charging state based on VSYS voltage
                    let is_charging = voltage > CHARGING_VOLTAGE_THRESHOLD;

                    let final_voltage = if is_charging {
                        // When charging/external power, use direct measurement (no median filtering)
                        voltage
                    } else {
                        // When on battery power, use moving median of 5 measurements
                        voltage_median.add_value(voltage);
                        voltage_median.median()
                    };

                    let battery_percentage = voltage_to_percentage(final_voltage);

                    // Send events only when states change
                    let charging_state_changed = prev_charging_state != Some(is_charging);
                    let battery_level_changed = !is_charging && prev_battery_percentage != Some(battery_percentage);

                    // Handle charging state changes
                    if charging_state_changed {
                        if is_charging {
                            send_event(Event::BatteryCharging).await;
                            info!("State change: Now charging ({}V)", final_voltage);
                        } else {
                            send_event(Event::BatteryLevel(battery_percentage)).await;
                            info!(
                                "State change: Now on battery ({}V, {}%)",
                                final_voltage, battery_percentage
                            );
                        }
                        prev_charging_state = Some(is_charging);
                    }
                    // Handle battery level changes (only when not charging and no charging state change)
                    else if battery_level_changed {
                        send_event(Event::BatteryLevel(battery_percentage)).await;
                        info!("Battery level change: {}% ({}V)", battery_percentage, final_voltage);
                    }

                    // Update previous battery percentage when on battery
                    if !is_charging {
                        prev_battery_percentage = Some(battery_percentage);
                    }

                    // Report task success for watchdog health monitoring
                    report_task_success(TaskId::Vsys).await;
                }
                Err(e) => {
                    error!("Could not read voltage: {}", e);
                    // Report task failure for watchdog health monitoring
                    report_task_failure(TaskId::Vsys).await;
                    info!("VSYS task: failed iteration, reporting failure to watchdog");
                }
            }
        }
    }
}

/// Reads ADC value and converts it to voltage
async fn read_voltage(adc: &mut Adc<'_, Async>, channel: &mut Channel<'_>) -> Result<f32, Error> {
    match with_timeout(Duration::from_millis(200), adc.read(channel)).await {
        Ok(Ok(adc_value)) => {
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
    const VOLTAGE_DIVIDER: f32 = 3.0;
    const ADC_MAX_VALUE: f32 = 4096.0; // 12-bit ADC
    f32::from(adc_value) * VOLTAGE_DIVIDER * (ADC_REF_VOLTAGE / ADC_MAX_VALUE) + VSYS_VOLTAGE_OFFSET
}

/// Converts voltage to battery percentage
fn voltage_to_percentage(voltage: f32) -> u8 {
    const MIN_VOLTAGE: f32 = 3.0; // 0% battery
    const MAX_VOLTAGE: f32 = 4.1; // 100% battery

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
