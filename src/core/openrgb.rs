use regex::Regex;
use std::process::Command;

#[derive(Debug, Clone, Default)]
pub struct OpenRgbDevice {
    pub id: u32,
    pub name: String,
    pub device_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyResult {
    pub needs_retry: bool,
}

pub fn device_profile_key(device: &OpenRgbDevice) -> String {
    format!(
        "{}::{}",
        normalize_key_part(&device.name),
        normalize_key_part(&device.device_type)
    )
}

fn normalize_key_part(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub fn list_devices() -> Result<Vec<OpenRgbDevice>, String> {
    let output = Command::new("openrgb")
        .arg("--list-devices")
        .output()
        .map_err(|e| format!("Failed to execute openrgb: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        if message.is_empty() {
            return Err("openrgb --list-devices failed".to_string());
        }
        return Err(format!("openrgb --list-devices failed: {}", message));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut devices = Vec::new();
    let re = Regex::new(r"(?m)^(\d+):\s+(.*)$").unwrap();
    let type_re = Regex::new(r"(?m)^\s+Type:\s+(.*)$").unwrap();

    let mut current_device: Option<OpenRgbDevice> = None;

    for line in stdout.lines() {
        if let Some(caps) = re.captures(line) {
            if let Some(device) = current_device.take() {
                devices.push(device);
            }
            let id = caps[1].parse::<u32>().unwrap_or(0);
            let name = caps[2].trim().to_string();
            current_device = Some(OpenRgbDevice {
                id,
                name,
                device_type: String::new(),
            });
        } else if let Some(caps) = type_re.captures(line) {
            if let Some(device) = current_device.as_mut() {
                device.device_type = caps[1].trim().to_string();
            }
        }
    }

    if let Some(device) = current_device {
        devices.push(device);
    }

    Ok(devices)
}

pub fn refresh_device_ids(devices: &mut [OpenRgbDevice]) -> Result<(), String> {
    let latest_devices = list_devices()?;
    for device in devices {
        if let Some(updated) = latest_devices
            .iter()
            .find(|candidate| device_profile_key(candidate) == device_profile_key(device))
        {
            device.id = updated.id;
            device.name = updated.name.clone();
            device.device_type = updated.device_type.clone();
        }
    }
    Ok(())
}

pub fn apply_color(device: &OpenRgbDevice, color_hex: &str) -> Result<ApplyResult, String> {
    let color = color_hex.trim_start_matches('#');
    let attempts = strategy_attempts_for_device(device);
    let mut last_error = None;

    for attempt in attempts {
        match apply_strategy(device.id, color, attempt.kind) {
            Ok(true) => {
                return Ok(ApplyResult {
                    needs_retry: attempt.needs_retry,
                });
            }
            Ok(false) => {}
            Err(err) => remember_error(&mut last_error, err),
        }
    }

    finalize_attempts(false, last_error).map(|_| ApplyResult { needs_retry: false })
}

fn strategy_attempts_for_device(device: &OpenRgbDevice) -> Vec<StrategyAttempt> {
    let name = device.name.to_ascii_lowercase();
    let device_type = device.device_type.to_ascii_lowercase();

    let board_like = name.contains("msi")
        || device_type.contains("motherboard")
        || device_type.contains("mainboard")
        || device_type.contains("ledstrip");
    let mouse_like = device_type.contains("mouse") || name.contains("mouse");

    if board_like {
        return vec![
            StrategyAttempt {
                kind: StrategyKind::FullFallback,
                needs_retry: true,
            },
            StrategyAttempt {
                kind: StrategyKind::ZoneResizeThenColor,
                needs_retry: true,
            },
            StrategyAttempt {
                kind: StrategyKind::DirectThenColor,
                needs_retry: true,
            },
            StrategyAttempt {
                kind: StrategyKind::ColorOnly,
                needs_retry: true,
            },
            StrategyAttempt {
                kind: StrategyKind::StaticThenColor,
                needs_retry: true,
            },
        ];
    }

    if mouse_like {
        return vec![
            StrategyAttempt {
                kind: StrategyKind::StaticThenColor,
                needs_retry: false,
            },
            StrategyAttempt {
                kind: StrategyKind::FullFallback,
                needs_retry: true,
            },
            StrategyAttempt {
                kind: StrategyKind::ColorOnly,
                needs_retry: false,
            },
            StrategyAttempt {
                kind: StrategyKind::DirectThenColor,
                needs_retry: false,
            },
        ];
    }

    vec![
        StrategyAttempt {
            kind: StrategyKind::ColorOnly,
            needs_retry: false,
        },
        StrategyAttempt {
            kind: StrategyKind::DirectThenColor,
            needs_retry: false,
        },
        StrategyAttempt {
            kind: StrategyKind::StaticThenColor,
            needs_retry: false,
        },
        StrategyAttempt {
            kind: StrategyKind::FullFallback,
            needs_retry: true,
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StrategyAttempt {
    kind: StrategyKind,
    needs_retry: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrategyKind {
    ColorOnly,
    DirectThenColor,
    StaticThenColor,
    ZoneResizeThenColor,
    FullFallback,
}

fn apply_strategy(device_id: u32, color: &str, strategy: StrategyKind) -> Result<bool, String> {
    match strategy {
        StrategyKind::ColorOnly => apply_color_only(device_id, color),
        StrategyKind::DirectThenColor => apply_direct_then_color(device_id, color),
        StrategyKind::StaticThenColor => apply_static_then_color(device_id, color),
        StrategyKind::ZoneResizeThenColor => apply_zone_resize_then_color(device_id, color),
        StrategyKind::FullFallback => apply_full_fallback(device_id, color),
    }
}

fn apply_full_fallback(device_id: u32, color: &str) -> Result<bool, String> {
    let mut any_succeeded = false;
    let mut last_error = None;

    record_attempt_result(
        apply_color_only(device_id, color),
        &mut any_succeeded,
        &mut last_error,
    );
    note_attempt_error(set_direct_mode(device_id), &mut last_error);
    record_attempt_result(
        apply_zone_resize_then_color(device_id, color),
        &mut any_succeeded,
        &mut last_error,
    );
    record_attempt_result(
        apply_zone_blast(device_id, color),
        &mut any_succeeded,
        &mut last_error,
    );
    record_attempt_result(
        apply_static_then_color(device_id, color),
        &mut any_succeeded,
        &mut last_error,
    );
    record_attempt_result(
        apply_static_with_profile_save(device_id, color),
        &mut any_succeeded,
        &mut last_error,
    );

    finalize_attempts(any_succeeded, last_error)
}

fn apply_color_only(device_id: u32, color: &str) -> Result<bool, String> {
    let id = device_id.to_string();
    run_openrgb(&["-d", id.as_str(), "-c", color])
}

fn apply_direct_then_color(device_id: u32, color: &str) -> Result<bool, String> {
    let _ = set_direct_mode(device_id)?;
    let id = device_id.to_string();
    run_openrgb(&["-d", id.as_str(), "-c", color])
}

fn set_direct_mode(device_id: u32) -> Result<bool, String> {
    let id = device_id.to_string();
    run_openrgb(&["-d", id.as_str(), "-m", "Direct"])
}

fn apply_zone_resize_then_color(device_id: u32, color: &str) -> Result<bool, String> {
    let id = device_id.to_string();
    run_openrgb(&[
        "-d",
        id.as_str(),
        "-z",
        "2",
        "-sz",
        "60",
        "-c",
        color,
        "-z",
        "3",
        "-sz",
        "60",
        "-c",
        color,
    ])
}

fn apply_zone_blast(device_id: u32, color: &str) -> Result<bool, String> {
    let id = device_id.to_string();
    let mut any_succeeded = false;
    let mut last_error = None;

    for zone in 0..6 {
        let zone_str = zone.to_string();
        record_attempt_result(
            run_openrgb(&["-d", id.as_str(), "-z", zone_str.as_str(), "-c", color]),
            &mut any_succeeded,
            &mut last_error,
        );
    }

    finalize_attempts(any_succeeded, last_error)
}

fn apply_static_then_color(device_id: u32, color: &str) -> Result<bool, String> {
    let id = device_id.to_string();
    run_openrgb(&["-d", id.as_str(), "-m", "Static", "-c", color])
}

fn apply_static_with_profile_save(device_id: u32, color: &str) -> Result<bool, String> {
    let id = device_id.to_string();
    run_openrgb(&[
        "-d",
        id.as_str(),
        "-m",
        "Static",
        "-c",
        color,
        "--save-profile",
        "omarchy.orp",
    ])
}

fn run_openrgb(args: &[&str]) -> Result<bool, String> {
    let output = Command::new("openrgb")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute openrgb: {}", e))?;

    Ok(output.status.success())
}

fn record_attempt_result(
    result: Result<bool, String>,
    any_succeeded: &mut bool,
    last_error: &mut Option<String>,
) {
    match result {
        Ok(success) => *any_succeeded |= success,
        Err(err) => remember_error(last_error, err),
    }
}

fn note_attempt_error(result: Result<bool, String>, last_error: &mut Option<String>) {
    if let Err(err) = result {
        remember_error(last_error, err);
    }
}

fn remember_error(last_error: &mut Option<String>, err: String) {
    if last_error.is_none() {
        *last_error = Some(err);
    }
}

fn finalize_attempts(any_succeeded: bool, last_error: Option<String>) -> Result<bool, String> {
    if any_succeeded {
        Ok(true)
    } else if let Some(err) = last_error {
        Err(err)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        device_profile_key, finalize_attempts, record_attempt_result, strategy_attempts_for_device,
        OpenRgbDevice, StrategyKind,
    };

    #[test]
    fn device_profile_key_is_stable_for_case_and_whitespace() {
        let device = OpenRgbDevice {
            id: 1,
            name: "  My GPU  ".to_string(),
            device_type: " Graphics Card ".to_string(),
        };

        assert_eq!(device_profile_key(&device), "my gpu::graphics card");
    }

    #[test]
    fn mouse_devices_prefer_static_mode_first() {
        let device = OpenRgbDevice {
            id: 1,
            name: "G502 Lightspeed".to_string(),
            device_type: "Mouse".to_string(),
        };

        let attempts = strategy_attempts_for_device(&device);
        assert_eq!(attempts[0].kind, StrategyKind::StaticThenColor);
    }

    #[test]
    fn motherboard_devices_prefer_full_fallback_first() {
        let device = OpenRgbDevice {
            id: 1,
            name: "MSI MAG B650".to_string(),
            device_type: "Motherboard".to_string(),
        };

        let attempts = strategy_attempts_for_device(&device);
        assert_eq!(attempts[0].kind, StrategyKind::FullFallback);
    }

    #[test]
    fn finalize_attempts_returns_success_if_any_attempt_succeeded() {
        let mut any_succeeded = false;
        let mut last_error = None;

        record_attempt_result(Err("boom".to_string()), &mut any_succeeded, &mut last_error);
        record_attempt_result(Ok(true), &mut any_succeeded, &mut last_error);

        assert_eq!(finalize_attempts(any_succeeded, last_error), Ok(true));
    }
}
