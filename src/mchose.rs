use anyhow::{Context, Result, anyhow};
use hidapi::{HidApi, HidDevice};

use crate::battery::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceSnapshot};

const VENDOR_ID: u16 = 0x3554;
const PRODUCT_ID: u16 = 0xfa09;
const USAGE_PAGE: u16 = 0xff02;
const USAGE: u16 = 0x02;
const REPORT_ID: u8 = 0x13;
const RESPONSE_TIMEOUT_MS: i32 = 1_000;
const RESPONSE_ATTEMPTS: usize = 3;

pub(crate) fn enumerate() -> Result<Vec<DeviceSnapshot>> {
    let api = HidApi::new().context("初始化 MCHOSE HID 失败")?;
    let mut devices = Vec::new();

    for info in api.device_list().filter(|info| {
        info.vendor_id() == VENDOR_ID
            && info.product_id() == PRODUCT_ID
            && info.usage_page() == USAGE_PAGE
            && info.usage() == USAGE
    }) {
        let id = stable_device_id(info.path().to_string_lossy().as_ref());
        let name = info
            .product_string()
            .filter(|name| !name.trim().is_empty())
            .map(|name| format!("G75 Pro ({name})"))
            .unwrap_or_else(|| "G75 Pro".to_string());

        let (online, battery) = match info.open_device(&api) {
            Ok(device) => (true, read_battery(&device).ok()),
            Err(_) => (false, None),
        };

        devices.push(DeviceSnapshot {
            id,
            name,
            online,
            battery,
        });
    }

    Ok(devices)
}

fn stable_device_id(path: &str) -> String {
    format!("mchose:{VENDOR_ID:04x}:{PRODUCT_ID:04x}:{path}")
}

fn read_battery(device: &HidDevice) -> Result<BatteryInfo> {
    let command = battery_command();
    device
        .write(&command)
        .context("发送 MCHOSE 电量读取命令失败")?;

    let mut response = [0_u8; 64];
    let mut last_error = None;
    for _ in 0..RESPONSE_ATTEMPTS {
        let length = device
            .read_timeout(&mut response, RESPONSE_TIMEOUT_MS)
            .context("读取 MCHOSE 电量响应失败")?;
        if length == 0 {
            continue;
        }

        match parse_battery_response(&response[..length]) {
            Ok(battery) => return Ok(battery),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("MCHOSE 电量响应超时")))
}

fn battery_command() -> Vec<u8> {
    let mut command = vec![REPORT_ID, 0x4a, 0x01];
    command.extend(std::iter::repeat_n(0, 16));
    let checksum = command[..]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    command.push(checksum);
    command
}

fn parse_battery_response(response: &[u8]) -> Result<BatteryInfo> {
    let response = response.strip_prefix(&[REPORT_ID]).unwrap_or(response);
    if response.len() < 6 {
        return Err(anyhow!("MCHOSE 电量响应长度不足"));
    }
    if response[0] != 0x4a {
        return Err(anyhow!("MCHOSE 电量响应命令不匹配：{:02x}", response[0]));
    }

    // Wireless responses contain a four-byte packet header and a trailing checksum.
    let battery = response[4];
    if battery > 100 {
        return Err(anyhow!("MCHOSE 电量值无效：{battery}"));
    }

    let status = response[5];
    let battery_status = if status & 0x10 != 0 {
        BatteryStatus::Charging
    } else {
        BatteryStatus::Discharging
    };

    Ok(BatteryInfo {
        percentage: battery,
        level: battery_level(battery),
        status: battery_status,
    })
}

fn battery_level(percentage: u8) -> BatteryLevel {
    match percentage {
        0..=10 => BatteryLevel::Critical,
        11..=20 => BatteryLevel::Low,
        21..=99 => BatteryLevel::Good,
        100 => BatteryLevel::Full,
        _ => BatteryLevel::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_command_matches_webhid_command() {
        let command = battery_command();

        assert_eq!(command.len(), 20);
        assert_eq!(&command[..3], &[0x13, 0x4a, 0x01]);
        assert_eq!(command[19], 0x5e);
    }

    #[test]
    fn parses_discharging_battery_response() {
        let battery = parse_battery_response(&[0x13, 0x4a, 0x01, 0x00, 0x00, 15, 0x00, 0x00])
            .expect("response should parse");

        assert_eq!(battery.percentage, 15);
        assert_eq!(battery.level, BatteryLevel::Low);
        assert_eq!(battery.status, BatteryStatus::Discharging);
    }

    #[test]
    fn charging_status_is_independent_of_battery_level() {
        let charging = parse_battery_response(&[0x4a, 0x01, 0x00, 0x00, 25, 0x10, 0x00])
            .expect("charging response should parse");
        let discharging = parse_battery_response(&[0x4a, 0x01, 0x00, 0x00, 25, 0x00, 0x00])
            .expect("discharging response should parse");
        let full_flag_without_charging =
            parse_battery_response(&[0x4a, 0x01, 0x00, 0x00, 100, 0x01, 0x00])
                .expect("full response should parse");
        let full_without_charging =
            parse_battery_response(&[0x4a, 0x01, 0x00, 0x00, 100, 0x00, 0x00])
                .expect("full discharging response should parse");
        let low_without_charging = parse_battery_response(&[0x4a, 0x01, 0x00, 0x00, 5, 0x00, 0x00])
            .expect("low battery response should parse");

        assert_eq!(charging.status, BatteryStatus::Charging);
        assert_eq!(charging.level, BatteryLevel::Good);
        assert_eq!(discharging.status, BatteryStatus::Discharging);
        assert_eq!(
            full_flag_without_charging.status,
            BatteryStatus::Discharging
        );
        assert_eq!(full_flag_without_charging.level, BatteryLevel::Full);
        assert_eq!(full_without_charging.status, BatteryStatus::Discharging);
        assert_eq!(full_without_charging.level, BatteryLevel::Full);
        assert_eq!(low_without_charging.status, BatteryStatus::Discharging);
        assert_eq!(low_without_charging.level, BatteryLevel::Critical);
    }

    #[test]
    fn rejects_short_and_invalid_responses() {
        assert!(parse_battery_response(&[0; 5]).is_err());
        assert!(parse_battery_response(&[0x49, 0x01, 0x00, 0x00, 20, 0x00]).is_err());
        assert!(parse_battery_response(&[0x4a, 0x01, 0x00, 0x00, 101, 0x00, 0x00]).is_err());
    }
}
