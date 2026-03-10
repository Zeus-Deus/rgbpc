use crate::core::{
    config::{AppConfig, SavedState},
    hook,
    openrgb::{self, OpenRgbDevice},
    DeviceActionSummary,
};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph},
    Terminal,
};
use std::io;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

pub enum AppEvent {
    DevicesReloaded(Result<Vec<OpenRgbDevice>, String>),
    SyncComplete(Result<(), String>),
    ColorSetComplete(DeviceActionSummary),
    OffComplete(DeviceActionSummary),
    RainbowComplete(DeviceActionSummary),
    Tick,
}

#[derive(PartialEq)]
pub enum AppMode {
    Normal,
    ColorPicker,
}

const PALETTE: &[&str] = &[
    "FF0000", "00FF00", "0000FF", "FFFF00", "00FFFF", "FF00FF", "FFFFFF", "000000", "FFA500",
    "800080", "008000", "FFC0CB", "808080", "A52A2A", "FFD700", "4B0082", "F08080", "E6E6FA",
    "FF7F50", "008080", "000080", "00008B", "40E0D0", "8B0000",
];

pub struct App {
    config: AppConfig,
    devices: Vec<OpenRgbDevice>,
    selected_index: usize,
    status_msg: Arc<Mutex<String>>,
    theme_color: String,
    is_syncing: bool,
    mode: AppMode,
    selected_color_index: usize,
    custom_hex_input: String,
    input_active: bool,
    is_omarchy: bool,
    pending_state_update: Option<PendingStateUpdate>,
}

#[derive(Clone)]
struct PendingStateUpdate {
    device_keys: Vec<String>,
    state: SavedState,
}

fn filter_device_keys(requested: &[String], succeeded: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|key| succeeded.contains(key))
        .cloned()
        .collect()
}

fn format_sync_scope(count: usize) -> String {
    format!("{} device{}", count, if count == 1 { "" } else { "s" })
}

impl App {
    pub fn new() -> Self {
        let devices = openrgb::list_devices().unwrap_or_default();
        let config = AppConfig::load();

        let mut omarchy_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        omarchy_dir.push(".config/omarchy");
        let is_omarchy = omarchy_dir.is_dir();

        let hex_color = crate::core::load_theme_color().unwrap_or_else(|_| String::from("7aa2f7"));

        Self {
            config,
            devices,
            selected_index: 0,
            status_msg: Arc::new(Mutex::new(if is_omarchy {
                "Press 'a' toggle startup restore for the whole remembered setup, 's' toggle Sync, 'Enter'/'Space' toggle Device, 'c' set color for all enabled, 't' Force sync all enabled, 'R' rescan, 'r' rainbow, 'o' off, 'q' quit.".to_string()
            } else {
                "Press 'a' toggle startup restore for the whole remembered setup, 'Enter'/'Space' toggle Device, 'c' set color for all enabled, 'R' rescan, 'r' rainbow, 'o' off, 'q' quit.".to_string()
            })),
            theme_color: hex_color,
            is_syncing: false,
            mode: AppMode::Normal,
            selected_color_index: 0,
            custom_hex_input: String::new(),
            input_active: false,
            is_omarchy,
            pending_state_update: None,
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            SetTitle("RGBPC")
        )?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let (tx, rx) = mpsc::channel();

        let tx_tick = tx.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(100));
            if tx_tick.send(AppEvent::Tick).is_err() {
                break;
            }
        });

        let res = self.run_app(&mut terminal, tx, rx);

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        res
    }

    fn run_app<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        tx: mpsc::Sender<AppEvent>,
        rx: mpsc::Receiver<AppEvent>,
    ) -> io::Result<()> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) => {
                        if self.mode == AppMode::Normal {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                                KeyCode::Down | KeyCode::Char('j') => self.next(),
                                KeyCode::Up | KeyCode::Char('k') => self.previous(),
                                KeyCode::Enter | KeyCode::Char(' ') => self.toggle_current(),
                                KeyCode::Char('a') => self.toggle_restore_on_startup(),
                                KeyCode::Char('s') if self.is_omarchy => self.toggle_sync(),
                                KeyCode::Char('c') => self.open_color_picker(),
                                KeyCode::Char('t') if self.is_omarchy => self.force_sync(&tx),
                                KeyCode::Char('R') => self.reload_devices(&tx),
                                KeyCode::Char('o') => self.force_off(&tx),
                                KeyCode::Char('r') => self.force_rainbow(&tx),
                                _ => {}
                            }
                        } else if self.mode == AppMode::ColorPicker {
                            if self.input_active {
                                match key.code {
                                    KeyCode::Esc => self.input_active = false,
                                    KeyCode::Enter => self.apply_custom_color(&tx),
                                    KeyCode::Backspace => {
                                        self.custom_hex_input.pop();
                                    }
                                    KeyCode::Char(c) => {
                                        if c.is_ascii_hexdigit() && self.custom_hex_input.len() < 6
                                        {
                                            self.custom_hex_input.push(c);
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                match key.code {
                                    KeyCode::Esc | KeyCode::Char('q') => {
                                        self.mode = AppMode::Normal;
                                        *self.status_msg.lock().unwrap() =
                                            "Color picker cancelled.".to_string();
                                    }
                                    KeyCode::Left | KeyCode::Char('h') => {
                                        if self.selected_color_index % 6 > 0 {
                                            self.selected_color_index -= 1;
                                        }
                                    }
                                    KeyCode::Right | KeyCode::Char('l') => {
                                        if self.selected_color_index % 6 < 5 {
                                            self.selected_color_index += 1;
                                        }
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        if self.selected_color_index >= 6 {
                                            self.selected_color_index -= 6;
                                        }
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        if self.selected_color_index + 6 < PALETTE.len() {
                                            self.selected_color_index += 6;
                                        }
                                    }
                                    KeyCode::Enter | KeyCode::Char(' ') => {
                                        self.apply_palette_color(&tx);
                                    }
                                    KeyCode::Char('i') => {
                                        self.input_active = true;
                                        self.custom_hex_input.clear();
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Event::Mouse(mouse) => {
                        if self.mode == AppMode::ColorPicker {
                            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                                self.handle_mouse_click(
                                    mouse.column,
                                    mouse.row,
                                    terminal.size().unwrap_or_default(),
                                    &tx,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }

            while let Ok(app_event) = rx.try_recv() {
                match app_event {
                    AppEvent::DevicesReloaded(result) => match result {
                        Ok(devices) => self.finish_reload_devices(devices),
                        Err(e) => {
                            *self.status_msg.lock().unwrap() =
                                format!("Device rescan failed: {}", e)
                        }
                    },
                    AppEvent::SyncComplete(res) => {
                        self.is_syncing = false;
                        match res {
                            Ok(_) => {
                                self.theme_color = crate::core::load_theme_color()
                                    .unwrap_or_else(|_| self.theme_color.clone());
                                *self.status_msg.lock().unwrap() = "Sync complete!".to_string()
                            }
                            Err(e) => {
                                *self.status_msg.lock().unwrap() = format!("Sync failed: {}", e)
                            }
                        }
                    }
                    AppEvent::ColorSetComplete(result) => match result {
                        summary if summary.is_any_success() => {
                            if let Some(update) = self.pending_state_update.take() {
                                let successful_keys = filter_device_keys(
                                    &update.device_keys,
                                    &summary.succeeded_keys,
                                );
                                self.config
                                    .set_saved_state_for_devices(&successful_keys, update.state);
                                self.config.remove_omarchy_sync_devices(&successful_keys);
                                let _ = self.config.save();
                            }
                            *self.status_msg.lock().unwrap() = if summary.failed_devices.is_empty()
                            {
                                "Color applied successfully!".to_string()
                            } else {
                                format!(
                                    "Color applied to {} device(s); some failed: {}",
                                    summary.succeeded_keys.len(),
                                    summary.failed_devices.join(" | ")
                                )
                            }
                        }
                        summary => {
                            self.pending_state_update = None;
                            *self.status_msg.lock().unwrap() = format!(
                                "Failed to apply color: {}",
                                summary.failed_devices.join(" | ")
                            );
                            self.mode = AppMode::Normal;
                        }
                    },
                    AppEvent::OffComplete(result) => match result {
                        summary if summary.is_any_success() => {
                            if let Some(update) = self.pending_state_update.take() {
                                let successful_keys = filter_device_keys(
                                    &update.device_keys,
                                    &summary.succeeded_keys,
                                );
                                self.config
                                    .set_saved_state_for_devices(&successful_keys, update.state);
                                self.config.remove_omarchy_sync_devices(&successful_keys);
                                let _ = self.config.save();
                            }
                            *self.status_msg.lock().unwrap() = if summary.failed_devices.is_empty()
                            {
                                "Lights turned off!".to_string()
                            } else {
                                format!(
                                    "Lights turned off on {} device(s); some failed: {}",
                                    summary.succeeded_keys.len(),
                                    summary.failed_devices.join(" | ")
                                )
                            };
                        }
                        summary => {
                            self.pending_state_update = None;
                            *self.status_msg.lock().unwrap() = format!(
                                "Failed to turn off lights: {}",
                                summary.failed_devices.join(" | ")
                            );
                        }
                    },
                    AppEvent::RainbowComplete(result) => match result {
                        summary if summary.is_any_success() => {
                            if let Some(update) = self.pending_state_update.take() {
                                let successful_keys = filter_device_keys(
                                    &update.device_keys,
                                    &summary.succeeded_keys,
                                );
                                self.config
                                    .set_saved_state_for_devices(&successful_keys, update.state);
                                self.config.remove_omarchy_sync_devices(&successful_keys);
                                let _ = self.config.save();
                            }
                            *self.status_msg.lock().unwrap() = if summary.failed_devices.is_empty()
                            {
                                "Rainbow mode command sent!".to_string()
                            } else {
                                format!(
                                    "Rainbow set on {} device(s); some failed: {}",
                                    summary.succeeded_keys.len(),
                                    summary.failed_devices.join(" | ")
                                )
                            };
                        }
                        summary => {
                            self.pending_state_update = None;
                            *self.status_msg.lock().unwrap() = format!(
                                "Failed to set rainbow mode: {}",
                                summary.failed_devices.join(" | ")
                            );
                        }
                    },
                    AppEvent::Tick => {}
                }
            }
        }
    }

    fn open_color_picker(&mut self) {
        if self.devices.is_empty() {
            *self.status_msg.lock().unwrap() = "No devices to apply color to.".to_string();
            return;
        }
        if !self.has_enabled_devices() {
            *self.status_msg.lock().unwrap() =
                "All devices are disabled. Enable at least one device before manual color."
                    .to_string();
            return;
        }
        self.mode = AppMode::ColorPicker;
        self.selected_color_index = 0;
        self.custom_hex_input.clear();
        self.input_active = false;
        *self.status_msg.lock().unwrap() =
            "Pick a color for all enabled devices or press 'i' to type HEX. Esc to cancel."
                .to_string();
    }

    fn reload_devices(&mut self, tx: &mpsc::Sender<AppEvent>) {
        *self.status_msg.lock().unwrap() = "Rescanning OpenRGB devices...".to_string();
        let tx_clone = tx.clone();
        thread::spawn(move || {
            let mut result = crate::core::openrgb::list_devices();
            if let Ok(devices) = result.as_mut() {
                let _ = crate::core::openrgb::refresh_device_ids(devices);
            }
            let _ = tx_clone.send(AppEvent::DevicesReloaded(result));
        });
    }

    fn finish_reload_devices(&mut self, devices: Vec<OpenRgbDevice>) {
        let previous_key = self
            .devices
            .get(self.selected_index)
            .map(openrgb::device_profile_key);

        self.devices = devices;

        self.selected_index = previous_key
            .and_then(|key| {
                self.devices
                    .iter()
                    .position(|device| openrgb::device_profile_key(device) == key)
            })
            .unwrap_or(0);

        if self.devices.is_empty() {
            self.selected_index = 0;
            *self.status_msg.lock().unwrap() =
                "Device rescan complete, but no OpenRGB devices were found.".to_string();
        } else {
            *self.status_msg.lock().unwrap() = format!(
                "Device rescan complete: {} device(s) found.",
                self.devices.len()
            );
        }
    }

    fn force_sync(&mut self, tx: &mpsc::Sender<AppEvent>) {
        if !self.is_syncing {
            self.is_syncing = true;
            *self.status_msg.lock().unwrap() = "Syncing devices... please wait.".to_string();

            let tx_clone = tx.clone();
            thread::spawn(move || {
                let res = crate::core::perform_sync(true);
                let _ = tx_clone.send(AppEvent::SyncComplete(res));
            });
        }
    }

    fn apply_palette_color(&mut self, tx: &mpsc::Sender<AppEvent>) {
        if self.devices.is_empty() {
            return;
        }
        let color = PALETTE[self.selected_color_index].to_string();
        let mut devices = self.enabled_devices();
        if devices.is_empty() {
            *self.status_msg.lock().unwrap() =
                "All devices are disabled. Enable at least one device before manual color."
                    .to_string();
            self.mode = AppMode::Normal;
            return;
        }

        *self.status_msg.lock().unwrap() =
            format!("Applying color #{} to all enabled devices...", color);
        self.pending_state_update = Some(PendingStateUpdate {
            device_keys: self.enabled_device_keys(),
            state: SavedState::Color { hex: color.clone() },
        });
        let tx_clone = tx.clone();
        thread::spawn(move || {
            let _ = openrgb::refresh_device_ids(&mut devices);
            let res = crate::core::apply_color_to_devices_summary(devices, &color, true);
            let _ = tx_clone.send(AppEvent::ColorSetComplete(res));
        });
        self.mode = AppMode::Normal;
    }

    fn apply_custom_color(&mut self, tx: &mpsc::Sender<AppEvent>) {
        if self.devices.is_empty() {
            return;
        }
        if self.custom_hex_input.len() == 6 {
            let color = self.custom_hex_input.clone();
            let mut devices = self.enabled_devices();
            if devices.is_empty() {
                *self.status_msg.lock().unwrap() =
                    "All devices are disabled. Enable at least one device before manual color."
                        .to_string();
                self.mode = AppMode::Normal;
                return;
            }

            *self.status_msg.lock().unwrap() =
                format!("Applying custom color #{} to all enabled devices...", color);
            self.pending_state_update = Some(PendingStateUpdate {
                device_keys: self.enabled_device_keys(),
                state: SavedState::Color { hex: color.clone() },
            });
            let tx_clone = tx.clone();
            thread::spawn(move || {
                let _ = openrgb::refresh_device_ids(&mut devices);
                let res = crate::core::apply_color_to_devices_summary(devices, &color, true);
                let _ = tx_clone.send(AppEvent::ColorSetComplete(res));
            });
            self.mode = AppMode::Normal;
        } else {
            *self.status_msg.lock().unwrap() = "HEX code must be 6 characters!".to_string();
        }
    }

    fn popup_area(area: Rect, width: u16, height: u16) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((area.height.saturating_sub(height)) / 2),
                Constraint::Length(height),
                Constraint::Min(0),
            ])
            .split(area);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length((area.width.saturating_sub(width)) / 2),
                Constraint::Length(width),
                Constraint::Min(0),
            ])
            .split(popup_layout[1])[1]
    }

    fn get_palette_rects(area: Rect) -> Vec<Rect> {
        let mut rects = Vec::new();
        let col_width = area.width / 6;
        let row_height = area.height / 4;

        for row in 0..4 {
            for col in 0..6 {
                rects.push(Rect {
                    x: area.x + col * col_width,
                    y: area.y + row * row_height,
                    width: col_width,
                    height: row_height,
                });
            }
        }
        rects
    }

    fn handle_mouse_click(
        &mut self,
        col: u16,
        row: u16,
        term_area: Rect,
        tx: &mpsc::Sender<AppEvent>,
    ) {
        let popup = Self::popup_area(term_area, 44, 22);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(12),
                Constraint::Length(3),
            ])
            .split(popup);

        let palette_rects = Self::get_palette_rects(chunks[1]);

        for (i, rect) in palette_rects.iter().enumerate() {
            if col >= rect.x
                && col < rect.x + rect.width
                && row >= rect.y
                && row < rect.y + rect.height
            {
                self.selected_color_index = i;
                self.input_active = false;
                self.apply_palette_color(tx);
                return;
            }
        }

        let input_rect = chunks[2];
        if col >= input_rect.x
            && col < input_rect.x + input_rect.width
            && row >= input_rect.y
            && row < input_rect.y + input_rect.height
        {
            self.input_active = true;
            self.custom_hex_input.clear();
        }
    }

    fn next(&mut self) {
        if !self.devices.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.devices.len();
        }
    }

    fn previous(&mut self) {
        if !self.devices.is_empty() {
            if self.selected_index > 0 {
                self.selected_index -= 1;
            } else {
                self.selected_index = self.devices.len() - 1;
            }
        }
    }

    fn has_enabled_devices(&self) -> bool {
        self.devices.iter().any(|device| {
            let dev_key = openrgb::device_profile_key(device);
            !self.config.is_device_disabled(&dev_key, &device.name)
        })
    }

    fn enabled_devices(&self) -> Vec<OpenRgbDevice> {
        self.devices
            .iter()
            .filter(|device| {
                let dev_key = openrgb::device_profile_key(device);
                !self.config.is_device_disabled(&dev_key, &device.name)
            })
            .cloned()
            .collect()
    }

    fn enabled_device_keys(&self) -> Vec<String> {
        self.enabled_devices()
            .iter()
            .map(openrgb::device_profile_key)
            .collect()
    }

    fn toggle_current(&mut self) {
        if self.devices.is_empty() {
            return;
        }
        let device = self.devices[self.selected_index].clone();
        let dev_key = openrgb::device_profile_key(&device);
        if self.config.is_device_disabled(&dev_key, &device.name) {
            self.config
                .set_device_disabled(&dev_key, &device.name, false);
            *self.status_msg.lock().unwrap() = format!("Enabled {}", device.name);
        } else {
            self.config
                .set_device_disabled(&dev_key, &device.name, true);
            *self.status_msg.lock().unwrap() = format!("Disabled {}", device.name);
        }
        let _ = self.config.save();
    }

    fn toggle_sync(&mut self) {
        let enable_sync = !self.config.omarchy_sync_enabled;
        if enable_sync {
            if let Err(e) = hook::install_hook() {
                *self.status_msg.lock().unwrap() = format!("Error installing hook: {}", e);
            } else {
                let sync_devices = self.enabled_device_keys();
                self.config.omarchy_sync_enabled = true;
                self.config.set_omarchy_sync_devices(&sync_devices);
                let _ = self.config.save();
                *self.status_msg.lock().unwrap() = format!(
                    "Omarchy Sync Hook Installed for {}!",
                    format_sync_scope(sync_devices.len())
                );
            }
        } else {
            if let Err(e) = hook::remove_hook() {
                *self.status_msg.lock().unwrap() = format!("Error removing hook: {}", e);
            } else {
                self.config.omarchy_sync_enabled = false;
                self.config.omarchy_sync_devices.clear();
                let _ = self.config.save();
                *self.status_msg.lock().unwrap() = "Omarchy Sync Hook Removed!".to_string();
            }
        }
    }

    fn toggle_restore_on_startup(&mut self) {
        let enable_restore = !self.config.restore_on_startup;
        if enable_restore {
            if let Err(e) = hook::install_restore_autostart() {
                *self.status_msg.lock().unwrap() = format!("Error installing autostart: {}", e);
            } else {
                self.config.restore_on_startup = true;
                let _ = self.config.save();
                *self.status_msg.lock().unwrap() =
                    "Startup restore enabled via XDG autostart.".to_string();
            }
        } else if let Err(e) = hook::remove_restore_autostart() {
            *self.status_msg.lock().unwrap() = format!("Error removing autostart: {}", e);
        } else {
            self.config.restore_on_startup = false;
            let _ = self.config.save();
            *self.status_msg.lock().unwrap() = "Startup restore disabled.".to_string();
        }
    }

    fn force_rainbow(&mut self, tx: &mpsc::Sender<AppEvent>) {
        *self.status_msg.lock().unwrap() = "Setting Rainbow mode...".to_string();
        let mut devices = self.enabled_devices();
        if devices.is_empty() {
            *self.status_msg.lock().unwrap() =
                "All devices are disabled. Enable at least one device first.".to_string();
            return;
        }

        self.pending_state_update = Some(PendingStateUpdate {
            device_keys: self.enabled_device_keys(),
            state: SavedState::Rainbow,
        });
        thread::spawn({
            let tx = tx.clone();
            move || {
                let _ = openrgb::refresh_device_ids(&mut devices);
                let result = crate::core::set_rainbow_for_devices_summary(devices, true);
                let _ = tx.send(AppEvent::RainbowComplete(result));
            }
        });
    }

    fn force_off(&mut self, tx: &mpsc::Sender<AppEvent>) {
        *self.status_msg.lock().unwrap() = "Turning off lights...".to_string();
        let mut devices = self.enabled_devices();
        if devices.is_empty() {
            *self.status_msg.lock().unwrap() =
                "All devices are disabled. Enable at least one device first.".to_string();
            return;
        }

        let tx_clone = tx.clone();
        self.pending_state_update = Some(PendingStateUpdate {
            device_keys: self.enabled_device_keys(),
            state: SavedState::Off,
        });
        thread::spawn(move || {
            let _ = openrgb::refresh_device_ids(&mut devices);
            let result = crate::core::apply_color_to_devices_summary(devices, "000000", true);
            let _ = tx_clone.send(AppEvent::OffComplete(result));
        });
    }

    fn hex_to_rgb(hex: &str) -> Color {
        let hex = hex.trim_start_matches('#');
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255);
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255);
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255);
            Color::Rgb(r, g, b)
        } else {
            Color::Blue
        }
    }

    fn ui(&self, f: &mut ratatui::Frame) {
        let settings_height = if self.is_omarchy { 7 } else { 4 };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Length(settings_height),
                    Constraint::Min(5),
                    Constraint::Length(4),
                ]
                .as_ref(),
            )
            .split(f.size());

        let theme_color = Self::hex_to_rgb(&self.theme_color);

        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                "RGB PC ",
                Style::default()
                    .fg(theme_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("Device Manager"),
        ]))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme_color)),
        );
        f.render_widget(title, chunks[0]);

        let restore_status = if self.config.restore_on_startup {
            Span::styled(
                "ENABLED",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("DISABLED", Style::default().fg(Color::DarkGray))
        };

        let mut settings_lines = vec![Line::from(vec![
            Span::raw("Startup Restore: "),
            restore_status,
            Span::raw(" (press 'a')"),
        ])];
        settings_lines.push(Line::from(
            "Restores the whole remembered setup at login, not just devices currently checked.",
        ));

        if self.is_omarchy {
            let sync_status = if self.config.omarchy_sync_enabled {
                Span::styled(
                    "ENABLED",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled("DISABLED", Style::default().fg(Color::DarkGray))
            };

            settings_lines.push(Line::from(vec![
                Span::raw("Omarchy Theme Sync: "),
                sync_status,
                Span::raw(" (press 's')"),
            ]));
            if self.config.omarchy_sync_enabled {
                settings_lines.push(Line::from(format!(
                    "Sync scope locked to {}. Manual color/off/rainbow removes only affected devices, and removed devices stay out until you re-enable sync.",
                    format_sync_scope(self.config.omarchy_sync_devices.len())
                )));
            }
            settings_lines.push(Line::from(
                "Startup restore uses current Omarchy theme when sync is enabled.",
            ));
        } else {
            settings_lines.push(Line::from(
                "Startup restore reapplies the last successful manual color or off state.",
            ));
        }

        let settings = Paragraph::new(settings_lines)
            .block(Block::default().borders(Borders::ALL).title("Settings"));
        f.render_widget(settings, chunks[1]);

        let items: Vec<ListItem> = self
            .devices
            .iter()
            .enumerate()
            .map(|(i, dev)| {
                let dev_key = openrgb::device_profile_key(dev);
                let is_enabled = !self.config.is_device_disabled(&dev_key, &dev.name);
                let checkbox = if is_enabled { "[X]" } else { "[ ]" };

                let mut style = Style::default();
                if i == self.selected_index {
                    style = style
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD);
                } else if !is_enabled {
                    style = style.fg(Color::DarkGray);
                }

                let content = format!(" {} {} ({})", checkbox, dev.name, dev.device_type);
                ListItem::new(content).style(style)
            })
            .collect();

        let list =
            List::new(items).block(Block::default().borders(Borders::ALL).title(
                 "Hardware Devices (Space/Enter toggle | 'a' Startup restore | 'c' Color all enabled | 'R' Rescan)",
             ));
        f.render_widget(list, chunks[2]);

        let bottom_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(3)].as_ref())
            .split(chunks[3]);

        let mut shortcut_spans = vec![
            Span::styled(" j/k: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Move | "),
            Span::styled("a: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Startup Restore | "),
            Span::styled("c: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("All Enabled Color | "),
            Span::styled("R: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Rescan | "),
        ];
        if self.is_omarchy {
            shortcut_spans.extend([
                Span::styled("s: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("Sync | "),
                Span::styled("t: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("Force Sync | "),
            ]);
        }
        shortcut_spans.extend([
            Span::styled("r: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Rainbow | "),
            Span::styled("o: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Off | "),
            Span::styled("q: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Quit"),
        ]);
        let shortcuts = Paragraph::new(Line::from(shortcut_spans)).alignment(Alignment::Center);
        f.render_widget(shortcuts, bottom_chunks[0]);

        let msg = self.status_msg.lock().unwrap().clone();
        let footer = Paragraph::new(msg)
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .style(Style::default().fg(Color::Yellow));
        f.render_widget(footer, bottom_chunks[1]);

        if self.mode == AppMode::ColorPicker {
            let popup = Self::popup_area(f.size(), 44, 22);
            f.render_widget(Clear, popup);

            let block = Block::default()
                .title(" Pick Color for Enabled Devices ")
                .borders(Borders::ALL)
                .border_type(BorderType::Thick)
                .style(Style::default().bg(Color::Black));
            f.render_widget(block.clone(), popup);

            let inner_area = block.inner(popup);
            let pop_chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Length(12),
                    Constraint::Length(3),
                ])
                .split(inner_area);

            let instruction = Paragraph::new(
                "Use h/j/k/l or mouse to pick. Enter applies to all enabled devices.",
            )
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Gray));
            f.render_widget(instruction, pop_chunks[0]);

            let palette_rects = Self::get_palette_rects(pop_chunks[1]);
            for (i, &hex) in PALETTE.iter().enumerate() {
                let mut color_block =
                    Block::default().style(Style::default().bg(Self::hex_to_rgb(hex)));

                if i == self.selected_color_index && !self.input_active {
                    color_block = color_block
                        .borders(Borders::ALL)
                        .border_style(
                            Style::default()
                                .fg(Color::White)
                                .bg(Color::Black)
                                .add_modifier(Modifier::BOLD),
                        )
                        .border_type(BorderType::Thick);
                }

                f.render_widget(color_block, palette_rects[i]);
            }

            let input_style = if self.input_active {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            let input_block = Block::default()
                .title(" Custom HEX (Press 'i' to type) ")
                .borders(Borders::ALL)
                .border_style(input_style);

            let input_text = Paragraph::new(format!("#{}", self.custom_hex_input))
                .block(input_block)
                .alignment(Alignment::Center);
            f.render_widget(input_text, pop_chunks[2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::filter_device_keys;
    use crate::core::config::AppConfig;

    #[test]
    fn startup_restore_toggle_defaults_off() {
        let config = AppConfig::default();
        assert!(!config.restore_on_startup);
    }

    #[test]
    fn filter_device_keys_keeps_only_successful_keys() {
        let requested = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let succeeded = vec!["b".to_string(), "c".to_string()];

        assert_eq!(filter_device_keys(&requested, &succeeded), vec!["b", "c"]);
    }
}
