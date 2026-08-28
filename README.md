# 罗技电量管家

一个 Windows 托盘应用，用于监控罗技 HID++ 设备的电量，并在选中设备低电量且处于放电状态时发送 Windows 通知。

程序使用固定的 `LogitechMonitor.App` 应用身份和当前用户开始菜单快捷方式，因此通知来源显示为“罗技电量管家”，不会显示为 Windows PowerShell。

## 启动

直接双击 `logitech-monitor.exe`，程序会在后台驻留托盘，不显示控制台窗口。再次双击时，第二个进程会静默退出。

右键托盘图标可以：

- 在“设备”菜单中选择一个目标设备。
- 打开“设置”窗口。
- 退出程序。

## 设置

设置保存在可执行文件旁边的 `config.toml`：

- 低电量阈值：`1%` 到 `99%`，默认 `20%`。
- 重复提醒间隔：`1` 到 `1440` 分钟，默认 `5` 分钟。
- 轮询间隔：`15` 到 `3600` 秒，默认 `60` 秒。
- 开机启动：写入当前用户的 Windows 启动项，不需要管理员权限。

如果配置文件缺失，程序会自动创建。配置损坏或超出范围时会使用默认值，并在设置窗口提示错误。程序目录不可写时，保存操作会保留当前配置并显示失败原因。

## 告警规则

- 选中设备电量小于等于阈值，并且状态为“放电中”时发送通知。
- 首次发现设备已经低于阈值时立即通知。
- 持续处于低电量放电状态时，按照重复提醒间隔再次通知。
- 电量恢复到阈值以上后，下一次低于阈值才会重新通知。
- 充电中、慢速充电、已充满、电量未知或设备离线时不会发送低电量通知；恢复为低电量放电后，按照当前告警周期继续计时。
- 设备断开时保留原选择，重新连接后继续监控，不会自动切换到其他设备。

## 开发与诊断

无参数启动托盘模式：

```text
cargo run
```

列出设备：

```text
cargo run -- devices
```

发送测试通知：

```text
cargo run -- notify-test
```

`devices` 和 `notify-test` 命令名保持兼容，输出内容为中文。托盘模式仅支持 Windows。

## 本机 Windows 开发环境

Rust 和 Cargo 安装在 `D:\Coding\environments\Rust`，Visual Studio Build Tools 安装在 `D:\Coding\environments\VisualStudioBuildTools`。需要 MSVC 链接器时，在 PowerShell 7 中加载 Visual Studio 开发环境：

```powershell
$vsRoot = "D:\Coding\environments\VisualStudioBuildTools"
Import-Module "$vsRoot\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstanceId "be9460df" -Arch amd64 -HostArch amd64
```
