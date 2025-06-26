//! Watchdog task to reset the system if it stops being fed
use defmt::{Format, info};
use embassy_rp::{Peri, peripherals::WATCHDOG, watchdog::Watchdog};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant, Timer};

/// How long our custom countdown timer runs before triggering a reset (15 minutes)
const COUNTDOWN_TIMEOUT: Duration = Duration::from_secs(520);
/// How often we check task health and update our countdown
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(60);
/// Hardware watchdog timeout (short, used only for actual reset)
const HARDWARE_WATCHDOG_TIMEOUT: Duration = Duration::from_millis(8000);

/// Task identifiers for health tracking
#[derive(Debug, Clone, Copy, Eq, PartialEq, Format)]
pub enum TaskId {
    /// Sensor task
    Sensor,
    /// Display task
    Display,
    /// VSYS voltage measurement task
    Vsys,
    /// Orchestrator task
    Orchestrator,
    /// Mode switch task
    ModeSwitch,
}

/// Task health tracking
#[derive(Copy, Clone, Format, Debug)]
struct TaskHealth {
    /// Whether this task is currently healthy
    is_healthy: bool,
}

impl TaskHealth {
    /// Create a new `TaskHealth` instance with default unhealthy state
    const fn new() -> Self {
        Self { is_healthy: false }
    }
}

/// System health state with custom countdown timer
struct SystemHealth {
    /// Health status of each task
    tasks: [TaskHealth; 5], // Sensor, Display, Vsys, Orchestrator, ModeSwitch
    /// Whether all tasks are currently healthy
    all_healthy: bool,
    /// Countdown timer - when this expires, we trigger hardware watchdog reset
    countdown_deadline: Option<Instant>,
}

impl SystemHealth {
    /// Create a new `SystemHealth` instance with all tasks unhealthy
    const fn new() -> Self {
        Self {
            tasks: [TaskHealth::new(); 5],
            all_healthy: false,
            countdown_deadline: None,
        }
    }

    /// report a task as succeeded
    const fn set_task_succeeded(&mut self, task_id: TaskId) {
        let index = task_id as usize;
        self.tasks[index].is_healthy = true;
    }

    /// report a task as failed
    const fn set_task_failed(&mut self, task_id: TaskId) {
        let index = task_id as usize;
        self.tasks[index].is_healthy = false;
    }

    /// Update overall health status based on individual task health
    fn update_overall_health(&mut self) {
        let was_all_healthy = self.all_healthy;

        // A task is considered healthy if it has reported success at least once
        self.all_healthy = self.tasks.iter().all(|task| task.is_healthy);

        if self.all_healthy && !was_all_healthy {
            info!("All tasks healthy - resetting countdown timer");
            // Reset countdown when all tasks become healthy
            self.countdown_deadline = Some(Instant::now() + COUNTDOWN_TIMEOUT);
        } else if !self.all_healthy && self.countdown_deadline.is_none() {
            info!("Some tasks unhealthy - countdown timer started");
            // Start countdown when tasks become unhealthy for the first time
            self.countdown_deadline = Some(Instant::now() + COUNTDOWN_TIMEOUT);
        }
    }

    /// Reset the countdown timer (equivalent to feeding the watchdog)
    fn reset_countdown(&mut self) {
        if self.all_healthy {
            self.countdown_deadline = Some(Instant::now() + COUNTDOWN_TIMEOUT);
            info!(
                "Countdown timer reset - {} seconds until reset",
                COUNTDOWN_TIMEOUT.as_secs()
            );
        }
    }

    /// Check if countdown has expired and we should trigger hardware watchdog
    fn should_trigger_reset(&self) -> bool {
        self.countdown_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

/// Global system health tracker
static SYSTEM_HEALTH: Mutex<CriticalSectionRawMutex, SystemHealth> = Mutex::new(SystemHealth::new());

/// Report a successful task iteration
pub async fn report_task_success(task_id: TaskId) {
    let mut health = SYSTEM_HEALTH.lock().await;
    health.set_task_succeeded(task_id);
}

/// Report a failed task iteration
pub async fn report_task_failure(task_id: TaskId) {
    let mut health = SYSTEM_HEALTH.lock().await;
    health.set_task_failed(task_id);
}

#[embassy_executor::task]
pub async fn watchdog_task(wd: Peri<'static, WATCHDOG>) {
    info!(
        "Custom watchdog started with {}s countdown, checking health every {}s",
        COUNTDOWN_TIMEOUT.as_secs(),
        HEALTH_CHECK_INTERVAL.as_secs()
    );

    loop {
        // Check system health and update countdown
        let (all_healthy, should_reset) = {
            let mut health = SYSTEM_HEALTH.lock().await;
            health.update_overall_health();

            // Reset countdown if all tasks are healthy
            if health.all_healthy {
                health.reset_countdown();
                info!("All tasks healthy");
            }

            (health.all_healthy, health.should_trigger_reset())
        };

        if !all_healthy && should_reset {
            info!("Countdown expired - system will reset due to unhealthy tasks");

            // Initialize hardware watchdog and don't feed it - this will cause reset
            let mut watchdog = Watchdog::new(wd);
            watchdog.pause_on_debug(false); // Don't pause during debug - we want the reset
            watchdog.start(HARDWARE_WATCHDOG_TIMEOUT);

            info!(
                "Hardware watchdog started - system will reset in {}ms",
                HARDWARE_WATCHDOG_TIMEOUT.as_millis()
            );

            // Wait for hardware watchdog to reset the system
            loop {
                Timer::after_secs(1).await;
            }
        }

        // Wait before next health check
        Timer::after(HEALTH_CHECK_INTERVAL).await;
    }
}
