use regex::Regex;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

static SERVER_MODE: AtomicBool = AtomicBool::new(false);

pub(super) fn set_server_mode(enabled: bool) {
    SERVER_MODE.store(enabled, Ordering::SeqCst);
}

pub(super) fn server_mode() -> bool {
    SERVER_MODE.load(Ordering::SeqCst)
}

pub(super) fn base_command() -> Command {
    let mut command = Command::new("openrgb");
    // A closed terminal or killed RGBPC must not leave a hardware owner.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = std::process::id();
        // Only async-signal-safe syscalls run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent as libc::pid_t {
                    return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
    command
}

pub(super) fn command() -> Command {
    let mut command = base_command();
    // A lost server must not turn a cached device ID into a raw hardware write.
    command.arg(if server_mode() {
        "--nodetect"
    } else {
        "--noautoconnect"
    });
    command
}

const RAINBOW_MODES: &[&str] = &["Rainbow wave", "Spectrum Cycle", "Rainbow Circle"];

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
    // Callers retain the action lock, so a cooperating owner cannot stop the
    // server between this check and discovery/writes. Never accept an empty
    // --nodetect roster merely because a borrowed server vanished.
    super::session::validate_transport()?;
    let output = command()
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
    remap_device_ids(devices, &latest_devices)
}

fn remap_device_ids(
    devices: &mut [OpenRgbDevice],
    latest_devices: &[OpenRgbDevice],
) -> Result<(), String> {
    use std::collections::HashMap;
    let mut requested = HashMap::new();
    for device in devices.iter() {
        *requested
            .entry(device_profile_key(device))
            .or_insert(0usize) += 1;
    }
    let mut used = HashMap::new();
    let mut replacements = Vec::new();
    for device in devices.iter() {
        let key = device_profile_key(device);
        let matches: Vec<_> = latest_devices
            .iter()
            .filter(|candidate| device_profile_key(candidate) == key)
            .collect();
        if matches.is_empty() {
            return Err(format!(
                "{} is no longer present; rescan devices before applying",
                device.name
            ));
        }
        if requested[&key] != matches.len() {
            return Err(format!(
                "{} has an ambiguous or changed controller identity; rescan before applying",
                device.name
            ));
        }
        let offset = used.entry(key).or_insert(0usize);
        replacements.push(matches[*offset].clone());
        *offset += 1;
    }
    for (device, replacement) in devices.iter_mut().zip(replacements) {
        *device = replacement;
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

    match finalize_attempts(false, last_error) {
        Ok(_) => Err("All color application strategies failed silently".to_string()),
        Err(err) => Err(err),
    }
}

fn strategy_attempts_for_device(device: &OpenRgbDevice) -> Vec<StrategyAttempt> {
    let name = device.name.to_ascii_lowercase();
    let device_type = device.device_type.to_ascii_lowercase();

    let board_like = name.contains("msi")
        || device_type.contains("motherboard")
        || device_type.contains("mainboard")
        || device_type.contains("ledstrip");
    let mouse_like = device_type.contains("mouse") || name.contains("mouse");
    let keyboard_like = device_type.contains("keyboard") || name.contains("keyboard");

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

    if keyboard_like {
        return vec![
            StrategyAttempt {
                kind: StrategyKind::CustomThenColor,
                needs_retry: false,
            },
            StrategyAttempt {
                kind: StrategyKind::StaticThenColor,
                needs_retry: false,
            },
            StrategyAttempt {
                kind: StrategyKind::ColorOnly,
                needs_retry: false,
            },
            StrategyAttempt {
                kind: StrategyKind::FullFallback,
                needs_retry: true,
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
    CustomThenColor,
    DirectThenColor,
    StaticThenColor,
    ZoneResizeThenColor,
    FullFallback,
}

fn apply_strategy(device_id: u32, color: &str, strategy: StrategyKind) -> Result<bool, String> {
    match strategy {
        StrategyKind::ColorOnly => apply_color_only(device_id, color),
        StrategyKind::CustomThenColor => apply_custom_then_color(device_id, color),
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

fn apply_custom_then_color(device_id: u32, color: &str) -> Result<bool, String> {
    let id = device_id.to_string();
    run_openrgb(&["-d", id.as_str(), "-m", "Custom", "-c", color])
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
    let output = command()
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute openrgb: {}", e))?;

    Ok(output.status.success())
}

pub fn set_rainbow(device: &OpenRgbDevice) -> Result<(), String> {
    let id = device.id.to_string();
    let mut last_error = None;

    for mode in RAINBOW_MODES {
        match command().args(["-d", id.as_str(), "-m", mode]).output() {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(_) => {}
            Err(err) => remember_error(&mut last_error, format!("{}: {}", device.name, err)),
        }
    }

    if let Some(err) = last_error {
        Err(err)
    } else {
        Err(format!("{}: no rainbow mode succeeded", device.name))
    }
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
    fn remap_rejects_missing_targets_instead_of_reusing_old_numeric_ids() {
        let mut selected = vec![OpenRgbDevice {
            id: 0,
            name: "Old board".to_string(),
            device_type: "Motherboard".to_string(),
        }];
        let latest = vec![OpenRgbDevice {
            id: 0,
            name: "New GPU".to_string(),
            device_type: "GPU".to_string(),
        }];
        assert!(super::remap_device_ids(&mut selected, &latest).is_err());
        assert_eq!(selected[0].name, "Old board");
    }

    #[test]
    fn remap_updates_all_identical_controllers_without_collapsing_to_the_first() {
        let mut selected = vec![
            OpenRgbDevice {
                id: 0,
                name: "Hub".to_string(),
                device_type: "LEDStrip".to_string(),
            },
            OpenRgbDevice {
                id: 1,
                name: "Hub".to_string(),
                device_type: "LEDStrip".to_string(),
            },
        ];
        let mut latest = selected.clone();
        latest[0].id = 4;
        latest[1].id = 6;
        assert!(super::remap_device_ids(&mut selected, &latest).is_ok());
        assert_eq!(
            selected.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![4, 6]
        );
        assert!(super::remap_device_ids(&mut selected[..1], &latest).is_err());
    }

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
    fn keyboard_devices_prefer_custom_mode_first() {
        let device = OpenRgbDevice {
            id: 1,
            name: "EVision Keyboard 0C45:7698".to_string(),
            device_type: "Keyboard".to_string(),
        };

        let attempts = strategy_attempts_for_device(&device);
        assert_eq!(attempts[0].kind, StrategyKind::CustomThenColor);
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
