use std::sync::OnceLock;

use anyhow::{Context, Result};

const ICON_PNG: &[u8] = include_bytes!("../assets/battery-monitor.png");

#[derive(Clone)]
struct IconPixels {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

static DECODED_ICON: OnceLock<Result<IconPixels, String>> = OnceLock::new();

pub(crate) fn egui_icon() -> Result<eframe::egui::IconData> {
    let (rgba, width, height) = decoded_icon()?;
    Ok(eframe::egui::IconData {
        rgba,
        width,
        height,
    })
}

pub(crate) fn tray_icon() -> Result<tray_icon::Icon> {
    let (rgba, width, height) = decoded_icon()?;
    tray_icon::Icon::from_rgba(rgba, width, height).context("创建托盘图标失败")
}

fn decoded_icon() -> Result<(Vec<u8>, u32, u32)> {
    let cached = DECODED_ICON.get_or_init(|| {
        let image = image::load_from_memory(ICON_PNG)
            .map_err(|error| format!("读取程序图标失败：{error}"))?
            .to_rgba8();
        Ok(IconPixels {
            rgba: image.to_vec(),
            width: image.width(),
            height: image.height(),
        })
    });

    cached
        .clone()
        .map(|icon| (icon.rgba, icon.width, icon.height))
        .map_err(anyhow::Error::msg)
}
