use crate::core::{
    config::AppConfig,
    hook,
    openrgb::{self, OpenRgbDevice},
};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
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
    Tick,
}

pub struct App {
    config: AppConfig,
    devices: Vec<OpenRgbDevice>,
    selected_index: usize,
    status_msg: Arc<Mutex<String>>,
    theme_color: String,
    is_syncing: bool,
}

impl App {
    pub fn new() -> Self {
        let devices = openrgb::list_devices().unwrap_or_default();
        let config = AppConfig::load();

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
            status_msg: Arc::new(Mutex::new("Press 's' to toggle Sync, 'Enter'/'Space' to toggle Device, 't' to force sync, 'r' for rainbow, 'q' to quit.".to_string())),
            theme_color: hex_color,
            is_syncing: false,
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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

            // Non-blocking event check to allow MPSC messages to process
            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Down | KeyCode::Char('j') => self.next(),
                        KeyCode::Up | KeyCode::Char('k') => self.previous(),
                        KeyCode::Enter | KeyCode::Char(' ') => self.toggle_current(),
                        KeyCode::Char('s') => self.toggle_sync(),
                        KeyCode::Char('t') => {
                            if !self.is_syncing {
                                self.is_syncing = true;
                                *self.status_msg.lock().unwrap() =
                                    "Syncing devices... please wait.".to_string();

                                let tx_clone = tx.clone();
                                thread::spawn(move || {
                                    let res = crate::core::perform_sync(true);
                                    let _ = tx_clone.send(AppEvent::SyncComplete(res));
                                });
                            }
                        }
                        KeyCode::Char('o') => self.force_off(),
                        KeyCode::Char('r') => self.force_rainbow(),
                        _ => {}
                    }
                }
            }

            // Process async events
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
                    AppEvent::Tick => {} // Just to wake up and redraw
                }
            }
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Length(4),
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
        f.render_widget(sync_info, chunks[1]);

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
                .title("Hardware Devices (Space/Enter to toggle)"),
        );
        f.render_widget(list, chunks[2]);

        let bottom_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(3)].as_ref())
            .split(chunks[3]);

        let shortcuts = Paragraph::new(Line::from(vec![
            Span::styled(" j/k: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Move | "),
            Span::styled(
                "Space/Enter: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("Toggle | "),
            Span::styled("s: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Sync Hook | "),
            Span::styled("t: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Force Sync | "),
            Span::styled("r: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Rainbow | "),
            Span::styled("o: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Lights Off | "),
            Span::styled("q: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Quit"),
        ]))
        .alignment(Alignment::Center);
        f.render_widget(shortcuts, bottom_chunks[0]);

        let msg = self.status_msg.lock().unwrap().clone();
        let footer = Paragraph::new(msg)
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .style(Style::default().fg(Color::Yellow));
        f.render_widget(footer, bottom_chunks[1]);
    }
}
