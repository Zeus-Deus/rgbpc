mod cli;
mod core;
mod ui;

use cli::{Action, Options};
use core::config::{AppConfig, SavedState};
use serde_json::json;
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let options = match cli::parse(&args) {
        Ok(options) => options,
        Err(error) => {
            emit_error(json_requested(&args), "arguments", &error);
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            emit_error(options.json, action_name(&options.action), &error);
            ExitCode::from(1)
        }
    }
}

fn json_requested(args: &[String]) -> bool {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--color" | "--device" => {
                args.next();
            }
            "--json" => return true,
            _ => {}
        }
    }
    false
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Tui => "tui",
        Action::ListDevices => "list-devices",
        Action::Color(_) => "color",
        Action::Off => "off",
        Action::Rainbow => "rainbow",
        Action::SyncTheme => "sync-theme",
        Action::RestoreLast => "restore-last",
        Action::Help => "help",
    }
}

fn emit_error(as_json: bool, action: &str, error: &str) {
    if as_json {
        println!(
            "{}",
            json!({"schema_version": 1, "ok": false, "action": action,
            "accepted_devices": [], "errors": [error], "verification": "command_acceptance"})
        );
    } else {
        eprintln!("RGBPC: {error}");
    }
}

fn emit(as_json: bool, report: serde_json::Value, human: &str) {
    if as_json {
        println!("{report}");
    } else {
        println!("{human}");
    }
}

fn emit_summary(options: &Options, transport: &str, summary: core::DeviceActionSummary) -> u8 {
    let ok = summary.failed_devices.is_empty();
    let human = if ok {
        format!(
            "Commands accepted for {} target(s).",
            summary.succeeded_keys.len()
        )
    } else {
        format!(
            "Some commands failed: {}",
            summary.failed_devices.join(" | ")
        )
    };
    emit(
        options.json,
        json!({"schema_version": 1, "ok": ok, "action": action_name(&options.action),
        "backend": transport, "accepted_devices": summary.succeeded_keys,
        "errors": summary.failed_devices, "verification": "command_acceptance"}),
        &human,
    );
    if ok {
        0
    } else {
        1
    }
}

fn run(options: &Options) -> Result<u8, String> {
    let automatic = !options.standalone;
    let action = action_name(&options.action);
    if options.action == Action::Help {
        print!("{}", cli::HELP);
        return Ok(0);
    }
    if options.action == Action::RestoreLast {
        core::perform_restore(automatic)?;
        emit(
            options.json,
            json!({"schema_version": 1, "ok": true, "action": action,
            "result": "completed", "verification": "command_acceptance"}),
            "Startup restore completed (if enabled).",
        );
        return Ok(0);
    }
    if options.action == Action::SyncTheme && !AppConfig::load().omarchy_sync_enabled {
        emit(
            options.json,
            json!({"schema_version": 1, "ok": true, "action": action,
            "skipped": true, "reason": "theme_sync_disabled"}),
            "Theme sync is disabled; no devices changed.",
        );
        return Ok(0);
    }

    let mut backend = core::session::Backend::new(automatic);
    if options.action == Action::Tui {
        if let Some(warning) = &backend.warning {
            eprintln!("OpenRGB connection warning: {warning}");
        }
        let backend = std::sync::Arc::new(std::sync::Mutex::new(backend));
        let mut app = ui::app::App::new(backend);
        return match app.run() {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                // App::run has restored the terminal. Force quit bypasses a
                // potentially blocked Drop; Linux PDEATHSIG stops our children.
                std::process::exit(130);
            }
            result => result.map(|_| 0).map_err(|e| e.to_string()),
        };
    }
    if let Some(warning) = &backend.warning {
        eprintln!("OpenRGB connection warning: {warning}");
    }
    match &options.action {
        Action::ListDevices => {
            let devices = backend.run(|| {
                core::with_hardware_lock(|| {
                    let config = AppConfig::load();
                    let devices = core::openrgb::list_devices()?;
                    let selected =
                        core::select_devices(&devices, &options.devices, &config, false)?;
                    Ok(selected
                        .into_iter()
                        .map(|device| {
                            let key = core::openrgb::device_profile_key(&device);
                            json!({"id": device.id, "name": device.name, "type": device.device_type,
                        "enabled": !config.is_device_disabled(&key, &device.name), "key": key})
                        })
                        .collect::<Vec<_>>())
                })
            })?;
            let human = devices
                .iter()
                .map(|device| {
                    format!(
                        "{}: {} [{}] {}",
                        device["id"],
                        device["name"].as_str().unwrap_or_default(),
                        if device["enabled"] == true {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        device["key"].as_str().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            emit(
                options.json,
                json!({"schema_version": 1, "ok": true, "action": action,
                "backend": backend.label(), "devices": devices}),
                &human,
            );
            Ok(0)
        }
        Action::SyncTheme => {
            let summary = backend.run(|| core::perform_sync_summary(false))?;
            Ok(emit_summary(options, backend.label(), summary))
        }
        Action::Color(_) | Action::Off | Action::Rainbow => {
            let state = match &options.action {
                Action::Color(hex) => SavedState::Color { hex: hex.clone() },
                Action::Off => SavedState::Off,
                _ => SavedState::Rainbow,
            };
            let summary =
                backend.run(|| core::manual_cli_action(state.clone(), &options.devices))?;
            Ok(emit_summary(options, backend.label(), summary))
        }
        _ => unreachable!("help, TUI and restore are handled before backend creation"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn literal_selector_named_json_does_not_request_json_output() {
        let args = ["--off", "--device", "--json"].map(str::to_string);
        assert!(!super::json_requested(&args));
        let args = ["--json", "--off", "--device", "--json"].map(str::to_string);
        assert!(super::json_requested(&args));
    }
}
