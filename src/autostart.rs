use anyhow::{Context, Result};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "BatteryMonitor";

pub(crate) fn set_enabled(enabled: bool) -> Result<()> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = current_user
        .create_subkey(RUN_KEY)
        .context("打开 Windows 启动项失败")?;

    if enabled {
        let executable = std::env::current_exe().context("定位程序路径失败")?;
        let command = format!("\"{}\"", executable.display());
        run_key
            .set_value(VALUE_NAME, &command)
            .context("启用 Windows 开机启动失败")?;
    } else {
        match run_key.delete_value(VALUE_NAME) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("关闭 Windows 开机启动失败"),
        }
    }

    Ok(())
}
