use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{self, Receiver},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use eframe::{App, Frame, NativeOptions, egui};
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, SW_HIDE, ShowWindow},
    },
    core::BOOL,
};

use crate::{
    APP_DISPLAY_NAME, APP_USER_MODEL_ID, app_identity, autostart,
    battery::{BatteryStatus, DeviceSnapshot},
    config::{self, Config},
    monitor::{self, MonitorCommand, MonitorEvent},
    visual,
};

const SETTINGS_ID: &str = "settings";
const EXIT_ID: &str = "exit";
const SETTINGS_VIEWPORT_ID: &str = "settings-window";
const SETTINGS_WINDOW_TITLE: &str = "设置 - 罗技电量管家";
static ROOT_VIEWPORTS: OnceLock<Vec<isize>> = OnceLock::new();

const CHINESE_FONT_PATHS: [&str; 4] = [
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\NotoSansSC-VF.ttf",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
];

pub(crate) fn run() -> Result<()> {
    let config_path = config::config_path()?;
    let loaded = config::Config::load(&config_path)?;
    let mut startup_warning = loaded.warning;

    if !config_path.exists()
        && let Err(error) = loaded.config.save(&config_path)
    {
        append_warning(&mut startup_warning, format!("无法创建默认配置：{error}"));
    }

    if let Err(error) = autostart::set_enabled(loaded.config.autostart) {
        append_warning(
            &mut startup_warning,
            format!("无法应用开机启动设置：{error}"),
        );
    }

    if let Err(error) = app_identity::ensure_notification_shortcut() {
        append_warning(
            &mut startup_warning,
            format!("通知应用身份注册失败：{error}"),
        );
    }

    let (event_sender, event_receiver) = mpsc::channel();
    let monitor_commands = monitor::spawn(loaded.config.clone(), event_sender);
    let tray_menu = TrayMenu::new()?;
    let tray_icon = TrayIconBuilder::new()
        .with_icon(visual::tray_icon()?)
        .with_tooltip(APP_DISPLAY_NAME)
        .with_menu(Box::new(tray_menu.root.clone()))
        .build()
        .context("failed to create tray icon")?;

    let app = TrayApp {
        config: loaded.config,
        config_path,
        devices: Vec::new(),
        selected_device: None,
        event_receiver,
        monitor_commands,
        menu: tray_menu,
        _tray_icon: tray_icon,
        settings_view: Arc::new(Mutex::new(None)),
        status: startup_warning,
    };

    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1.0, 1.0])
            .with_min_inner_size([1.0, 1.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_taskbar(false)
            .with_app_id(APP_USER_MODEL_ID)
            .with_icon(visual::egui_icon()?)
            .with_visible(false),
        ..Default::default()
    };

    let result = eframe::run_native(
        APP_DISPLAY_NAME,
        options,
        Box::new(|creation_context| {
            let mut app = app;
            hide_root_viewport();
            if let Err(error) = configure_chinese_font(&creation_context.egui_ctx) {
                append_warning(&mut app.status, error);
            }
            Ok(Box::new(app))
        }),
    );

    result.map_err(|error| anyhow::anyhow!("settings window stopped unexpectedly: {error:?}"))
}

struct TrayApp {
    config: Config,
    config_path: PathBuf,
    devices: Vec<DeviceSnapshot>,
    selected_device: Option<String>,
    event_receiver: Receiver<MonitorEvent>,
    monitor_commands: tokio::sync::mpsc::Sender<MonitorCommand>,
    menu: TrayMenu,
    _tray_icon: TrayIcon,
    settings_view: Arc<Mutex<Option<SettingsViewState>>>,
    status: Option<String>,
}

struct SettingsViewState {
    form: SettingsForm,
    status: Option<String>,
    action: Option<SettingsAction>,
    center_on_open: bool,
}

#[derive(Clone, Copy)]
enum SettingsAction {
    Save,
    Cancel,
    Close,
}

impl App for TrayApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut Frame) {
        hide_root_viewport();
        context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.process_monitor_events();
        self.process_menu_events(context);
        self.process_settings_action(context);
        self.sync_settings_view();
        self.show_settings_viewport(context);
        context.request_repaint_after(Duration::from_millis(100));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        hide_root_viewport();
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }
}

impl TrayApp {
    fn sync_settings_view(&self) {
        let Ok(mut view) = self.settings_view.lock() else {
            return;
        };
        let Some(state) = view.as_mut() else {
            return;
        };

        state.status = self.status.clone();
    }

    fn show_settings_viewport(&self, context: &egui::Context) {
        let is_open = self
            .settings_view
            .lock()
            .ok()
            .is_some_and(|view| view.is_some());
        if !is_open {
            return;
        }

        let settings_viewport = egui::ViewportId::from_hash_of(SETTINGS_VIEWPORT_ID);
        let shared_view = Arc::clone(&self.settings_view);
        context.show_viewport_deferred(
            settings_viewport,
            egui::ViewportBuilder::default()
                .with_title(SETTINGS_WINDOW_TITLE)
                .with_inner_size([460.0, 420.0])
                .with_min_inner_size([460.0, 420.0])
                .with_resizable(false)
                .with_visible(true)
                .with_icon(visual::egui_icon().unwrap_or_default()),
            move |ui, _| {
                apply_settings_style(ui.ctx());
                let Ok(mut view) = shared_view.lock() else {
                    return;
                };
                let Some(state) = view.as_mut() else {
                    return;
                };
                if state.center_on_open
                    && let Some(command) = egui::ViewportCommand::center_on_screen(ui.ctx())
                {
                    ui.ctx().send_viewport_cmd(command);
                    state.center_on_open = false;
                }
                let mut save_requested = false;
                let mut cancel_requested = false;
                let close_requested = ui.input(|input| input.viewport().close_requested());
                if close_requested {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                render_settings(
                    ui,
                    SettingsRender {
                        settings: &mut state.form,
                        status: state.status.as_deref(),
                        save_requested: &mut save_requested,
                        cancel_requested: &mut cancel_requested,
                    },
                );

                if close_requested {
                    state.action = Some(SettingsAction::Close);
                } else if save_requested {
                    state.action = Some(SettingsAction::Save);
                } else if cancel_requested {
                    state.action = Some(SettingsAction::Cancel);
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            },
        );
    }

    fn process_settings_action(&mut self, context: &egui::Context) {
        let action_and_form = self.settings_view.lock().ok().and_then(|mut view| {
            let state = view.as_mut()?;
            let action = state.action.take()?;
            Some((action, state.form.clone()))
        });

        let Some((action, form)) = action_and_form else {
            return;
        };

        match action {
            SettingsAction::Save => {
                if let Err(error) = self.save_settings(&form) {
                    self.status = Some(error.to_string());
                    if let Ok(mut view) = self.settings_view.lock()
                        && let Some(state) = view.as_mut()
                    {
                        state.status = self.status.clone();
                        state.action = None;
                    }
                    context.request_repaint();
                    return;
                }
                context.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of(SETTINGS_VIEWPORT_ID),
                    egui::ViewportCommand::Close,
                );
                if let Ok(mut view) = self.settings_view.lock() {
                    *view = None;
                }
            }
            SettingsAction::Cancel | SettingsAction::Close => {
                if let Ok(mut view) = self.settings_view.lock() {
                    *view = None;
                }
            }
        }
    }
}

fn apply_settings_style(context: &egui::Context) {
    let background = egui::Color32::from_rgb(245, 247, 250);
    let surface = egui::Color32::WHITE;
    let border = egui::Color32::from_rgb(218, 225, 235);
    let text = egui::Color32::from_rgb(31, 41, 55);
    let muted = egui::Color32::from_rgb(102, 112, 133);

    context.set_theme(egui::Theme::Light);
    context.style_mut_of(egui::Theme::Light, |style| {
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.visuals = egui::Visuals::light();
        style.visuals.window_fill = background;
        style.visuals.panel_fill = background;
        style.visuals.override_text_color = Some(text);
        style.visuals.weak_text_color = Some(muted);
        style.visuals.extreme_bg_color = surface;
        style.visuals.text_edit_bg_color = Some(surface);
        style.visuals.warn_fg_color = egui::Color32::from_rgb(180, 105, 20);
        style.visuals.error_fg_color = egui::Color32::from_rgb(190, 55, 55);
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            widget.corner_radius = egui::CornerRadius::same(6);
            widget.bg_fill = surface;
            widget.bg_stroke = egui::Stroke::new(1.0, border);
        }
        style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(245, 249, 255);
        style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(232, 241, 255);
    });
}

struct SettingsRender<'a> {
    settings: &'a mut SettingsForm,
    status: Option<&'a str>,
    save_requested: &'a mut bool,
    cancel_requested: &'a mut bool,
}

fn render_settings(ui: &mut egui::Ui, render: SettingsRender<'_>) {
    let SettingsRender {
        settings,
        status,
        save_requested,
        cancel_requested,
    } = render;
    let background = egui::Color32::from_rgb(245, 247, 250);
    let surface = egui::Color32::WHITE;
    let border = egui::Color32::from_rgb(218, 225, 235);
    let blue = egui::Color32::from_rgb(37, 99, 235);
    let error_text = egui::Color32::from_rgb(190, 55, 55);

    egui::Panel::bottom("settings_actions")
        .frame(
            egui::Frame::new()
                .fill(surface)
                .stroke(egui::Stroke::new(1.0, border))
                .inner_margin(egui::Margin::symmetric(18, 9)),
        )
        .show(ui, |ui| {
            ui.set_min_height(46.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_sized(
                        egui::vec2(88.0, 32.0),
                        egui::Button::new(egui::RichText::new("保存").color(egui::Color32::WHITE))
                            .fill(blue),
                    )
                    .clicked()
                {
                    *save_requested = true;
                }
                if ui
                    .add_sized(egui::vec2(88.0, 32.0), egui::Button::new("取消"))
                    .clicked()
                {
                    *cancel_requested = true;
                }
            });
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(background)
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let row_height = 60.0;
            ui.add_space(4.0);
            let threshold_error = validate_threshold_text(&settings.threshold);
            setting_row(
                ui,
                row_height,
                "低电量阈值",
                "低于此电量时提醒",
                |ui| {
                    numeric_field(ui, &mut settings.threshold, "%", threshold_error.as_deref());
                },
            );
            setting_separator(ui, border);

            let repeat_alert_error =
                validate_repeat_alert_interval_text(&settings.repeat_alert_interval);
            setting_row(
                ui,
                row_height,
                "重复提醒间隔",
                "持续低电量时再次提醒",
                |ui| {
                    numeric_field(
                        ui,
                        &mut settings.repeat_alert_interval,
                        "分钟",
                        repeat_alert_error.as_deref(),
                    );
                },
            );
            setting_separator(ui, border);

            let interval_error = validate_poll_interval_text(&settings.poll_interval);
            setting_row(
                ui,
                row_height,
                "轮询间隔",
                "两次检查之间的间隔",
                |ui| {
                    numeric_field(
                        ui,
                        &mut settings.poll_interval,
                        "秒",
                        interval_error.as_deref(),
                    );
                },
            );
            setting_separator(ui, border);

            setting_row(
                ui,
                row_height,
                "开机启动",
                "登录 Windows 后自动运行",
                |ui| {
                    switch_control(ui, &mut settings.autostart);
                },
            );

            if let Some(status) = status {
                ui.add_space(12.0);
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(255, 242, 242))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(239, 176, 176),
                    ))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.colored_label(error_text, status);
                    });
            }
        });
}

fn setting_row(
    ui: &mut egui::Ui,
    row_height: f32,
    title: &str,
    description: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    const LABEL_COLUMN_WIDTH: f32 = 220.0;
    const CONTROL_WIDTH: f32 = 176.0;
    const COLUMN_GAP: f32 = 16.0;

    let row_rect = ui
        .allocate_exact_size(
            egui::vec2(ui.available_width(), row_height),
            egui::Sense::hover(),
        )
        .0;
    let label_rect =
        egui::Rect::from_min_size(row_rect.min, egui::vec2(LABEL_COLUMN_WIDTH, row_height));
    let control_rect = egui::Rect::from_min_size(
        egui::pos2(
            row_rect.left() + LABEL_COLUMN_WIDTH + COLUMN_GAP,
            row_rect.top(),
        ),
        egui::vec2(CONTROL_WIDTH, row_height),
    );

    let mut label_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(label_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    label_ui.set_clip_rect(label_ui.clip_rect().intersect(label_rect));
    label_ui.add_space(((row_height - 42.0) / 2.0).max(0.0));
    label_ui.label(egui::RichText::new(title).size(14.0).strong());
    label_ui.add_space(2.0);
    label_ui.label(egui::RichText::new(description).size(11.0).weak());

    let mut control_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(control_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    control(&mut control_ui);
}

fn setting_separator(ui: &mut egui::Ui, color: egui::Color32) {
    ui.add_space(3.0);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        egui::Stroke::new(1.0, color),
    );
    ui.add_space(3.0);
}

fn switch_control(ui: &mut egui::Ui, value: &mut bool) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(42.0, 24.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
    }

    let track = if *value {
        egui::Color32::from_rgb(37, 99, 235)
    } else {
        egui::Color32::from_rgb(185, 193, 205)
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(12), track);
    let knob_x = if *value {
        rect.right() - 12.0
    } else {
        rect.left() + 12.0
    };
    ui.painter().circle_filled(
        egui::pos2(knob_x, rect.center().y),
        9.0,
        egui::Color32::WHITE,
    );
}

fn numeric_field(ui: &mut egui::Ui, value: &mut String, unit: &str, error: Option<&str>) {
    const INPUT_WIDTH: f32 = 72.0;
    const UNIT_WIDTH: f32 = 30.0;
    const INPUT_HEIGHT: f32 = 26.0;
    const ERROR_HEIGHT: f32 = 14.0;
    const ERROR_GAP: f32 = 6.0;

    let border = if error.is_some() {
        egui::Color32::from_rgb(210, 67, 57)
    } else {
        egui::Color32::from_rgb(196, 206, 220)
    };

    let available = ui.available_size();
    let content_height = INPUT_HEIGHT + error.map_or(0.0, |_| ERROR_GAP + ERROR_HEIGHT);
    ui.allocate_ui_with_layout(available, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.add_space(((available.y - content_height) / 2.0).max(0.0));
        ui.allocate_ui_with_layout(
            egui::vec2(INPUT_WIDTH + UNIT_WIDTH + 6.0, INPUT_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .stroke(egui::Stroke::new(1.0, border))
                    .corner_radius(egui::CornerRadius::same(5))
                    .inner_margin(egui::Margin::symmetric(7, 2))
                    .show(ui, |ui| {
                        ui.add_sized(
                            egui::vec2(INPUT_WIDTH - 14.0, 22.0),
                            egui::TextEdit::singleline(value)
                                .horizontal_align(egui::Align::Center)
                                .vertical_align(egui::Align::Center)
                                .frame(egui::Frame::NONE),
                        );
                    });
                ui.add_sized(
                    egui::vec2(UNIT_WIDTH, 30.0),
                    egui::Label::new(unit).sense(egui::Sense::hover()),
                );
            },
        );
        if let Some(error) = error {
            ui.colored_label(
                egui::Color32::from_rgb(190, 55, 55),
                egui::RichText::new(error).size(11.0),
            );
        }
    });
}

fn validate_threshold_text(value: &str) -> Option<String> {
    match value.trim().parse::<u8>() {
        Ok(value) if (1..=99).contains(&value) => None,
        Ok(_) => Some("请输入 1-99 之间的整数".to_string()),
        Err(_) => Some("请输入整数".to_string()),
    }
}

fn validate_repeat_alert_interval_text(value: &str) -> Option<String> {
    match value.trim().parse::<u64>() {
        Ok(value) if (1..=1440).contains(&value) => None,
        Ok(_) => Some("请输入 1-1440 之间的整数".to_string()),
        Err(_) => Some("请输入整数".to_string()),
    }
}

fn validate_poll_interval_text(value: &str) -> Option<String> {
    match value.trim().parse::<u64>() {
        Ok(value) if (15..=3600).contains(&value) => None,
        Ok(_) => Some("请输入 15-3600 之间的整数".to_string()),
        Err(_) => Some("请输入整数".to_string()),
    }
}

impl TrayApp {
    fn process_monitor_events(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            match event {
                MonitorEvent::Devices {
                    devices,
                    selected_device,
                    alert,
                } => {
                    self.devices = devices;
                    self.selected_device = selected_device.clone();
                    self.menu
                        .rebuild_devices(&self.devices, self.selected_device.as_deref());

                    if self.config.selected_device != selected_device {
                        self.config.selected_device = selected_device;
                        self.persist_config();
                        self.send_config_update();
                    }

                    if let Some(alert) = alert
                        && let Err(error) =
                            show_low_battery_notification(&alert.device_name, alert.percentage)
                    {
                        self.status = Some(error.to_string());
                    }
                }
                MonitorEvent::ScanFailed(error) => {
                    self.status = Some(format!("上次扫描失败：{error}"));
                }
            }
        }
    }

    fn process_menu_events(&mut self, context: &egui::Context) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.menu.settings.id().clone() {
                if let Ok(mut view) = self.settings_view.lock() {
                    *view = Some(SettingsViewState {
                        form: SettingsForm::from_config(&self.config),
                        status: self.status.clone(),
                        action: None,
                        center_on_open: true,
                    });
                }
                context.request_repaint();
            } else if event.id == self.menu.exit.id().clone() {
                let _ = self.monitor_commands.try_send(MonitorCommand::Shutdown);
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if let Some(device_id) = self.menu.device_ids.get(&event.id).cloned() {
                self.config.selected_device = Some(device_id.clone());
                self.selected_device = Some(device_id);
                self.menu
                    .rebuild_devices(&self.devices, self.selected_device.as_deref());
                self.persist_config();
                self.send_config_update();
            }
        }
    }

    fn send_config_update(&mut self) {
        if self
            .monitor_commands
            .try_send(MonitorCommand::UpdateConfig(self.config.clone()))
            .is_err()
        {
            self.status = Some("监控任务已停止。".to_string());
        }
    }

    fn persist_config(&mut self) {
        if let Err(error) = self.config.save(&self.config_path) {
            self.status = Some(error.to_string());
        }
    }

    fn save_settings(&mut self, settings: &SettingsForm) -> Result<()> {
        let threshold = settings
            .threshold
            .trim()
            .parse::<u8>()
            .context("低电量阈值必须是整数")?;
        let poll_interval_seconds = settings
            .poll_interval
            .trim()
            .parse::<u64>()
            .context("轮询间隔必须是整数")?;
        let repeat_alert_interval_minutes = settings
            .repeat_alert_interval
            .trim()
            .parse::<u64>()
            .context("重复提醒间隔必须是整数")?;

        let next_config = Config {
            threshold,
            poll_interval_seconds,
            repeat_alert_interval_minutes,
            selected_device: self.config.selected_device.clone(),
            autostart: settings.autostart,
        };
        next_config.validate()?;

        let previous_config = self.config.clone();
        next_config.save(&self.config_path)?;
        if let Err(error) = autostart::set_enabled(next_config.autostart) {
            let _ = previous_config.save(&self.config_path);
            return Err(error);
        }

        self.config = next_config;
        self.status = None;
        self.send_config_update();
        Ok(())
    }
}

#[derive(Clone)]
struct SettingsForm {
    threshold: String,
    repeat_alert_interval: String,
    poll_interval: String,
    autostart: bool,
}

impl SettingsForm {
    fn from_config(config: &Config) -> Self {
        Self {
            threshold: config.threshold.to_string(),
            repeat_alert_interval: config.repeat_alert_interval_minutes.to_string(),
            poll_interval: config.poll_interval_seconds.to_string(),
            autostart: config.autostart,
        }
    }
}

struct TrayMenu {
    root: Menu,
    devices: Submenu,
    settings: MenuItem,
    exit: MenuItem,
    device_ids: HashMap<MenuId, String>,
}

impl TrayMenu {
    fn new() -> Result<Self> {
        let root = Menu::new();
        let devices = Submenu::with_id("devices", "设备", true);
        let settings = MenuItem::with_id(SETTINGS_ID, "设置", true, None);
        let exit = MenuItem::with_id(EXIT_ID, "退出", true, None);

        root.append(&devices)
            .context("failed to add devices menu")?;
        root.append(&PredefinedMenuItem::separator())
            .context("failed to add menu separator")?;
        root.append(&settings)
            .context("failed to add settings menu")?;
        root.append(&exit).context("failed to add exit menu")?;

        Ok(Self {
            root,
            devices,
            settings,
            exit,
            device_ids: HashMap::new(),
        })
    }

    fn rebuild_devices(&mut self, devices: &[DeviceSnapshot], selected_id: Option<&str>) {
        while self.devices.remove_at(0).is_some() {}
        self.device_ids.clear();

        if devices.is_empty() {
            let item = MenuItem::with_id("device:none", "未找到罗技设备", false, None);
            let _ = self.devices.append(&item);
            return;
        }

        for device in devices {
            let menu_id = MenuId::new(format!("device:{}", device.id));
            let item = CheckMenuItem::with_id(
                menu_id.clone(),
                format_device_label(device),
                device.online,
                selected_id == Some(device.id.as_str()),
                None,
            );
            let _ = self.devices.append(&item);
            self.device_ids.insert(menu_id, device.id.clone());
        }
    }
}

fn format_device_label(device: &DeviceSnapshot) -> String {
    let connection = if device.online { "在线" } else { "离线" };
    let battery = device.battery.map_or_else(
        || "电量未知".to_string(),
        |battery| format!("{}% ({})", battery.percentage, status_name(battery.status)),
    );
    format!("{} - {}，{}", device.name, battery, connection)
}

fn status_name(status: BatteryStatus) -> &'static str {
    match status {
        BatteryStatus::Discharging => "放电中",
        BatteryStatus::Charging => "充电中",
        BatteryStatus::ChargingSlow => "慢速充电",
        BatteryStatus::Full => "已充满",
        BatteryStatus::Error => "错误",
        BatteryStatus::Unknown => "未知",
    }
}

fn show_low_battery_notification(name: &str, percentage: u8) -> Result<()> {
    let body = format!("{name} 当前电量为 {percentage}%，请及时充电。");
    notify_rust::Notification::new()
        .summary(APP_DISPLAY_NAME)
        .app_id(APP_USER_MODEL_ID)
        .body(&body)
        .show()
        .context("发送低电量通知失败")?;
    Ok(())
}

fn append_warning(warning: &mut Option<String>, message: String) {
    *warning = Some(match warning.take() {
        Some(previous) => format!("{previous}; {message}"),
        None => message,
    });
}

fn configure_chinese_font(context: &egui::Context) -> Result<(), String> {
    for path in CHINESE_FONT_PATHS {
        let Ok(data) = fs::read(path) else {
            continue;
        };

        let mut definitions = egui::FontDefinitions::default();
        definitions.font_data.insert(
            "system-chinese".to_string(),
            Arc::new(egui::FontData::from_owned(data)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            if let Some(fonts) = definitions.families.get_mut(&family) {
                fonts.insert(0, "system-chinese".to_string());
            }
        }
        context.set_fonts(definitions);
        return Ok(());
    }

    Err("未找到可用中文字体，请检查 Windows 字体目录。".to_string())
}

fn hide_root_viewport() {
    if ROOT_VIEWPORTS.get().is_none() {
        let mut search = ProcessWindowSearch {
            process_id: std::process::id(),
            windows: Vec::new(),
        };
        let _ = unsafe {
            EnumWindows(
                Some(collect_process_window),
                LPARAM(&mut search as *mut ProcessWindowSearch as isize),
            )
        };
        if !search.windows.is_empty() {
            let _ = ROOT_VIEWPORTS.set(search.windows);
        }
    }

    for raw_window in ROOT_VIEWPORTS.get().into_iter().flatten() {
        let _ = unsafe { ShowWindow(HWND(*raw_window as *mut _), SW_HIDE) };
    }
}

struct ProcessWindowSearch {
    process_id: u32,
    windows: Vec<isize>,
}

unsafe extern "system" fn collect_process_window(window: HWND, data: LPARAM) -> BOOL {
    let search = unsafe { &mut *(data.0 as *mut ProcessWindowSearch) };
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    if process_id == search.process_id {
        search.windows.push(window.0 as isize);
    }
    BOOL(1)
}
