//! Display task for the Air Quality Monitor

use core::fmt::Write;

use defmt::{Debug2Format, error, info};
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
use ens160_aq::data::AirQualityIndex;
use heapless::String;
use panic_probe as _;
use ssd1306_async::{I2CDisplayInterface, Ssd1306, prelude::*};
use tinybmp::Bmp;

use crate::watchdog::trigger_watchdog_reset;

/// Commands for controlling the display
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum DisplayCommand {
    /// Update the display with the current sensor data
    SensorData {
        /// Temperature in degrees Celsius
        temperature: f32,
        /// Humidity in percentage
        humidity: f32,
        /// CO2 level in ppm
        co2: u16,
        /// Ethanol level in ppb
        etoh: u16,
        /// Air quality index
        air_quality: AirQualityIndex,
    },
    /// Update the battery charging state
    BatteryCharging(bool),
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

    // Critical initialization - if this fails, we need to reset
    if let Err(e) = display.init().await {
        error!(
            "Failed to initialize display: {} - triggering system reset",
            Debug2Format(&e)
        );
        trigger_watchdog_reset();
        return;
    }

    if let Err(e) = display.set_brightness(Brightness::DIMMEST).await {
        error!(
            "Failed to set display brightness: {} - triggering system reset",
            Debug2Format(&e)
        );
        trigger_watchdog_reset();
        return;
    }

    // Clear the display - this is still critical initialization
    display.clear();
    if let Err(e) = display.flush().await {
        error!(
            "Failed to initial display flush: {} - triggering system reset",
            Debug2Format(&e)
        );
        trigger_watchdog_reset();
        return;
    }

    // Create settings for the display
    let settings = match Settings::new() {
        Ok(settings) => settings,
        Err(e) => {
            error!("Failed to load display assets: {} - triggering system reset", e);
            trigger_watchdog_reset();
            return;
        }
    };
    let mut state = DisplayState::new();

    info!("Display task initialized successfully");

    // Main display loop - all errors here are considered transient
    loop {
        let command = wait_for_display_command().await;

        display.clear();

        match command {
            DisplayCommand::SensorData {
                temperature,
                humidity,
                co2,
                etoh,
                air_quality,
            } => {
                // Store the new sensor data
                let sensor_data = SensorData {
                    temperature,
                    humidity,
                    co2,
                    etoh,
                    air_quality,
                };

                // Draw the sensor data
                settings.draw_sensor_data(&mut display.color_converted(), &sensor_data);

                // Cache the sensor data for future battery-only updates
                state.last_sensor_data = Some(sensor_data);
            }
            DisplayCommand::BatteryCharging(is_charging) => {
                state.is_charging = is_charging;

                // Always redraw sensor data if we have it
                if let Some(ref sensor_data) = state.last_sensor_data {
                    settings.draw_sensor_data(&mut display.color_converted(), sensor_data);
                } else {
                    // If no sensor data yet, show a simple status message
                    settings.draw_initialization_message(&mut display.color_converted());
                }
            }
            DisplayCommand::UpdateBatteryPercentage(bat_percent) => {
                state.battery_percent = bat_percent;

                // Always redraw sensor data if we have it
                if let Some(ref sensor_data) = state.last_sensor_data {
                    settings.draw_sensor_data(&mut display.color_converted(), sensor_data);
                } else {
                    // If no sensor data yet, show a simple status message
                    settings.draw_initialization_message(&mut display.color_converted());
                }
            }
        }

        // Draw battery icon
        let battery_icon = settings.get_battery_icon(&state.get_battery_level());
        let bat_image = Image::new(battery_icon, settings.bat_position);
        bat_image.draw(&mut display.color_converted()).unwrap_or_default();

        // Flush display - if this fails, it's transient, so we continue
        if let Err(e) = display.flush().await {
            error!("Failed to flush display (continuing): {}", Debug2Format(&e));
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
    init_icon: Bmp<'static, Gray8>,
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
    /// Position of the sensor initialization message
    sensor_init_position: Point,
    /// Style of the sensor initialization message
    sensor_init_text_style: MonoTextStyle<'a, BinaryColor>,
}

impl Settings<'_> {
    /// Creates a new `Settings` instance by loading BMP images and setting up text styles
    fn new() -> Result<Self, &'static str> {
        let bat_000 = Bmp::from_slice(include_bytes!("media/bat_000.bmp")).map_err(|_| "Failed to load bat_000.bmp")?;
        let bat_020 = Bmp::from_slice(include_bytes!("media/bat_020.bmp")).map_err(|_| "Failed to load bat_020.bmp")?;
        let bat_040 = Bmp::from_slice(include_bytes!("media/bat_040.bmp")).map_err(|_| "Failed to load bat_040.bmp")?;
        let bat_060 = Bmp::from_slice(include_bytes!("media/bat_060.bmp")).map_err(|_| "Failed to load bat_060.bmp")?;
        let bat_080 = Bmp::from_slice(include_bytes!("media/bat_080.bmp")).map_err(|_| "Failed to load bat_080.bmp")?;
        let bat_100 = Bmp::from_slice(include_bytes!("media/bat_100.bmp")).map_err(|_| "Failed to load bat_100.bmp")?;
        let bat_mains =
            Bmp::from_slice(include_bytes!("media/bat_mains.bmp")).map_err(|_| "Failed to load bat_mains.bmp")?;
        let settings_icon =
            Bmp::from_slice(include_bytes!("media/settings.bmp")).map_err(|_| "Failed to load settings.bmp")?;

        Ok(Self {
            bat: [bat_000, bat_020, bat_040, bat_060, bat_080, bat_100],
            bat_mains,
            init_icon: settings_icon,
            bat_position: Point::new(108, 1),
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
            sensor_init_position: Point::new(0, 30),
            sensor_init_text_style: MonoTextStyleBuilder::new()
                .font(&FONT_6X13)
                .text_color(BinaryColor::On)
                .build(),
        })
    }

    /// Returns the appropriate battery icon based on the current battery level
    const fn get_battery_icon(&self, level: &BatteryLevel) -> &Bmp<'static, Gray8> {
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

    /// Draws an initialization message when no sensor data is available
    fn draw_initialization_message<D>(&self, display: &mut D)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // Draw the settings icon
        let settings_image = Image::new(&self.init_icon, self.air_quality_position);
        settings_image.draw(&mut display.color_converted()).unwrap_or_default();

        Text::with_baseline(
            "Initializing sensors",
            self.sensor_init_position,
            self.sensor_init_text_style,
            Baseline::Top,
        )
        .draw(display)
        .unwrap_or_default();
    }

    /// Draws sensor data to the display
    fn draw_sensor_data<D>(&self, display: &mut D, sensor_data: &SensorData)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // Draw the air quality text
        let mut aq_text: String<12> = String::new();
        let _ = write!(aq_text, "{:?}", sensor_data.air_quality);
        Text::with_baseline(
            &aq_text,
            self.air_quality_position,
            self.air_quality_text_style,
            Baseline::Top,
        )
        .draw(display)
        .unwrap_or_default();

        // Draw the CO2 text
        let mut co2_text: String<16> = String::new();
        let _ = write!(co2_text, "CO2: {} ppm", sensor_data.co2);
        Text::with_baseline(&co2_text, self.co2_position, self.co2_text_style, Baseline::Top)
            .draw(display)
            .unwrap_or_default();

        // Draw the Ethanol text
        let mut etoh_text: String<16> = String::new();
        let _ = write!(etoh_text, "EtOH: {} ppb", sensor_data.etoh);
        Text::with_baseline(&etoh_text, self.etoh_position, self.etoh_text_style, Baseline::Top)
            .draw(display)
            .unwrap_or_default();

        // Draw the temperature text
        let mut temp_text: String<16> = String::new();
        let _ = write!(temp_text, "Temp: {:.1}C", sensor_data.temperature);
        Text::with_baseline(
            &temp_text,
            self.temperature_position,
            self.temperature_text_style,
            Baseline::Top,
        )
        .draw(display)
        .unwrap_or_default();

        // Draw the humidity text
        let mut humidity_text: String<16> = String::new();
        let _ = write!(humidity_text, "Humidity: {:.1}%", sensor_data.humidity);
        Text::with_baseline(
            &humidity_text,
            self.humidity_position,
            self.humidity_text_style,
            Baseline::Top,
        )
        .draw(display)
        .unwrap_or_default();
    }
}

/// Holds the current state of the display, including battery level and sensor data
struct DisplayState {
    /// Current battery level
    battery_percent: u8,
    /// Whether the battery is charging
    is_charging: bool,
    /// Last sensor data for redrawing
    last_sensor_data: Option<SensorData>,
}

/// Holds the sensor data to be displayed
#[derive(Clone)]
struct SensorData {
    /// Temperature in degrees Celsius
    temperature: f32,
    /// Humidity in percentage
    humidity: f32,
    /// CO2 level in ppm
    co2: u16,
    /// Ethanol level in ppb
    etoh: u16,
    /// Air quality index
    air_quality: AirQualityIndex,
}

impl DisplayState {
    /// Creates a new `DisplayState` with default values
    const fn new() -> Self {
        Self {
            battery_percent: 0,
            is_charging: false,
            last_sensor_data: None,
        }
    }

    /// Returns the current battery level based on the battery percentage and charging state
    const fn get_battery_level(&self) -> BatteryLevel {
        if self.is_charging {
            BatteryLevel::Charging
        } else {
            match self.battery_percent {
                0..=25 => BatteryLevel::Bat000,  // 26% range - compensates for quick drop
                26..=45 => BatteryLevel::Bat020, // 20% range - medium compensation
                46..=65 => BatteryLevel::Bat040, // 20% range - some compensation
                66..=80 => BatteryLevel::Bat060, // 15% range - less time needed
                81..=90 => BatteryLevel::Bat080, // 10% range - short time
                _ => BatteryLevel::Bat100,
            }
        }
    }
}

/// The Charge Level of the battery
#[derive(PartialEq, Debug, Clone, Eq)]
pub enum BatteryLevel {
    /// Battery is charging
    Charging,
    /// Battery levels
    /// roughly 1/6 of the run time left
    Bat000,
    /// roughly 1/3 of the run time left
    Bat020,
    /// roughly 3/6 of the run time left
    Bat040,
    /// roughly 2/3 fifths of the run time left
    Bat060,
    /// roughly 5/6 of the run time left
    Bat080,
    /// Almost full, most of the run time left
    Bat100,
}
