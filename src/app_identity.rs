use std::{
    mem::{ManuallyDrop, size_of},
    path::{Path, PathBuf},
    ptr,
};

use anyhow::{Context, Result};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, PROPERTYKEY},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemAlloc, CoUninitialize, IPersistFile,
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantClear,
                },
            },
            Threading::CreateMutexW,
            Variant::VT_LPWSTR,
        },
        UI::{
            Shell::PropertiesSystem::IPropertyStore,
            Shell::{IShellLinkW, SetCurrentProcessExplicitAppUserModelID},
        },
    },
    core::{GUID, HSTRING, Interface, PWSTR, w},
};

use crate::{APP_DISPLAY_NAME, APP_USER_MODEL_ID};

const INSTANCE_MUTEX_NAME: &str = "Local\\BatteryMonitor.Singleton.v1";
const SHORTCUT_FILE_NAME: &str = "电量管家.lnk";

pub(crate) struct SingleInstance {
    handle: HANDLE,
    already_running: bool,
}

impl SingleInstance {
    pub(crate) fn acquire() -> Result<Self> {
        let name = HSTRING::from(INSTANCE_MUTEX_NAME);
        let handle =
            unsafe { CreateMutexW(None, false, &name) }.context("创建程序单实例互斥体失败")?;
        let already_running =
            unsafe { windows::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS;

        Ok(Self {
            handle,
            already_running,
        })
    }

    pub(crate) fn is_already_running(&self) -> bool {
        self.already_running
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

pub(crate) fn set_current_process_identity() -> Result<()> {
    unsafe { SetCurrentProcessExplicitAppUserModelID(w!("BatteryMonitor.App")) }
        .context("设置 Windows 应用身份失败")?;
    Ok(())
}

pub(crate) fn ensure_notification_shortcut() -> Result<()> {
    let executable = std::env::current_exe().context("定位程序路径失败")?;
    let shortcut = shortcut_path()?;
    if let Some(parent) = shortcut.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建开始菜单目录失败：{}", parent.display()))?;
    }

    create_shortcut(&shortcut, &executable)
}

fn shortcut_path() -> Result<PathBuf> {
    let app_data = std::env::var_os("APPDATA").context("无法定位当前用户的 AppData 目录")?;
    Ok(PathBuf::from(app_data)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join(SHORTCUT_FILE_NAME))
}

fn create_shortcut(shortcut: &Path, executable: &Path) -> Result<()> {
    let com_result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    com_result.ok().context("初始化 Windows Shell 组件失败")?;

    let result = unsafe { create_shortcut_inner(shortcut, executable) };
    unsafe { CoUninitialize() };
    result
}

unsafe fn create_shortcut_inner(shortcut: &Path, executable: &Path) -> Result<()> {
    let link: IShellLinkW = unsafe {
        CoCreateInstance(
            &GUID::from_u128(0x00021401_0000_0000_c000_000000000046),
            None,
            CLSCTX_INPROC_SERVER,
        )
    }
    .context("创建 Windows 快捷方式对象失败")?;
    let executable = HSTRING::from(executable.to_string_lossy().as_ref());
    let working_directory = HSTRING::from(
        executable
            .to_string_lossy()
            .rsplit_once(['\\', '/'])
            .map_or_else(|| ".".to_string(), |(directory, _)| directory.to_string()),
    );
    let description = HSTRING::from(APP_DISPLAY_NAME);

    unsafe {
        link.SetPath(&executable).context("设置快捷方式目标失败")?;
        link.SetWorkingDirectory(&working_directory)
            .context("设置快捷方式工作目录失败")?;
        link.SetDescription(&description)
            .context("设置快捷方式描述失败")?;
        link.SetIconLocation(&executable, 0)
            .context("设置快捷方式图标失败")?;
    }

    let property_store: IPropertyStore = link.cast().context("打开快捷方式属性存储失败")?;
    let app_id: Vec<u16> = APP_USER_MODEL_ID.encode_utf16().chain([0]).collect();
    let app_id_ptr = unsafe { CoTaskMemAlloc(app_id.len() * size_of::<u16>()) as *mut u16 };
    if app_id_ptr.is_null() {
        anyhow::bail!("为快捷方式应用身份分配内存失败");
    }
    unsafe { ptr::copy_nonoverlapping(app_id.as_ptr(), app_id_ptr, app_id.len()) };
    let property = PROPVARIANT {
        Anonymous: windows::Win32::System::Com::StructuredStorage::PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_LPWSTR,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    pwszVal: PWSTR(app_id_ptr),
                },
            }),
        },
    };
    let mut property = property;
    unsafe {
        let set_result = property_store.SetValue(&app_user_model_id_key(), &property);
        let clear_result = PropVariantClear(&mut property);
        set_result.context("写入快捷方式应用身份失败")?;
        clear_result.context("清理快捷方式应用身份失败")?;
        property_store.Commit().context("提交快捷方式属性失败")?;
    }

    let persist_file: IPersistFile = link.cast().context("打开快捷方式持久化接口失败")?;
    let shortcut = HSTRING::from(shortcut.to_string_lossy().as_ref());
    unsafe {
        persist_file
            .Save(&shortcut, true)
            .context("保存开始菜单快捷方式失败")?;
    }
    Ok(())
}

fn app_user_model_id_key() -> PROPERTYKEY {
    PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    }
}
