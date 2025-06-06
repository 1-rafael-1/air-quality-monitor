use embassy_rp::{Peri, peripherals::WATCHDOG, watchdog::Watchdog};
use embassy_time::{Duration, Timer};

static COOLDOWN: Duration = Duration::from_millis(10000);

#[embassy_executor::task]
pub async fn watchdog_task(wd: Peri<'static, WATCHDOG>) {
    // Initialize the watchdog device
    let mut watchdog = Watchdog::new(wd);
    watchdog.pause_on_debug(true);
    watchdog.start(COOLDOWN);

    //ToDo: Implement a mechanism to handle the watchdog reset condition
    loop {
        // Feed the watchdog to prevent it from resetting the system
        watchdog.feed();
        // info!("Watchdog tick");

        // Wait for a while before feeding again
        Timer::after_millis(1000).await; // Feed every 4 seconds
    }
}
