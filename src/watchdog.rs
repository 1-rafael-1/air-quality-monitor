//! Watchdog task to reset the system if it stops being fed
use defmt::info;
use embassy_rp::{Peri, peripherals::WATCHDOG, watchdog::Watchdog};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Timer};

/// How long the watchdog will wait before resetting the system
const WATCHDOG_TIMEOUT: Duration = Duration::from_millis(10000);
/// How often the watchdog should be fed to prevent a reset
const WATCHDOG_FEED_INTERVAL: Duration = Duration::from_millis(1000);

/// Signal to stop feeding the watchdog (causes system reset)
pub static WATCHDOG_STOP: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Signal the watchdog to stop feeding, causing a system reset
pub fn trigger_watchdog_reset() {
    WATCHDOG_STOP.signal(());
}

#[embassy_executor::task]
pub async fn watchdog_task(wd: Peri<'static, WATCHDOG>) {
    // Initialize the watchdog device
    let mut watchdog = Watchdog::new(wd);
    watchdog.pause_on_debug(true);
    watchdog.start(WATCHDOG_TIMEOUT);

    info!("Watchdog started with {}ms timeout", WATCHDOG_TIMEOUT.as_millis());

    loop {
        // Check if we should stop feeding the watchdog
        if WATCHDOG_STOP.signaled() {
            info!("Watchdog stop signal received - system will reset");
            // Stop feeding the watchdog, which will cause a reset after the timeout
            break;
        }

        // Feed the watchdog to prevent it from resetting the system
        watchdog.feed();

        // Wait before feeding again
        Timer::after(WATCHDOG_FEED_INTERVAL).await;
    }

    // If we reach here, we stop feeding and the watchdog will reset the system
    info!("Watchdog feeding stopped - waiting for reset...");
    loop {
        Timer::after_secs(1).await;
    }
}
