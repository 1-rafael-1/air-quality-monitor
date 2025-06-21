//! Display state management for the Air Quality Monitor

use ens160_aq::data::AirQualityIndex;
use heapless::Vec;

/// Display modes for alternating between raw data and history graphs
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum DisplayMode {
    /// Show raw sensor data
    RawData,
    /// Show CO2 history bar chart
    Co2History,
}

/// Holds the current state of the display, including battery level and sensor data
pub struct DisplayState {
    /// Current battery level
    battery_percent: u8,
    /// Whether the battery is charging
    is_charging: bool,
    /// Last sensor data for redrawing
    pub last_sensor_data: Option<SensorData>,
    /// CO2 history buffer (last 10 measurements)
    co2_history: Vec<u16, 10>,
    /// Current display mode
    display_mode: DisplayMode,
}

/// Holds the sensor data to be displayed
#[derive(Clone)]
pub struct SensorData {
    /// Temperature in degrees Celsius
    pub temperature: f32,
    /// Humidity in percentage
    pub humidity: f32,
    /// CO2 level in ppm
    pub co2: u16,
    /// Ethanol level in ppb
    pub etoh: u16,
    /// Air quality index
    pub air_quality: AirQualityIndex,
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

impl DisplayState {
    /// Creates a new `DisplayState` with default values
    pub const fn new() -> Self {
        Self {
            battery_percent: 0,
            is_charging: false,
            last_sensor_data: None,
            co2_history: Vec::new(),
            display_mode: DisplayMode::RawData,
        }
    }

    /// Sets the battery percentage
    pub fn set_battery_percent(&mut self, percent: u8) {
        self.battery_percent = percent;
    }

    /// Sets the charging state
    pub fn set_charging(&mut self, is_charging: bool) {
        self.is_charging = is_charging;
    }

    /// Adds a CO2 measurement to the history buffer
    pub fn add_co2_measurement(&mut self, co2: u16) {
        if self.co2_history.len() >= 10 {
            // Remove the oldest measurement if buffer is full
            self.co2_history.remove(0);
        }
        // Add the new measurement (ignore if push fails - shouldn't happen due to above check)
        let _ = self.co2_history.push(co2);
    }

    /// Toggles the display mode between raw data and CO2 history
    pub fn toggle_display_mode(&mut self) {
        self.display_mode = match self.display_mode {
            DisplayMode::RawData => DisplayMode::Co2History,
            DisplayMode::Co2History => DisplayMode::RawData,
        };
    }

    /// Gets the current display mode
    pub const fn get_display_mode(&self) -> DisplayMode {
        self.display_mode
    }

    /// Gets the CO2 history for drawing charts
    pub fn get_co2_history(&self) -> &[u16] {
        &self.co2_history
    }

    /// Returns the current battery level based on the battery percentage and charging state
    pub const fn get_battery_level(&self) -> BatteryLevel {
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
