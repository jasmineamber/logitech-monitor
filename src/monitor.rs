use std::{
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use tokio::sync::mpsc::{self, Receiver};

use crate::{
    battery,
    battery::{BatteryStatus, DeviceSnapshot},
    config::Config,
};

#[derive(Debug)]
pub(crate) enum MonitorCommand {
    UpdateConfig(Config),
    Shutdown,
}

#[derive(Debug)]
pub(crate) enum MonitorEvent {
    Devices {
        devices: Vec<DeviceSnapshot>,
        selected_device: Option<String>,
        alert: Option<Alert>,
    },
    ScanFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Alert {
    pub(crate) device_name: String,
    pub(crate) percentage: u8,
}

#[derive(Debug, Default)]
struct AlertTracker {
    device_id: Option<String>,
    last_notified_at: Option<Instant>,
}

impl AlertTracker {
    fn evaluate(
        &mut self,
        device: Option<&DeviceSnapshot>,
        threshold: u8,
        repeat_interval: Duration,
        now: Instant,
    ) -> Option<Alert> {
        let device = device?;

        if self.device_id.as_deref() != Some(device.id.as_str()) {
            self.device_id = Some(device.id.clone());
            self.last_notified_at = None;
        }

        if !device.online {
            return None;
        }

        let battery = device.battery?;

        if battery.percentage > threshold {
            self.last_notified_at = None;
            return None;
        }

        if battery.status != BatteryStatus::Discharging {
            return None;
        }

        let should_notify = self
            .last_notified_at
            .is_none_or(|last_notified_at| now.duration_since(last_notified_at) >= repeat_interval);
        if !should_notify {
            return None;
        }

        self.last_notified_at = Some(now);
        Some(Alert {
            device_name: device.name.clone(),
            percentage: battery.percentage,
        })
    }
}

pub(crate) fn spawn(
    config: Config,
    event_sender: Sender<MonitorEvent>,
) -> mpsc::Sender<MonitorCommand> {
    let (command_sender, command_receiver) = mpsc::channel(8);

    std::thread::Builder::new()
        .name("battery-monitor".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("battery monitor runtime should build");
            runtime.block_on(run(config, command_receiver, event_sender));
        })
        .expect("battery monitor thread should start");

    command_sender
}

async fn run(
    mut config: Config,
    mut commands: Receiver<MonitorCommand>,
    event_sender: Sender<MonitorEvent>,
) {
    let mut tracker = AlertTracker::default();
    let mut interval = poll_interval(&config);

    loop {
        tokio::select! {
            Some(command) = commands.recv() => match command {
                MonitorCommand::UpdateConfig(next_config) => {
                    interval = poll_interval(&next_config);
                    config = next_config;
                }
                MonitorCommand::Shutdown => break,
            },
            _ = interval.tick() => {
                match battery::enumerate().await {
                    Ok(devices) => {
                        let selected_device = config.selected_device.clone().or_else(|| {
                            devices.iter().find(|device| device.online).map(|device| device.id.clone())
                        });
                        let selected = selected_device.as_deref().and_then(|id| {
                            devices.iter().find(|device| device.id == id)
                        });
                        let alert = tracker.evaluate(
                            selected,
                            config.threshold,
                            repeat_alert_interval(&config),
                            Instant::now(),
                        );

                        let _ = event_sender.send(MonitorEvent::Devices {
                            devices,
                            selected_device,
                            alert,
                        });
                    }
                    Err(error) => {
                        let _ = event_sender.send(MonitorEvent::ScanFailed(error.to_string()));
                    }
                }
            }
        }
    }
}

fn poll_interval(config: &Config) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(Duration::from_secs(config.poll_interval_seconds));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval
}

fn repeat_alert_interval(config: &Config) -> Duration {
    Duration::from_secs(config.repeat_alert_interval_minutes * 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::{BatteryInfo, BatteryLevel};

    const THRESHOLD: u8 = 20;
    const REPEAT_INTERVAL: Duration = Duration::from_secs(300);

    fn device(id: &str, percentage: u8, status: BatteryStatus) -> DeviceSnapshot {
        DeviceSnapshot {
            id: id.to_string(),
            name: "Test device".to_string(),
            online: true,
            battery: Some(BatteryInfo {
                percentage,
                level: BatteryLevel::Low,
                status,
            }),
        }
    }

    #[test]
    fn initial_low_battery_alerts_and_repeats_after_interval() {
        let mut tracker = AlertTracker::default();
        let low = device("one", 20, BatteryStatus::Discharging);
        let start = Instant::now();

        assert!(
            tracker
                .evaluate(Some(&low), THRESHOLD, REPEAT_INTERVAL, start)
                .is_some()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&low),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + Duration::from_secs(299),
                )
                .is_none()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&low),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .is_some()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&low),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL + Duration::from_secs(1),
                )
                .is_none()
        );
    }

    #[test]
    fn alert_rearms_only_after_recovery_above_threshold() {
        let mut tracker = AlertTracker::default();
        let low = device("one", 10, BatteryStatus::Discharging);
        let charging = device("one", 10, BatteryStatus::Charging);
        let recovered = device("one", 21, BatteryStatus::Discharging);
        let start = Instant::now();

        assert!(
            tracker
                .evaluate(Some(&low), THRESHOLD, REPEAT_INTERVAL, start)
                .is_some()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&charging),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .is_none()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&charging),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL + Duration::from_secs(1),
                )
                .is_none()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&recovered),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL + Duration::from_secs(2),
                )
                .is_none()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&low),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL + Duration::from_secs(3),
                )
                .is_some()
        );
    }

    #[test]
    fn charging_unknown_and_offline_devices_do_not_alert() {
        let mut tracker = AlertTracker::default();
        let charging = device("one", 5, BatteryStatus::Charging);
        let unknown = device("one", 5, BatteryStatus::Unknown);
        let mut offline = device("one", 5, BatteryStatus::Discharging);
        offline.online = false;
        let start = Instant::now();

        assert!(
            tracker
                .evaluate(Some(&charging), THRESHOLD, REPEAT_INTERVAL, start)
                .is_none()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&unknown),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .is_none()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&offline),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .is_none()
        );
    }

    #[test]
    fn reconnecting_after_interval_repeats_alert() {
        let mut tracker = AlertTracker::default();
        let low = device("one", 5, BatteryStatus::Discharging);
        let mut offline = low.clone();
        offline.online = false;
        let start = Instant::now();

        assert!(
            tracker
                .evaluate(Some(&low), THRESHOLD, REPEAT_INTERVAL, start)
                .is_some()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&offline),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .is_none()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&low),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .is_some()
        );
    }

    #[test]
    fn changing_device_starts_a_new_alert_cycle() {
        let mut tracker = AlertTracker::default();
        let first = device("one", 10, BatteryStatus::Discharging);
        let second = device("two", 10, BatteryStatus::Discharging);
        let start = Instant::now();

        assert!(
            tracker
                .evaluate(Some(&first), THRESHOLD, REPEAT_INTERVAL, start)
                .is_some()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&second),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + Duration::from_secs(1),
                )
                .is_some()
        );
    }

    #[test]
    fn changing_interval_applies_to_existing_alert_cycle() {
        let mut tracker = AlertTracker::default();
        let low = device("one", 10, BatteryStatus::Discharging);
        let start = Instant::now();

        assert!(
            tracker
                .evaluate(Some(&low), THRESHOLD, REPEAT_INTERVAL, start)
                .is_some()
        );
        assert!(
            tracker
                .evaluate(
                    Some(&low),
                    THRESHOLD,
                    Duration::from_secs(60),
                    start + Duration::from_secs(60),
                )
                .is_some()
        );
    }
}
