use anyhow::{Context, Result};
use openlogi_core::device::{
    BatteryInfo as OpenLogiBatteryInfo, BatteryLevel as OpenLogiBatteryLevel,
    BatteryStatus as OpenLogiBatteryStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceSnapshot {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) online: bool,
    pub(crate) battery: Option<BatteryInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatteryInfo {
    pub(crate) percentage: u8,
    pub(crate) level: BatteryLevel,
    pub(crate) status: BatteryStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatteryLevel {
    Critical,
    Low,
    Good,
    Full,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatteryStatus {
    Discharging,
    Charging,
    ChargingSlow,
    Full,
    Error,
    Unknown,
}

pub(crate) async fn enumerate() -> Result<Vec<DeviceSnapshot>> {
    let inventories = openlogi_hid::enumerate()
        .await
        .context("读取罗技 HID++ 设备失败")?;

    let mut devices = inventories
        .iter()
        .flat_map(|inventory| {
            inventory.paired.iter().map(move |device| DeviceSnapshot {
                id: stable_device_id(
                    inventory.receiver.unique_id.as_deref(),
                    inventory.receiver.vendor_id,
                    inventory.receiver.product_id,
                    &inventory.receiver.name,
                    device.slot,
                    device.wpid,
                ),
                name: device
                    .codename
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| inventory.receiver.name.clone()),
                online: device.online,
                battery: device.battery.as_ref().map(map_battery),
            })
        })
        .collect::<Vec<_>>();

    #[cfg(target_os = "windows")]
    {
        let mchose_devices = tokio::task::spawn_blocking(crate::mchose::enumerate)
            .await
            .context("MCHOSE HID 扫描任务失败")??;
        devices.extend(mchose_devices);
    }

    Ok(devices)
}

fn stable_device_id(
    receiver_id: Option<&str>,
    vendor_id: u16,
    product_id: u16,
    receiver_name: &str,
    slot: u8,
    wpid: Option<u16>,
) -> String {
    receiver_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("receiver:{id}:slot:{slot}"))
        .unwrap_or_else(|| {
            format!(
                "receiver:{vendor_id:04x}:{product_id:04x}:{receiver_name}:slot:{slot}:wpid:{}",
                wpid.map_or_else(|| "unknown".to_string(), |wpid| format!("{wpid:04x}")),
            )
        })
}

fn map_battery(battery: &OpenLogiBatteryInfo) -> BatteryInfo {
    BatteryInfo {
        percentage: battery.percentage,
        level: map_level(battery.level),
        status: map_status(battery.status),
    }
}

fn map_level(level: OpenLogiBatteryLevel) -> BatteryLevel {
    match level {
        OpenLogiBatteryLevel::Critical => BatteryLevel::Critical,
        OpenLogiBatteryLevel::Low => BatteryLevel::Low,
        OpenLogiBatteryLevel::Good => BatteryLevel::Good,
        OpenLogiBatteryLevel::Full => BatteryLevel::Full,
        OpenLogiBatteryLevel::Unknown => BatteryLevel::Unknown,
    }
}

fn map_status(status: OpenLogiBatteryStatus) -> BatteryStatus {
    match status {
        OpenLogiBatteryStatus::Discharging => BatteryStatus::Discharging,
        OpenLogiBatteryStatus::Charging => BatteryStatus::Charging,
        OpenLogiBatteryStatus::ChargingSlow => BatteryStatus::ChargingSlow,
        OpenLogiBatteryStatus::Full => BatteryStatus::Full,
        OpenLogiBatteryStatus::Error => BatteryStatus::Error,
        OpenLogiBatteryStatus::Unknown => BatteryStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::stable_device_id;

    #[test]
    fn receiver_id_is_preferred_for_device_identity() {
        assert_eq!(
            stable_device_id(Some("receiver-1"), 0x046d, 0xc548, "Bolt", 2, Some(0x1234)),
            "receiver:receiver-1:slot:2"
        );
    }

    #[test]
    fn device_identity_falls_back_to_hardware_fields() {
        assert_eq!(
            stable_device_id(None, 0x046d, 0xc548, "Bolt", 2, Some(0x1234)),
            "receiver:046d:c548:Bolt:slot:2:wpid:1234"
        );
    }
}
