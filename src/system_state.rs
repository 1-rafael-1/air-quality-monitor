//! System state management for the Air Quality Monitor

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use ens160_aq::data::AirQualityIndex;
use heapless::Vec;

/// Global system state - initialized with default values
pub static SYSTEM_STATE: Mutex<CriticalSectionRawMutex, SystemState> = Mutex::new(SystemState::new());

/// Display modes for alternating between raw data and history graphs
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum DisplayMode {
    /// Show raw sensor data
    RawData,
    /// Show CO2 history bar chart
    Co2History,
}

/// Holds the current state of the system, including battery level and sensor data
pub struct SystemState {
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
    /// ENS160 calibration state
    ens160_calibration_state: Ens160CalibrationState,
}

/// Represents the calibration state of the ENS160 sensor
#[derive(Debug, Clone, Copy)]
pub struct Ens160CalibrationState {
    /// Whether the sensor has completed its initial 25-hour calibration
    pub is_calibrated: bool,
    /// Start time of continuous operation (in seconds since boot)
    pub calibration_start_time: Option<u64>,
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

impl SystemState {
    /// Creates a new `SystemState` with default values
    pub const fn new() -> Self {
        Self {
            battery_percent: 100,
            is_charging: false,
            last_sensor_data: None,
            co2_history: Vec::new(),
            display_mode: DisplayMode::RawData,
            ens160_calibration_state: Ens160CalibrationState {
                is_calibrated: false,
                calibration_start_time: None,
            },
        }
    }

    /// Sets the last sensor data
    pub const fn set_last_sensor_data(&mut self, data: SensorData) {
        self.last_sensor_data = Some(data);
    }

    /// Sets the battery percentage
    pub const fn set_battery_percent(&mut self, percent: u8) {
        self.battery_percent = percent;
    }

    /// Sets the charging state
    pub const fn set_charging(&mut self, is_charging: bool) {
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
    pub const fn toggle_display_mode(&mut self) {
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
    /// Attempts to compensate for the fact that the voltage of the battery does not change linearly but drops way steeper at the end
    pub const fn get_battery_level(&self) -> BatteryLevel {
        if self.is_charging {
            BatteryLevel::Charging
        } else {
            match self.battery_percent {
                0..=24 => BatteryLevel::Bat000,
                25..=44 => BatteryLevel::Bat020,
                45..=58 => BatteryLevel::Bat040,
                59..=72 => BatteryLevel::Bat060,
                73..=86 => BatteryLevel::Bat080,
                _ => BatteryLevel::Bat100,
            }
        }
    }

    /// Gets the ENS160 calibration state
    pub const fn get_ens160_calibration_state(&self) -> Ens160CalibrationState {
        self.ens160_calibration_state
    }

    /// Starts the ENS160 calibration period
    pub const fn start_ens160_calibration(&mut self, start_time: u64) {
        self.ens160_calibration_state.calibration_start_time = Some(start_time);
        self.ens160_calibration_state.is_calibrated = false;
    }

    /// Marks the ENS160 as fully calibrated
    pub const fn mark_ens160_calibrated(&mut self) {
        self.ens160_calibration_state.is_calibrated = true;
        self.ens160_calibration_state.calibration_start_time = None;
    }

    /// Resets the ENS160 calibration state (e.g., after sensor reset)
    #[allow(dead_code)]
    pub const fn reset_ens160_calibration(&mut self) {
        self.ens160_calibration_state.is_calibrated = false;
        self.ens160_calibration_state.calibration_start_time = None;
    }
}
