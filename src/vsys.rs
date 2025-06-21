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
};

/// Interval for periodic voltage measurements
static INTERVAL: Duration = Duration::from_secs(4);

/// Voltage threshold for determining charging state (above this = charging)
const CHARGING_VOLTAGE_THRESHOLD: f32 = 4.4;

/// Median window size for voltage measurements when on battery power
const MEDIAN_WINDOW_SIZE: usize = 5;

#[embassy_executor::task]
pub async fn vsys_voltage_task(mut p_adc: Peri<'static, ADC>, mut p_pin29: Peri<'static, PIN_29>) {
    let mut voltage_median = MovingMedian::<f32, MEDIAN_WINDOW_SIZE>::new();
    info!("VSYS voltage task initialized successfully");

    loop {
        // Wait for periodic measurement trigger
        Timer::after(INTERVAL).await;
        info!("VSYS periodic measurement triggered");

        let adc_peri = p_adc.reborrow();
        let pin_peri = p_pin29.reborrow();

        '_adc: {
            // Initialize ADC and channel for this measurement session
            let mut adc = Adc::new(adc_peri, Irqs, Config::default());
            let mut channel = Channel::new_pin(pin_peri, Pull::None);
            Timer::after_millis(100).await; // small delay to ensure ADC is ready

            match read_voltage(&mut adc, &mut channel).await {
                Ok(voltage) => {
                    info!("VSYS voltage measurement: {}V", voltage);

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

                    info!("VSYS final voltage: {}V, charging: {}", final_voltage, is_charging);

                    // Send battery level and charging state events
                    if is_charging {
                        send_event(Event::BatteryCharging).await;
                    } else {
                        send_event(Event::BatteryLevel(battery_percentage)).await;
                    }

                    // // Send battery level always
                    // send_event(Event::BatteryLevel(battery_percentage)).await;

                    // // Send charging state only if it changed
                    // if is_charging != last_charging_state {
                    //     send_event(Event::BatteryCharging(is_charging)).await;
                    //     last_charging_state = is_charging;
                    // }
                }
                Err(e) => {
                    error!("Could not read voltage: {}", e);
                }
            }
        }
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
    const VOLTAGE_DIVIDER: f32 = 3.0;
    const ADC_MAX_VALUE: f32 = 4096.0; // 12-bit ADC
    f32::from(adc_value) * VOLTAGE_DIVIDER * (ADC_REF_VOLTAGE / ADC_MAX_VALUE)
}

/// Converts voltage to battery percentage
fn voltage_to_percentage(voltage: f32) -> u8 {
    const MIN_VOLTAGE: f32 = 3.0; // 0% battery
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
