use defmt::info;
use embassy_rp::watchdog::Watchdog;

#[embassy_executor::task]
pub async fn watchdog_task() {
    // Initialize the watchdog device
    let mut watchdog = Watchdog::new().await.unwrap();

    // Configure the watchdog
    watchdog.set_timeout(5000).await.unwrap(); // Set timeout to 5 seconds

    loop {
        // Feed the watchdog to prevent it from resetting the system
        watchdog.feed().await.unwrap();
        info!("Watchdog fed");

        // Wait for a while before feeding again
        Timer::after_millis(4000).await; // Feed every 4 seconds
    }
}
