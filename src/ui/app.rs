use crate::core::{
    config::AppConfig,
    hook,
    openrgb::{self, OpenRgbDevice},
};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, size as term_size, EnterAlternateScreen,
        LeaveAlternateScreen, SetTitle,
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
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

pub enum AppEvent {
    SyncComplete(Result<(), String>),
    ColorSetComplete(Result<(), String>),
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
}

impl App {
    pub fn new() -> Self {
        let devices = openrgb::list_devices().unwrap_or_default();
        let config = AppConfig::load();

        let mut omarchy_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        omarchy_dir.push(".config/omarchy");
        let is_omarchy = omarchy_dir.is_dir();

        let mut hex_color = String::from("7aa2f7");
        let mut theme_path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        theme_path.push(".config/omarchy/current/theme/colors.toml");
        if let Ok(content) = fs::read_to_string(&theme_path) {
            for line in content.lines() {
                if line.starts_with("rgb") || line.starts_with("accent") {
                    let parts: Vec<&str> = line.split('=').collect();
                    if parts.len() == 2 {
                        hex_color = parts[1]
                            .trim()
                            .trim_matches('"')
                            .trim_start_matches('#')
                            .to_string();
                        if line.starts_with("rgb") {
                            break;
                        }
                    }
                }
            }
        }

        Self {
            config,
            devices,
            selected_index: 0,
            status_msg: Arc::new(Mutex::new(if is_omarchy {
                "Press 's' toggle Sync, 'Enter'/'Space' toggle Device, 'c' manual Color, 't' Force sync, 'r' rainbow, 'q' quit.".to_string()
            } else {
                "Press 'Enter'/'Space' toggle Device, 'c' manual Color, 'r' rainbow, 'o' off, 'q' quit.".to_string()
            })),
            theme_color: hex_color,
            is_syncing: false,
            mode: AppMode::Normal,
            selected_color_index: 0,
            custom_hex_input: String::new(),
            input_active: false,
            is_omarchy,
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
                                KeyCode::Char('s') if self.is_omarchy => self.toggle_sync(),
                                KeyCode::Char('c') => self.open_color_picker(),
                                KeyCode::Char('t') if self.is_omarchy => self.force_sync(&tx),
                                KeyCode::Char('o') => self.force_off(),
                                KeyCode::Char('r') => self.force_rainbow(),
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
                    AppEvent::SyncComplete(res) => {
                        self.is_syncing = false;
                        match res {
                            Ok(_) => {
                                *self.status_msg.lock().unwrap() = "Sync complete!".to_string()
                            }
                            Err(e) => {
                                *self.status_msg.lock().unwrap() = format!("Sync failed: {}", e)
                            }
                        }
                    }
                    AppEvent::ColorSetComplete(res) => match res {
                        Ok(_) => {
                            *self.status_msg.lock().unwrap() =
                                "Color applied successfully!".to_string()
                        }
                        Err(e) => {
                            *self.status_msg.lock().unwrap() =
                                format!("Failed to apply color: {}", e)
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
        self.mode = AppMode::ColorPicker;
        self.selected_color_index = 0;
        self.custom_hex_input.clear();
        self.input_active = false;
        *self.status_msg.lock().unwrap() =
            "Pick a color or press 'i' to type HEX. Esc to cancel.".to_string();
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
        let dev_id = self.devices[self.selected_index].id;

        *self.status_msg.lock().unwrap() = format!("Applying color #{}...", color);
        let tx_clone = tx.clone();
        thread::spawn(move || {
            let res = crate::core::openrgb::set_color(dev_id, &color)
                .map(|_| ())
                .map_err(|e| e.to_string());
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
            let dev_id = self.devices[self.selected_index].id;

            *self.status_msg.lock().unwrap() = format!("Applying custom color #{}...", color);
            let tx_clone = tx.clone();
            thread::spawn(move || {
                let res = crate::core::openrgb::set_color(dev_id, &color)
                    .map(|_| ())
                    .map_err(|e| e.to_string());
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

    fn toggle_current(&mut self) {
        if self.devices.is_empty() {
            return;
        }
        let dev_name = self.devices[self.selected_index].name.clone();
        if self.config.disabled_devices.contains(&dev_name) {
            self.config.disabled_devices.remove(&dev_name);
            *self.status_msg.lock().unwrap() = format!("Enabled {}", dev_name);
        } else {
            self.config.disabled_devices.insert(dev_name.clone());
            *self.status_msg.lock().unwrap() = format!("Disabled {}", dev_name);
        }
        let _ = self.config.save();
    }

    fn toggle_sync(&mut self) {
        self.config.omarchy_sync_enabled = !self.config.omarchy_sync_enabled;
        let _ = self.config.save();
        if self.config.omarchy_sync_enabled {
            if let Err(e) = hook::install_hook() {
                *self.status_msg.lock().unwrap() = format!("Error installing hook: {}", e);
                self.config.omarchy_sync_enabled = false;
            } else {
                *self.status_msg.lock().unwrap() = "Omarchy Sync Hook Installed!".to_string();
            }
        } else {
            if let Err(e) = hook::remove_hook() {
                *self.status_msg.lock().unwrap() = format!("Error removing hook: {}", e);
            } else {
                *self.status_msg.lock().unwrap() = "Omarchy Sync Hook Removed!".to_string();
            }
        }
    }

    fn force_rainbow(&mut self) {
        *self.status_msg.lock().unwrap() = "Setting Rainbow mode...".to_string();
        for dev in &self.devices {
            if !self.config.disabled_devices.contains(&dev.name) {
                let _ = std::process::Command::new("openrgb")
                    .args(&["-d", &dev.id.to_string(), "-m", "Rainbow wave"])
                    .output();
                let _ = std::process::Command::new("openrgb")
                    .args(&["-d", &dev.id.to_string(), "-m", "Spectrum Cycle"])
                    .output();
                let _ = std::process::Command::new("openrgb")
                    .args(&["-d", &dev.id.to_string(), "-m", "Rainbow Circle"])
                    .output();
            }
        }
        *self.status_msg.lock().unwrap() = "Rainbow mode command sent!".to_string();
    }

    fn force_off(&mut self) {
        *self.status_msg.lock().unwrap() = "Turning off lights...".to_string();
        for dev in &self.devices {
            if !self.config.disabled_devices.contains(&dev.name) {
                let _ = crate::core::openrgb::set_color(dev.id, "000000");
            }
        }
        *self.status_msg.lock().unwrap() = "Lights turned off!".to_string();
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
        let settings_height = if self.is_omarchy { 4 } else { 0 };
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

        let sync_status = if self.config.omarchy_sync_enabled {
            Span::styled(
                "ENABLED (Auto-Syncing with Omarchy)",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("DISABLED", Style::default().fg(Color::DarkGray))
        };
        let sync_info = Paragraph::new(Line::from(vec![
            Span::raw("Omarchy Theme Sync: "),
            sync_status,
        ]))
        .block(Block::default().borders(Borders::ALL).title("Settings"));
        if self.is_omarchy {
            f.render_widget(sync_info, chunks[1]);
        }

        let items: Vec<ListItem> = self
            .devices
            .iter()
            .enumerate()
            .map(|(i, dev)| {
                let is_enabled = !self.config.disabled_devices.contains(&dev.name);
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

        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Hardware Devices (Space/Enter toggle | 'c' Manual Color)"),
        );
        f.render_widget(list, chunks[2]);

        let bottom_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(3)].as_ref())
            .split(chunks[3]);

        let mut shortcut_spans = vec![
            Span::styled(" j/k: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Move | "),
            Span::styled("c: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Color | "),
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
                .title(format!(
                    " Pick Color for {} ",
                    self.devices
                        .get(self.selected_index)
                        .map(|d| d.name.as_str())
                        .unwrap_or("Device")
                ))
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

            let instruction = Paragraph::new("Use h/j/k/l or mouse to pick. Enter to apply.")
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
