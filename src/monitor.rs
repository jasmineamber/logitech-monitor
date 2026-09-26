use std::{
    collections::{HashMap, HashSet},
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
        alerts: Vec<Alert>,
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
    states: HashMap<String, AlertState>,
    monitored_devices: HashSet<String>,
}

#[derive(Debug, Default)]
struct AlertState {
    last_notified_at: Option<Instant>,
}

impl AlertTracker {
    fn evaluate(
        &mut self,
        devices: &[DeviceSnapshot],
        monitored_devices: &[String],
        threshold: u8,
        repeat_interval: Duration,
        now: Instant,
    ) -> Vec<Alert> {
        let monitored_ids = monitored_devices.iter().cloned().collect::<HashSet<_>>();
        self.states.retain(|id, _| monitored_ids.contains(id));
        self.monitored_devices = monitored_ids;

        devices
            .iter()
            .filter(|device| self.monitored_devices.contains(&device.id))
            .filter_map(|device| {
                let state = self.states.entry(device.id.clone()).or_default();

                if !device.online {
                    return None;
                }

                let battery = device.battery?;

                if battery.percentage > threshold {
                    state.last_notified_at = None;
                    return None;
                }

                if battery.status != BatteryStatus::Discharging {
                    return None;
                }

                let should_notify = state.last_notified_at.is_none_or(|last_notified_at| {
                    now.duration_since(last_notified_at) >= repeat_interval
                });
                if !should_notify {
                    return None;
                }

                state.last_notified_at = Some(now);
                Some(Alert {
                    device_name: device.name.clone(),
                    percentage: battery.percentage,
                })
            })
            .collect()
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
                        let alerts = tracker.evaluate(
                            &devices,
                            &config.monitored_devices,
                            config.threshold,
                            repeat_alert_interval(&config),
                            Instant::now(),
                        );

                        let _ = event_sender.send(MonitorEvent::Devices {
                            devices,
                            alerts,
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
            name: format!("Test device {id}"),
            online: true,
            battery: Some(BatteryInfo {
                percentage,
                level: BatteryLevel::Low,
                status,
            }),
        }
    }

    fn monitored(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn multiple_devices_alert_independently() {
        let mut tracker = AlertTracker::default();
        let devices = vec![
            device("one", 20, BatteryStatus::Discharging),
            device("two", 10, BatteryStatus::Discharging),
        ];
        let start = Instant::now();

        let alerts = tracker.evaluate(
            &devices,
            &monitored(&["one", "two"]),
            THRESHOLD,
            REPEAT_INTERVAL,
            start,
        );

        assert_eq!(alerts.len(), 2);
        assert_eq!(alerts[0].device_name, "Test device one");
        assert_eq!(alerts[1].device_name, "Test device two");
    }

    #[test]
    fn each_device_has_its_own_repeat_interval() {
        let mut tracker = AlertTracker::default();
        let devices = vec![
            device("one", 10, BatteryStatus::Discharging),
            device("two", 10, BatteryStatus::Discharging),
        ];
        let start = Instant::now();

        assert_eq!(
            tracker
                .evaluate(
                    &devices,
                    &monitored(&["one", "two"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start,
                )
                .len(),
            2
        );
        assert!(
            tracker
                .evaluate(
                    &devices,
                    &monitored(&["one", "two"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + Duration::from_secs(299),
                )
                .is_empty()
        );
        assert_eq!(
            tracker
                .evaluate(
                    &devices,
                    &monitored(&["one", "two"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .len(),
            2
        );
    }

    #[test]
    fn alert_rearms_only_after_the_same_device_recovers() {
        let mut tracker = AlertTracker::default();
        let low_one = device("one", 10, BatteryStatus::Discharging);
        let low_two = device("two", 10, BatteryStatus::Discharging);
        let recovered_one = device("one", 21, BatteryStatus::Discharging);
        let start = Instant::now();

        let devices = vec![low_one.clone(), low_two.clone()];
        assert_eq!(
            tracker
                .evaluate(
                    &devices,
                    &monitored(&["one", "two"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start,
                )
                .len(),
            2
        );

        let devices = vec![recovered_one, low_two.clone()];
        assert!(
            tracker
                .evaluate(
                    &devices,
                    &monitored(&["one", "two"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + Duration::from_secs(1),
                )
                .is_empty()
        );

        let devices = vec![low_one, low_two];
        let alerts = tracker.evaluate(
            &devices,
            &monitored(&["one", "two"]),
            THRESHOLD,
            REPEAT_INTERVAL,
            start + Duration::from_secs(2),
        );
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].device_name, "Test device one");
    }

    #[test]
    fn charging_unknown_and_offline_devices_do_not_alert() {
        let mut tracker = AlertTracker::default();
        let charging = device("charging", 5, BatteryStatus::Charging);
        let unknown = device("unknown", 5, BatteryStatus::Unknown);
        let mut offline = device("offline", 5, BatteryStatus::Discharging);
        offline.online = false;

        let devices = vec![charging, unknown, offline];
        assert!(
            tracker
                .evaluate(
                    &devices,
                    &monitored(&["charging", "unknown", "offline"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    Instant::now(),
                )
                .is_empty()
        );
    }

    #[test]
    fn reconnecting_after_interval_repeats_alert() {
        let mut tracker = AlertTracker::default();
        let low = device("one", 5, BatteryStatus::Discharging);
        let mut offline = low.clone();
        offline.online = false;
        let start = Instant::now();

        assert_eq!(
            tracker
                .evaluate(
                    &[low.clone()],
                    &monitored(&["one"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start,
                )
                .len(),
            1
        );
        assert!(
            tracker
                .evaluate(
                    &[offline],
                    &monitored(&["one"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .is_empty()
        );
        assert_eq!(
            tracker
                .evaluate(
                    &[low],
                    &monitored(&["one"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + REPEAT_INTERVAL,
                )
                .len(),
            1
        );
    }

    #[test]
    fn unselected_devices_do_not_alert_and_reselection_starts_a_cycle() {
        let mut tracker = AlertTracker::default();
        let one = device("one", 10, BatteryStatus::Discharging);
        let two = device("two", 10, BatteryStatus::Discharging);
        let start = Instant::now();

        assert!(
            tracker
                .evaluate(
                    &[one.clone(), two.clone()],
                    &monitored(&["one"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start,
                )
                .iter()
                .all(|alert| alert.device_name == "Test device one")
        );
        assert!(
            tracker
                .evaluate(
                    &[one.clone(), two.clone()],
                    &[],
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + Duration::from_secs(1),
                )
                .is_empty()
        );
        assert_eq!(
            tracker
                .evaluate(
                    &[one, two],
                    &monitored(&["two"]),
                    THRESHOLD,
                    REPEAT_INTERVAL,
                    start + Duration::from_secs(2),
                )
                .len(),
            1
        );
    }
}
