// use defmt_rtt as _;
use core::fmt::Write;

use defmt::{Debug2Format, error};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_rp::{
    i2c::{Async, I2c},
    peripherals::I2C0,
};
use embassy_sync::{
    blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex},
    signal::Signal,
};
use embedded_graphics::{
    image::Image,
    mono_font::{
        MonoTextStyle, MonoTextStyleBuilder,
        ascii::{FONT_6X13, FONT_8X13_BOLD},
    },
    pixelcolor::{BinaryColor, Gray8},
    prelude::*,
    text::{Baseline, Text},
};
use ens160_aq::data::{AirQualityIndex, ValidityFlag};
use heapless::String;
use panic_probe as _;
use ssd1306_async::{I2CDisplayInterface, Ssd1306, prelude::*};
use tinybmp::Bmp;

/// Wrapper for ValidityFlag to add PartialEq
#[derive(Debug, Copy, Clone)]
pub struct ValidityFlagWrapper(pub ValidityFlag);

impl PartialEq for ValidityFlagWrapper {
    fn eq(&self, other: &Self) -> bool {
        // Compare based on the discriminants since ValidityFlag doesn't implement PartialEq
        core::mem::discriminant(&self.0) == core::mem::discriminant(&other.0)
    }
}

impl From<ValidityFlag> for ValidityFlagWrapper {
    fn from(flag: ValidityFlag) -> Self {
        ValidityFlagWrapper(flag)
    }
}

/// Commands for controlling the display
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum DisplayCommand {
    /// Update the display with the current sensor data
    UpdateSensorData {
        temperature: f32,
        humidity: f32,
        co2: u16,
        etoh: u16,
        air_quality: AirQualityIndex,
        ens160_validity: ValidityFlagWrapper,
    },
    /// Update the battery charging state
    UpdateBatteryCharging(bool),
    /// Update the battery level
    UpdateBatteryPercentage(u8),
}

/// Signal for triggering state updates
pub static DISPLAY: Signal<CriticalSectionRawMutex, DisplayCommand> = Signal::new();

/// Triggers a display update with the provided command
pub fn send_display_command(command: DisplayCommand) {
    DISPLAY.signal(command);
}

/// Waits for next indicator state change signal
async fn wait_for_display_command() -> DisplayCommand {
    DISPLAY.wait().await
}

#[embassy_executor::task]
pub async fn display_task(i2c_device: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>) {
    // Initialize the display
    let interface = I2CDisplayInterface::new(i2c_device);
    let mut display =
        Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0).into_buffered_graphics_mode();
    match display.init().await {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to initialize display: {}", Debug2Format(&e));
            return;
        }
    }

    match display.set_brightness(Brightness::DIMMEST).await {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to set display brightness: {}", Debug2Format(&e));
            return; // ToDo: Handle error
        }
    }

    // Clear the display
    display.clear();
    match display.flush().await {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to flush display: {}", Debug2Format(&e));
            // but we continue to run the display task, hoping it will recover
        }
    }

    // Create settings for the display
    let settings = Settings::new();
    let mut state = DisplayState::new();

    loop {
        let command = wait_for_display_command().await;

        display.clear();

        match command {
            DisplayCommand::UpdateSensorData {
                temperature,
                humidity,
                co2,
                etoh,
                air_quality,
                ens160_validity,
            } => {
                // Check if ENS160 is in InitialStartupPhase
                let is_initial_startup = matches!(ens160_validity.0, ValidityFlag::InitialStartupPhase);

                if is_initial_startup {
                    // Draw the settings icon instead of air quality
                    let settings_image = Image::new(&settings.settings_icon, settings.air_quality_position);
                    settings_image.draw(&mut display.color_converted()).unwrap_or_default();
                } else {
                    // Draw the air quality text
                    let mut aq_text: String<12> = String::new();
                    let _ = write!(aq_text, "{:?}", air_quality);
                    Text::with_baseline(
                        &aq_text,
                        settings.air_quality_position,
                        settings.air_quality_text_style,
                        Baseline::Top,
                    )
                    .draw(&mut display.color_converted())
                    .unwrap_or_default();
                }

                // Draw the CO2 text
                let mut co2_text: String<16> = String::new();
                let _ = write!(co2_text, "CO2: {} ppm", co2);
                Text::with_baseline(&co2_text, settings.co2_position, settings.co2_text_style, Baseline::Top)
                    .draw(&mut display.color_converted())
                    .unwrap_or_default();

                // Draw the Ethanol text
                let mut etoh_text: String<16> = String::new();
                let etoh_value = etoh;
                let _ = write!(etoh_text, "EtOH: {} ppb", etoh_value);
                Text::with_baseline(
                    &etoh_text,
                    settings.etoh_position,
                    settings.etoh_text_style,
                    Baseline::Top,
                )
                .draw(&mut display.color_converted())
                .unwrap_or_default();

                // Draw the temperature text
                let mut temp_text: String<16> = String::new();
                let _ = write!(temp_text, "Temp: {:.1}C", temperature);
                Text::with_baseline(
                    &temp_text,
                    settings.temperature_position,
                    settings.temperature_text_style,
                    Baseline::Top,
                )
                .draw(&mut display.color_converted())
                .unwrap_or_default();

                // Draw the humidity text
                let mut humidity_text: String<16> = String::new();
                let _ = write!(humidity_text, "Humidity: {:.1}%", humidity);
                Text::with_baseline(
                    &humidity_text,
                    settings.humidity_position,
                    settings.humidity_text_style,
                    Baseline::Top,
                )
                .draw(&mut display.color_converted())
                .unwrap_or_default();
            }
            DisplayCommand::UpdateBatteryCharging(is_charging) => {
                state.is_charging = is_charging;
            }
            DisplayCommand::UpdateBatteryPercentage(bat_percent) => {
                state.battery_percent = bat_percent;
            }
        }

        let battery_icon = settings.get_battery_icon(state.get_battery_level());
        let bat_image = Image::new(battery_icon, settings.bat_position);
        bat_image.draw(&mut display.color_converted()).unwrap();

        match display.flush().await {
            Ok(_) => {}
            Err(e) => {
                error!("Failed to flush display: {}", Debug2Format(&e));
                continue;
            }
        }
    }
}

/// Loads and holds BMP images and Points for the display
/// Holds some settings for composing the display
struct Settings<'a> {
    /// BMP images of the battery status icons
    bat: [Bmp<'static, Gray8>; 6],
    /// BMP image of the battery mains icon
    bat_mains: Bmp<'static, Gray8>,
    /// BMP image of the settings icon
    settings_icon: Bmp<'static, Gray8>,
    /// Position of the battery status images, hight is 11
    bat_position: Point,
    /// Position of the air quality text
    air_quality_position: Point,
    /// Style of the air quality text
    air_quality_text_style: MonoTextStyle<'a, BinaryColor>,
    /// Position of the CO2 text
    co2_position: Point,
    /// Style of the CO2 text
    co2_text_style: MonoTextStyle<'a, BinaryColor>,
    /// Position of the etoh text
    etoh_position: Point,
    /// Style of the etoh text
    etoh_text_style: MonoTextStyle<'a, BinaryColor>,
    /// Position of the temperature text
    temperature_position: Point,
    /// Style of the temperature text
    temperature_text_style: MonoTextStyle<'a, BinaryColor>,
    /// Position of the humidity text
    humidity_position: Point,
    /// Style of the humidity text
    humidity_text_style: MonoTextStyle<'a, BinaryColor>,
}

impl<'a> Settings<'a> {
    fn new() -> Self {
        Self {
            bat: [
                Bmp::from_slice(include_bytes!("media/bat_000.bmp")).expect("Failed to load BMP image"),
                Bmp::from_slice(include_bytes!("media/bat_020.bmp")).expect("Failed to load BMP image"),
                Bmp::from_slice(include_bytes!("media/bat_040.bmp")).expect("Failed to load BMP image"),
                Bmp::from_slice(include_bytes!("media/bat_060.bmp")).expect("Failed to load BMP image"),
                Bmp::from_slice(include_bytes!("media/bat_080.bmp")).expect("Failed to load BMP image"),
                Bmp::from_slice(include_bytes!("media/bat_100.bmp")).expect("Failed to load BMP image"),
            ],
            bat_mains: Bmp::from_slice(include_bytes!("media/bat_mains.bmp")).expect("Failed to load BMP image"),
            settings_icon: Bmp::from_slice(include_bytes!("media/settings.bmp")).expect("Failed to load BMP image"),
            bat_position: Point::new(108, 0),
            air_quality_text_style: MonoTextStyleBuilder::new()
                .font(&FONT_8X13_BOLD)
                .text_color(BinaryColor::On)
                .build(),
            air_quality_position: Point::new(0, 0),
            co2_position: Point::new(0, 14),
            co2_text_style: MonoTextStyleBuilder::new()
                .font(&FONT_6X13)
                .text_color(BinaryColor::On)
                .build(),
            etoh_position: Point::new(0, 26),
            etoh_text_style: MonoTextStyleBuilder::new()
                .font(&FONT_6X13)
                .text_color(BinaryColor::On)
                .build(),
            temperature_position: Point::new(0, 38),
            temperature_text_style: MonoTextStyleBuilder::new()
                .font(&FONT_6X13)
                .text_color(BinaryColor::On)
                .build(),
            humidity_position: Point::new(0, 50),
            humidity_text_style: MonoTextStyleBuilder::new()
                .font(&FONT_6X13)
                .text_color(BinaryColor::On)
                .build(),
        }
    }

    fn get_battery_icon(&self, level: BatteryLevel) -> &Bmp<'static, Gray8> {
        match level {
            BatteryLevel::Charging => &self.bat_mains,
            BatteryLevel::Bat000 => &self.bat[0],
            BatteryLevel::Bat020 => &self.bat[1],
            BatteryLevel::Bat040 => &self.bat[2],
            BatteryLevel::Bat060 => &self.bat[3],
            BatteryLevel::Bat080 => &self.bat[4],
            BatteryLevel::Bat100 => &self.bat[5],
        }
    }
}

struct DisplayState {
    /// Current battery level
    battery_percent: u8,
    /// Whether the battery is charging
    is_charging: bool,
}

impl<'a> DisplayState {
    fn new() -> Self {
        Self {
            battery_percent: 0,
            is_charging: false,
        }
    }

    fn get_battery_level(&self) -> BatteryLevel {
        if self.is_charging {
            BatteryLevel::Charging
        } else {
            match self.battery_percent {
                0..=19 => BatteryLevel::Bat020,
                20..=39 => BatteryLevel::Bat040,
                40..=59 => BatteryLevel::Bat060,
                60..=79 => BatteryLevel::Bat080,
                80..=100 => BatteryLevel::Bat100,
                _ => BatteryLevel::Bat000, // Fallback case
            }
        }
    }
}

#[derive(PartialEq, Debug, Clone)]
pub enum BatteryLevel {
    Charging,
    Bat000,
    Bat020,
    Bat040,
    Bat060,
    Bat080,
    Bat100,
}
