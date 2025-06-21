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
    channel::Channel,
};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    image::Image,
    mono_font::{
        MonoTextStyle, MonoTextStyleBuilder,
        ascii::{FONT_5X8, FONT_6X13, FONT_8X13_BOLD},
    },
    pixelcolor::{BinaryColor, Gray8},
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use ens160_aq::data::AirQualityIndex;
use heapless::String;
use panic_probe as _;
use ssd1306_async::{I2CDisplayInterface, Ssd1306, prelude::*};
use tinybmp::Bmp;

use crate::{
    event::{Event, send_event},
    system_state::{BatteryLevel, DisplayMode, SYSTEM_STATE, SensorData},
    watchdog::trigger_watchdog_reset,
};

/// Channel for triggering state updates  
pub static DISPLAY_CHANNEL: Channel<CriticalSectionRawMutex, DisplayCommand, 3> = Channel::new();

/// Duration for toggling display modes
static TOGGLE_MODE: Duration = Duration::from_secs(10);

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
    UpdateBatteryCharging,
    /// Update the battery level
    UpdateBatteryPercentage(u8),
    /// Toggle display mode (triggered by mode switching task)
    ToggleMode,
}

/// Triggers a display update with the provided command
pub async fn send_display_command(command: DisplayCommand) {
    DISPLAY_CHANNEL.send(command).await;
}

/// Waits for next indicator state change signal
async fn wait_for_display_command() -> DisplayCommand {
    DISPLAY_CHANNEL.receive().await
}

#[embassy_executor::task]
#[allow(clippy::too_many_lines)]
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

    info!("Display task initialized successfully");

    // Show initial startup screen
    settings.draw_initialization_message(&mut display.color_converted());
    {
        let state = SYSTEM_STATE.lock().await;
        settings.draw_battery_icon(&mut display.color_converted(), &state.get_battery_level());
    }
    if let Err(e) = display.flush().await {
        error!("Failed to flush initial screen (continuing): {}", Debug2Format(&e));
    }

    // Main display loop - all errors here are considered transient
    loop {
        let command = wait_for_display_command().await;

        match command {
            DisplayCommand::SensorData {
                temperature,
                humidity,
                co2,
                etoh,
                air_quality,
            } => {
                // Create the sensor data structure
                let sensor_data = SensorData {
                    temperature,
                    humidity,
                    co2,
                    etoh,
                    air_quality,
                };

                // Clear main content area (preserves battery icon)
                settings.clear_main_area(&mut display.color_converted());

                // Draw based on current display mode
                {
                    let state = SYSTEM_STATE.lock().await;
                    match state.get_display_mode() {
                        DisplayMode::RawData => {
                            settings.draw_sensor_data(&mut display.color_converted(), &sensor_data);
                        }
                        DisplayMode::Co2History => {
                            settings.draw_co2_history(&mut display.color_converted(), state.get_co2_history());
                        }
                    }

                    // Draw battery icon
                    settings.draw_battery_icon(&mut display.color_converted(), &state.get_battery_level());
                }
            }
            DisplayCommand::UpdateBatteryCharging => {
                // Only clear and redraw battery icon area
                settings.clear_battery_area(&mut display.color_converted());
                {
                    let state = SYSTEM_STATE.lock().await;
                    settings.draw_battery_icon(&mut display.color_converted(), &state.get_battery_level());
                }
            }
            DisplayCommand::UpdateBatteryPercentage(_bat_percent) => {
                // Only clear and redraw battery icon area
                settings.clear_battery_area(&mut display.color_converted());
                {
                    let state = SYSTEM_STATE.lock().await;
                    settings.draw_battery_icon(&mut display.color_converted(), &state.get_battery_level());
                }
            }
            DisplayCommand::ToggleMode => {
                // State has already been toggled by orchestrator, just redraw
                let sensor_data_option = {
                    let state = SYSTEM_STATE.lock().await;
                    state.last_sensor_data.clone()
                };

                settings.clear_main_area(&mut display.color_converted());
                if let Some(sensor_data) = sensor_data_option {
                    // Redraw with the current mode
                    {
                        let state = SYSTEM_STATE.lock().await;
                        match state.get_display_mode() {
                            DisplayMode::RawData => {
                                settings.draw_sensor_data(&mut display.color_converted(), &sensor_data);
                            }
                            DisplayMode::Co2History => {
                                settings.draw_co2_history(&mut display.color_converted(), state.get_co2_history());
                            }
                        }
                    }
                } else {
                    // No sensor data yet, clear main area and show initialization message
                    settings.draw_initialization_message(&mut display.color_converted());
                }

                // Draw battery icon (common to both branches)
                {
                    let state = SYSTEM_STATE.lock().await;
                    settings.draw_battery_icon(&mut display.color_converted(), &state.get_battery_level());
                }
            }
        }

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
    /// Position for min text in CO2 history
    minmax_min_position: Point,
    /// Position for max text in CO2 history
    minmax_max_position: Point,
    /// Style for min/max labels in CO2 history chart
    minmax_text_style: MonoTextStyle<'a, BinaryColor>,
    /// Bar chart starting Y position
    chart_start_y: i32,
    /// Bar chart height
    chart_height: i32,
    /// Bar chart width
    chart_width: i32,
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
            minmax_min_position: Point::new(0, 57),
            minmax_max_position: Point::new(64, 57),
            minmax_text_style: MonoTextStyleBuilder::new()
                .font(&FONT_5X8)
                .text_color(BinaryColor::On)
                .build(),
            chart_start_y: 17,
            chart_height: 39,
            chart_width: 128,
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

    /// Clears only the battery icon area (preserves main content)
    fn clear_battery_area<D>(&self, display: &mut D)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // Battery icon is 20x11 pixels at position (108, 1)
        let battery_area = Rectangle::new(self.bat_position, Size::new(20, 11));
        battery_area
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(display)
            .unwrap_or_default();
    }

    /// Clears the main content area (preserves battery icon)
    fn clear_main_area<D>(&self, display: &mut D)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // Clear everything except the battery icon area
        // Clear the main content area (everything to the left of battery icon)
        #[allow(clippy::cast_sign_loss)]
        let main_left_area = Rectangle::new(Point::new(0, 0), Size::new(self.bat_position.x.max(0) as u32, 64));
        main_left_area
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(display)
            .unwrap_or_default();

        // Clear the area below the battery icon (y > battery_bottom)
        let battery_bottom = self.bat_position.y + 11; // Battery icon is 11 pixels tall
        if battery_bottom < 64 {
            #[allow(clippy::cast_sign_loss)]
            let main_bottom_area = Rectangle::new(
                Point::new(self.bat_position.x, battery_bottom),
                Size::new(20, (64 - battery_bottom).max(0) as u32),
            );
            main_bottom_area
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                .draw(display)
                .unwrap_or_default();
        }
    }

    /// Helper function to draw the battery icon
    fn draw_battery_icon<D>(&self, display: &mut D, battery_level: &BatteryLevel)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let battery_icon = self.get_battery_icon(battery_level);
        let bat_image = Image::new(battery_icon, self.bat_position);
        bat_image.draw(&mut display.color_converted()).unwrap_or_default();
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

    /// Draws CO2 history bar chart to the display
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    fn draw_co2_history<D>(&self, display: &mut D, co2_history: &[u16])
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // Draw the title "CO2 history" where air quality normally appears
        Text::with_baseline(
            "CO2 history",
            self.air_quality_position,
            self.air_quality_text_style,
            Baseline::Top,
        )
        .draw(display)
        .unwrap_or_default();

        if co2_history.is_empty() {
            // Show message if no history available
            Text::with_baseline("No data yet", self.co2_position, self.co2_text_style, Baseline::Top)
                .draw(display)
                .unwrap_or_default();
            return;
        }

        // Find min and max CO2 values for scaling
        let min_co2 = *co2_history.iter().min().unwrap_or(&0);
        let max_co2 = *co2_history.iter().max().unwrap_or(&1000);

        // Avoid division by zero
        let range = if max_co2 > min_co2 { max_co2 - min_co2 } else { 1 };

        // Bar chart area: configured in Settings
        let chart_start_y = self.chart_start_y;
        let chart_height = self.chart_height;
        let chart_width = self.chart_width;
        #[allow(clippy::cast_possible_truncation)]
        let bar_width = chart_width / co2_history.len().max(1) as i32;

        // Draw bars
        for (i, &co2_value) in co2_history.iter().enumerate() {
            // Calculate bar height (scaled to chart area)
            let normalized_value = co2_value.saturating_sub(min_co2);
            let bar_height = if range > 0 {
                (i32::from(normalized_value) * chart_height) / i32::from(range)
            } else {
                1
            };

            // Calculate bar position
            #[allow(clippy::cast_possible_truncation)]
            let bar_x = i as i32 * bar_width;
            let bar_y = chart_start_y + chart_height - bar_height; // Draw from bottom up

            // Draw the bar
            let bar_rect = Rectangle::new(
                Point::new(bar_x, bar_y),
                Size::new(
                    (bar_width - 1).max(0) as u32, // -1 for spacing between bars, ensure non-negative
                    bar_height.max(0) as u32,
                ),
            );
            bar_rect
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(display)
                .unwrap_or_default();
        }

        // Draw min/max labels - using configured positions and smaller font
        let mut min_text: String<16> = String::new();
        let _ = write!(min_text, "Min: {min_co2}");
        Text::with_baseline(
            &min_text,
            self.minmax_min_position,
            self.minmax_text_style,
            Baseline::Top,
        )
        .draw(display)
        .unwrap_or_default();

        let mut max_text: String<16> = String::new();
        let _ = write!(max_text, "Max: {max_co2}");
        Text::with_baseline(
            &max_text,
            self.minmax_max_position,
            self.minmax_text_style,
            Baseline::Top,
        )
        .draw(display)
        .unwrap_or_default();
    }
}

/// Mode switching task that sends ToggleDisplayMode events every 10 seconds
#[embassy_executor::task]
pub async fn mode_switch_task() {
    loop {
        Timer::after(TOGGLE_MODE).await;

        // Send toggle mode event to orchestrator
        send_event(Event::ToggleDisplayMode).await;
    }
}
