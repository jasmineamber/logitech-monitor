use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_THRESHOLD: u8 = 20;
pub(crate) const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 60;
pub(crate) const DEFAULT_REPEAT_ALERT_INTERVAL_MINUTES: u64 = 5;
pub(crate) const MIN_THRESHOLD: u8 = 1;
pub(crate) const MAX_THRESHOLD: u8 = 99;
pub(crate) const MIN_POLL_INTERVAL_SECONDS: u64 = 15;
pub(crate) const MAX_POLL_INTERVAL_SECONDS: u64 = 3600;
pub(crate) const MIN_REPEAT_ALERT_INTERVAL_MINUTES: u64 = 1;
pub(crate) const MAX_REPEAT_ALERT_INTERVAL_MINUTES: u64 = 1440;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    pub(crate) threshold: u8,
    pub(crate) poll_interval_seconds: u64,
    pub(crate) repeat_alert_interval_minutes: u64,
    pub(crate) selected_device: Option<String>,
    pub(crate) autostart: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            poll_interval_seconds: DEFAULT_POLL_INTERVAL_SECONDS,
            repeat_alert_interval_minutes: DEFAULT_REPEAT_ALERT_INTERVAL_MINUTES,
            selected_device: None,
            autostart: false,
        }
    }
}

impl Config {
    pub(crate) fn load(path: &Path) -> Result<ConfigLoad> {
        match fs::read_to_string(path) {
            Ok(contents) => match toml::from_str::<Self>(&contents) {
                Ok(config) if config.validate().is_ok() => Ok(ConfigLoad {
                    config,
                    warning: None,
                }),
                Ok(_) => Ok(ConfigLoad {
                    config: Self::default(),
                    warning: Some("配置中的数值超出允许范围".to_string()),
                }),
                Err(error) => Ok(ConfigLoad {
                    config: Self::default(),
                    warning: Some(format!("读取配置失败：{error}")),
                }),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ConfigLoad {
                config: Self::default(),
                warning: None,
            }),
            Err(error) => Ok(ConfigLoad {
                config: Self::default(),
                warning: Some(format!("读取配置失败：{error}")),
            }),
        }
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;

        let contents = toml::to_string_pretty(self).context("序列化配置失败")?;
        fs::write(path, contents).with_context(|| format!("写入配置失败：{}", path.display()))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if !(MIN_THRESHOLD..=MAX_THRESHOLD).contains(&self.threshold) {
            bail!("低电量阈值必须在 {MIN_THRESHOLD} 到 {MAX_THRESHOLD} 之间");
        }
        if !(MIN_POLL_INTERVAL_SECONDS..=MAX_POLL_INTERVAL_SECONDS)
            .contains(&self.poll_interval_seconds)
        {
            bail!(
                "轮询间隔必须在 {MIN_POLL_INTERVAL_SECONDS} 到 {MAX_POLL_INTERVAL_SECONDS} 秒之间"
            );
        }
        if !(MIN_REPEAT_ALERT_INTERVAL_MINUTES..=MAX_REPEAT_ALERT_INTERVAL_MINUTES)
            .contains(&self.repeat_alert_interval_minutes)
        {
            bail!(
                "重复提醒间隔必须在 {MIN_REPEAT_ALERT_INTERVAL_MINUTES} 到 {MAX_REPEAT_ALERT_INTERVAL_MINUTES} 分钟之间"
            );
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct ConfigLoad {
    pub(crate) config: Config,
    pub(crate) warning: Option<String>,
}

pub(crate) fn config_path() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("定位程序路径失败")?;
    let directory = executable
        .parent()
        .context("executable has no parent directory")?;
    Ok(directory.join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_documented_values() {
        assert_eq!(Config::default().threshold, 20);
        assert_eq!(Config::default().poll_interval_seconds, 60);
        assert_eq!(Config::default().repeat_alert_interval_minutes, 5);
        assert!(!Config::default().autostart);
    }

    #[test]
    fn valid_config_round_trips_through_toml() {
        let config = Config {
            threshold: 35,
            poll_interval_seconds: 120,
            repeat_alert_interval_minutes: 10,
            selected_device: Some("device-1".to_string()),
            autostart: true,
        };

        let encoded = toml::to_string(&config).expect("config should serialize");
        let decoded: Config = toml::from_str(&encoded).expect("config should deserialize");

        assert_eq!(decoded, config);
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(
            Config {
                threshold: 0,
                ..Config::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                threshold: 100,
                ..Config::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                poll_interval_seconds: 14,
                ..Config::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                poll_interval_seconds: 3601,
                ..Config::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                repeat_alert_interval_minutes: 0,
                ..Config::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                repeat_alert_interval_minutes: 1441,
                ..Config::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn malformed_file_uses_defaults_and_reports_a_warning() {
        let path = std::env::temp_dir().join(format!(
            "logitech-monitor-config-test-{}.toml",
            std::process::id()
        ));
        fs::write(&path, "threshold = 100\n").expect("test config should be writable");

        let loaded = Config::load(&path).expect("config load should not fail");

        assert_eq!(loaded.config, Config::default());
        assert!(loaded.warning.is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn old_config_without_repeat_interval_uses_default() {
        let decoded: Config = toml::from_str(
            "threshold = 35\npoll_interval_seconds = 120\nselected_device = 'device-1'\nautostart = true\n",
        )
        .expect("old config should deserialize");

        assert_eq!(decoded.repeat_alert_interval_minutes, 5);
        assert_eq!(decoded.threshold, 35);
        assert_eq!(decoded.poll_interval_seconds, 120);
    }
}
