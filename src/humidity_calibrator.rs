//! Humidity calibration module for adaptive baseline and statistical drift correction.

use defmt::info;
use heapless::Vec;

/// Number of initial readings to treat as baseline truth
const INITIAL_BASELINE_READINGS: usize = 5;

/// Very conservative drift learning rate (much slower than before)
const DRIFT_LEARNING_RATE: f32 = 0.02;

/// Minimum drift threshold - only correct drift above this amount
const MIN_DRIFT_THRESHOLD: f32 = 2.0;

/// Rapid change threshold - changes above this are considered environmental events
const RAPID_CHANGE_THRESHOLD: f32 = 5.0;

/// Number of recent readings to track for change rate analysis
const CHANGE_HISTORY_SIZE: usize = 3;

/// Minimum stable period (readings) before resuming calibration after rapid change
const MIN_STABLE_READINGS_AFTER_RAPID_CHANGE: usize = 12; // ~1 hour at 5min intervals

/// Baseline shift threshold - sustained changes above this indicate new environmental baseline
const BASELINE_SHIFT_THRESHOLD: f32 = 8.0;

/// Number of readings to confirm a baseline shift
const BASELINE_SHIFT_CONFIRMATION_READINGS: usize = 6; // ~30 minutes

/// Long-term drift detection: minimum readings before using statistical expectation for drift detection
const MIN_READINGS_FOR_LONG_TERM_DRIFT: usize = 100; // ~8 hours of stable readings

/// Long-term drift threshold: only apply statistical drift correction for deviations this large
const LONG_TERM_DRIFT_THRESHOLD: f32 = 10.0; // 10% deviation from expected

/// Very conservative long-term drift learning rate
const LONG_TERM_DRIFT_LEARNING_RATE: f32 = 0.005; // Even slower than regular drift correction

/// Recent humidity reading for change rate analysis
#[derive(Clone, Copy)]
struct RecentReading {
    /// Raw humidity measurement
    raw_humidity: f32,
}

/// Statistical humidity calibration system
/// Uses a hybrid approach:
/// 1. Adaptive baseline: Handles rapid environmental changes by establishing and updating baselines
/// 2. Statistical expectation: Provides long-term drift correction against theoretical indoor humidity ranges
pub struct HumidityCalibrator {
    /// Recent readings for change rate analysis
    recent_readings: Vec<RecentReading, CHANGE_HISTORY_SIZE>,
    /// Current drift correction offset
    humidity_offset: f32,
    /// Current environmental baseline (established from initial readings or after rapid changes)
    current_baseline: Option<f32>,
    /// Number of readings used to establish current baseline
    pub baseline_reading_count: usize,
    /// Current reading sequence number
    reading_sequence: u32,
    /// Number of consecutive stable readings
    stable_reading_count: usize,
    /// Flag indicating if we're in a rapid change period
    in_rapid_change_period: bool,
    /// Baseline humidity level before the current change period
    pre_change_baseline: Option<f32>,
    /// Number of readings confirming the current level
    baseline_confirmation_count: usize,
    /// Whether we've detected a sustained baseline shift
    pub baseline_shifted: bool,
    /// Long-term statistical drift offset (separate from short-term baseline drift)
    long_term_statistical_offset: f32,
    /// Number of stable readings accumulated for long-term drift analysis
    long_term_stable_count: usize,
}

impl HumidityCalibrator {
    /// Create a new humidity calibrator
    pub const fn new() -> Self {
        Self {
            recent_readings: Vec::new(),
            humidity_offset: 0.0,
            current_baseline: None,
            baseline_reading_count: 0,
            reading_sequence: 0,
            stable_reading_count: 0,
            in_rapid_change_period: false,
            pre_change_baseline: None,
            baseline_confirmation_count: 0,
            baseline_shifted: false,
            long_term_statistical_offset: 0.0,
            long_term_stable_count: 0,
        }
    }

    /// Expected indoor humidity based on temperature
    /// Indoor environments typically maintain 30-60% RH, with seasonal variations
    fn expected_indoor_humidity(temperature_c: f32) -> f32 {
        // Empirical model for indoor humidity based on temperature
        // Cooler indoor temps tend to have higher relative humidity
        // Warmer indoor temps tend to have lower relative humidity
        let base_humidity = 45.0; // Base indoor humidity target
        let temp_coefficient = -0.5; // RH decreases as temperature increases
        let seasonal_variation = 5.0; // Account for seasonal HVAC differences

        let expected = base_humidity + (25.0 - temperature_c) * temp_coefficient;

        // Clamp to reasonable indoor range with seasonal variation
        expected.clamp(30.0 - seasonal_variation, 60.0 + seasonal_variation)
    }

    /// Detect rapid humidity changes and baseline shifts
    fn detect_rapid_change(&mut self, raw_humidity: f32) -> (bool, f32) {
        self.reading_sequence += 1;

        // Add current reading to recent history
        let current_reading = RecentReading { raw_humidity };

        if self.recent_readings.len() >= CHANGE_HISTORY_SIZE {
            self.recent_readings.remove(0);
        }
        let _ = self.recent_readings.push(current_reading);

        // Need at least 2 readings to detect change
        if self.recent_readings.len() < 2 {
            self.stable_reading_count = 0;
            self.baseline_confirmation_count = 0;
            return (false, 0.0);
        }

        // Calculate change rate over recent readings
        let oldest_reading = self.recent_readings[0].raw_humidity;
        let newest_reading = raw_humidity;
        let total_change = newest_reading - oldest_reading;

        let is_rapid_change = total_change.abs() >= RAPID_CHANGE_THRESHOLD;

        #[allow(clippy::cast_precision_loss)]
        if is_rapid_change {
            // Store baseline before change if not already stored
            if self.pre_change_baseline.is_none() && !self.in_rapid_change_period {
                // Calculate average of recent stable readings as baseline
                let baseline = if self.recent_readings.len() >= 2 {
                    self.recent_readings
                        .iter()
                        .take(self.recent_readings.len() - 1) // Exclude current reading
                        .map(|r| r.raw_humidity)
                        .sum::<f32>()
                        / (self.recent_readings.len() - 1) as f32
                } else {
                    oldest_reading
                };
                self.pre_change_baseline = Some(baseline);
                info!("Stored pre-change baseline: {}%", baseline);
            }

            self.stable_reading_count = 0;
            self.baseline_confirmation_count = 0;
            self.in_rapid_change_period = true;
            self.baseline_shifted = false;

            info!(
                "Rapid humidity change detected: {}% over {} readings (change: {}%)",
                total_change.abs(),
                self.recent_readings.len(),
                total_change
            );
        } else {
            self.stable_reading_count += 1;

            // Check for baseline shift: sustained change from pre-change baseline
            if let Some(baseline) = self.pre_change_baseline {
                let change_from_baseline = raw_humidity - baseline;

                if change_from_baseline.abs() >= BASELINE_SHIFT_THRESHOLD {
                    // Still significantly different from baseline
                    self.baseline_confirmation_count += 1;

                    if self.baseline_confirmation_count >= BASELINE_SHIFT_CONFIRMATION_READINGS {
                        // Confirmed baseline shift - this is the new normal
                        self.baseline_shifted = true;
                        info!(
                            "Baseline shift confirmed: {}% -> {}% (change: {}%, confirmed over {} readings)",
                            baseline, raw_humidity, change_from_baseline, self.baseline_confirmation_count
                        );
                    }
                } else {
                    // Returned close to original baseline
                    self.baseline_confirmation_count = 0;
                    if self.stable_reading_count >= MIN_STABLE_READINGS_AFTER_RAPID_CHANGE {
                        // Back to normal - establish new baseline from current level
                        self.in_rapid_change_period = false;
                        self.current_baseline = Some(raw_humidity); // Update baseline to current stable level
                        self.baseline_reading_count = INITIAL_BASELINE_READINGS; // Mark as established
                        self.pre_change_baseline = None;
                        self.baseline_shifted = false;
                        self.humidity_offset = 0.0; // Reset drift correction for new baseline
                        info!(
                            "Returned to stable level - established new baseline at {}% and reset drift correction",
                            raw_humidity
                        );
                    }
                }
            } else {
                // No baseline stored, normal stability check
                if self.stable_reading_count >= MIN_STABLE_READINGS_AFTER_RAPID_CHANGE {
                    self.in_rapid_change_period = false;
                }
            }
        }

        (is_rapid_change, total_change)
    }

    /// Handle rapid change detection and response
    fn handle_rapid_change(&mut self, raw_humidity: f32) -> bool {
        let (is_rapid_change, _change_magnitude) = self.detect_rapid_change(raw_humidity);

        if is_rapid_change {
            info!("Humidity calibration: Rapid change detected - will establish new baseline after stabilization");
            self.reset_calibration_for_rapid_change();
            return true;
        }

        // If we're still in a rapid change period, wait for stabilization
        if self.in_rapid_change_period {
            if self.baseline_shifted {
                info!(
                    "Humidity calibration: Baseline shift detected - waiting for stabilization to establish new baseline"
                );
            } else {
                info!("Humidity calibration: Still in rapid change period - waiting for stabilization");
            }
            return true;
        }

        false
    }

    /// Reset calibration state when rapid change is detected
    const fn reset_calibration_for_rapid_change(&mut self) {
        self.current_baseline = None; // Reset baseline to be re-established
        self.baseline_reading_count = 0;
        self.humidity_offset = 0.0; // Reset drift offset
    }

    /// Establish baseline from initial stable readings
    fn update_baseline_establishment(&mut self, raw_humidity: f32) -> bool {
        if self.baseline_reading_count >= INITIAL_BASELINE_READINGS {
            return false; // Baseline already established
        }

        if self.current_baseline.is_none() {
            self.current_baseline = Some(raw_humidity);
            info!(
                "Humidity calibration: Establishing new baseline starting with {}%",
                raw_humidity
            );
        } else {
            // Update baseline as running average of initial readings
            if let Some(current_baseline) = self.current_baseline {
                #[allow(clippy::cast_precision_loss)]
                let new_baseline = (current_baseline * self.baseline_reading_count as f32 + raw_humidity)
                    / (self.baseline_reading_count + 1) as f32;
                self.current_baseline = Some(new_baseline);
                info!(
                    "Humidity calibration: Updating baseline {} -> {} (reading {}/{})",
                    current_baseline,
                    new_baseline,
                    self.baseline_reading_count + 1,
                    INITIAL_BASELINE_READINGS
                );
            }
        }

        self.baseline_reading_count += 1;

        if self.baseline_reading_count >= INITIAL_BASELINE_READINGS {
            info!(
                "Humidity calibration: Baseline established at {}% from {} initial readings",
                self.current_baseline.unwrap(),
                INITIAL_BASELINE_READINGS
            );
        }

        true // Still establishing baseline
    }

    /// Update long-term stable reading count
    fn update_long_term_stability(&mut self, drift: f32) {
        if !self.in_rapid_change_period && drift.abs() < RAPID_CHANGE_THRESHOLD {
            self.long_term_stable_count += 1;
        } else {
            self.long_term_stable_count = 0; // Reset if we have rapid changes
        }
    }

    /// Apply long-term statistical drift correction
    fn apply_long_term_drift_correction(&mut self, temperature: f32, raw_humidity: f32) {
        if self.long_term_stable_count < MIN_READINGS_FOR_LONG_TERM_DRIFT {
            return;
        }

        let expected = Self::expected_indoor_humidity(temperature);
        let statistical_error = raw_humidity - expected;

        if statistical_error.abs() >= LONG_TERM_DRIFT_THRESHOLD {
            let old_statistical_offset = self.long_term_statistical_offset;
            self.long_term_statistical_offset = self.long_term_statistical_offset
                * (1.0 - LONG_TERM_DRIFT_LEARNING_RATE)
                + (-statistical_error) * LONG_TERM_DRIFT_LEARNING_RATE;

            info!(
                "Long-term statistical drift correction - expected={}%, reading={}%, error={}%, statistical offset {} -> {} (change: {})",
                expected,
                raw_humidity,
                statistical_error,
                old_statistical_offset,
                self.long_term_statistical_offset,
                self.long_term_statistical_offset - old_statistical_offset
            );
        }
    }

    /// Apply short-term baseline drift correction
    fn apply_baseline_drift_correction(&mut self, baseline: f32, raw_humidity: f32) {
        let drift = raw_humidity - baseline;

        if drift.abs() >= MIN_DRIFT_THRESHOLD {
            // Very gradual drift correction
            let old_offset = self.humidity_offset;
            self.humidity_offset = self.humidity_offset * (1.0 - DRIFT_LEARNING_RATE) + (-drift) * DRIFT_LEARNING_RATE;

            info!(
                "Humidity calibration: Gradual drift correction - baseline={}%, reading={}%, drift={}%, offset {} -> {} (change: {})",
                baseline,
                raw_humidity,
                drift,
                old_offset,
                self.humidity_offset,
                self.humidity_offset - old_offset
            );
        } else {
            info!(
                "Humidity calibration: Reading {}% within drift threshold of baseline {}% (drift: {}% < {}%)",
                raw_humidity, baseline, drift, MIN_DRIFT_THRESHOLD
            );
        }
    }

    /// Add a new humidity measurement for adaptive baseline calibration
    pub fn add_measurement(&mut self, temperature: f32, raw_humidity: f32) {
        // First, detect if this is a rapid change
        let (_, change_magnitude) = self.detect_rapid_change(raw_humidity);

        info!(
            "Humidity calibration: T={}°C, RH={}%, Rapid change={}, Change={}%, Stable readings={}, Baseline readings={}",
            temperature,
            raw_humidity,
            self.in_rapid_change_period,
            change_magnitude,
            self.stable_reading_count,
            self.baseline_reading_count
        );

        // Handle rapid changes and check if we should continue processing
        if self.handle_rapid_change(raw_humidity) {
            return;
        }

        // Establish baseline from initial stable readings
        if self.update_baseline_establishment(raw_humidity) {
            return;
        }

        // After baseline is established, check for both short-term drift and long-term statistical drift
        if let Some(baseline) = self.current_baseline {
            let drift = raw_humidity - baseline;

            // Track stable readings for long-term analysis
            self.update_long_term_stability(drift);

            // Check for long-term statistical drift (independent of baseline drift)
            self.apply_long_term_drift_correction(temperature, raw_humidity);

            // Apply short-term baseline drift correction
            self.apply_baseline_drift_correction(baseline, raw_humidity);
        }
    }

    /// Apply calibration to a humidity reading
    /// Uses hybrid approach: adaptive baseline for rapid changes + statistical expectation for long-term drift
    pub fn calibrate_humidity(&self, _temperature: f32, raw_humidity: f32) -> f32 {
        // During initial baseline establishment, return raw values
        if self.baseline_reading_count < INITIAL_BASELINE_READINGS {
            info!(
                "Humidity calibration: Establishing baseline ({}/{}) - returning raw value {}%",
                self.baseline_reading_count, INITIAL_BASELINE_READINGS, raw_humidity
            );
            return raw_humidity;
        }

        // Apply both baseline drift correction and long-term statistical drift correction
        let baseline_corrected = raw_humidity + self.humidity_offset;
        let fully_corrected = baseline_corrected + self.long_term_statistical_offset;
        let final_value = fully_corrected.clamp(0.0, 100.0);

        let was_clamped = (fully_corrected - final_value).abs() > f32::EPSILON;
        let status = if self.in_rapid_change_period {
            "RAPID_CHANGE"
        } else if self.baseline_reading_count < INITIAL_BASELINE_READINGS {
            "ESTABLISHING_BASELINE"
        } else {
            "HYBRID_DRIFT_CORRECTION"
        };

        info!(
            "Humidity calibration: {} - Applied baseline offset {} + statistical offset {} to {}% -> {}%{}",
            status,
            self.humidity_offset,
            self.long_term_statistical_offset,
            raw_humidity,
            final_value,
            if was_clamped { " (clamped)" } else { "" }
        );

        final_value
    }

    /// Get calibration status information
    pub const fn get_calibration_info(&self) -> (bool, f32, f32, usize, bool, usize) {
        let is_calibrated = self.baseline_reading_count >= INITIAL_BASELINE_READINGS;
        (
            is_calibrated,
            self.humidity_offset,
            self.long_term_statistical_offset,
            self.baseline_reading_count,
            self.in_rapid_change_period,
            self.long_term_stable_count,
        )
    }
}
