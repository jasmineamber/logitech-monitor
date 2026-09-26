#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod battery;
mod config;
mod monitor;

#[cfg(target_os = "windows")]
mod mchose;

#[cfg(target_os = "windows")]
mod app_identity;

#[cfg(target_os = "windows")]
mod autostart;
#[cfg(target_os = "windows")]
mod tray;
#[cfg(target_os = "windows")]
mod visual;

use anyhow::{Context, Result};
use battery::{BatteryInfo, BatteryLevel, BatteryStatus};
use clap::{Parser, Subcommand};

const APP_DISPLAY_NAME: &str = "电量管家";
const APP_USER_MODEL_ID: &str = "BatteryMonitor.App";

#[derive(Debug, Parser)]
#[command(name = "battery-monitor", about = "Read supported HID device status")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List supported HID devices and their battery status.
    Devices,
    /// Send a Windows Toast notification to verify notifications work.
    NotifyTest,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        None => run_tray(),
        Some(Command::Devices) => list_devices().await,
        Some(Command::NotifyTest) => send_test_notification(),
    }
}

fn run_tray() -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let instance = app_identity::SingleInstance::acquire()?;
        if instance.is_already_running() {
            return Ok(());
        }

        app_identity::set_current_process_identity()?;
        tray::run()
    }

    #[cfg(not(target_os = "windows"))]
    anyhow::bail!("battery-monitor tray mode currently supports Windows only")
}

async fn list_devices() -> Result<()> {
    let devices = battery::enumerate().await.context("读取设备信息失败")?;

    if devices.is_empty() {
        println!("未找到兼容 HID 设备。");
        return Ok(());
    }

    for (index, device) in devices.iter().enumerate() {
        if index > 0 {
            println!();
        }

        let connection = if device.online { "在线" } else { "离线" };

        println!("设备：{}", device.name);
        println!("  连接状态：{connection}");
        println!("  电池：{}", format_battery(device.battery.as_ref()));
    }

    Ok(())
}

fn format_battery(battery: Option<&BatteryInfo>) -> String {
    let Some(battery) = battery else {
        return "未知".to_string();
    };

    format!(
        "{}% ({}, {})",
        battery.percentage,
        battery_level_name(battery.level),
        battery_status_name(battery.status)
    )
}

fn battery_level_name(level: BatteryLevel) -> &'static str {
    match level {
        BatteryLevel::Critical => "严重",
        BatteryLevel::Low => "低",
        BatteryLevel::Good => "良好",
        BatteryLevel::Full => "满电",
        BatteryLevel::Unknown => "未知",
    }
}

fn battery_status_name(status: BatteryStatus) -> &'static str {
    match status {
        BatteryStatus::Discharging => "放电中",
        BatteryStatus::Charging => "充电中",
        BatteryStatus::ChargingSlow => "慢速充电",
        BatteryStatus::Full => "已充满",
        BatteryStatus::Error => "错误",
        BatteryStatus::Unknown => "未知",
    }
}

#[cfg(target_os = "windows")]
fn send_test_notification() -> Result<()> {
    app_identity::set_current_process_identity()?;
    app_identity::ensure_notification_shortcut().context("注册 Windows 通知应用身份失败")?;
    notify_rust::Notification::new()
        .summary(APP_DISPLAY_NAME)
        .app_id(APP_USER_MODEL_ID)
        .body("Windows 通知测试成功。")
        .show()
        .context("发送 Windows 通知失败")?;
    println!("测试通知已发送。");
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn send_test_notification() -> Result<()> {
    anyhow::bail!("battery-monitor currently supports notifications on Windows only")
}

#[cfg(test)]
mod tests {
    use super::battery::{BatteryInfo, BatteryLevel, BatteryStatus};
    use super::format_battery;

    #[test]
    fn missing_battery_is_reported_as_unknown() {
        assert_eq!(format_battery(None), "未知");
    }

    #[test]
    fn battery_details_include_percentage_level_and_status() {
        let battery = BatteryInfo {
            percentage: 42,
            level: BatteryLevel::Low,
            status: BatteryStatus::Discharging,
        };

        assert_eq!(format_battery(Some(&battery)), "42% (低, 放电中)");
    }
}
