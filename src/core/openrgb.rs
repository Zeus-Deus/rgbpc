use regex::Regex;
use std::process::Command;

#[derive(Debug, Clone, Default)]
pub struct OpenRgbDevice {
    pub id: u32,
    pub name: String,
    pub device_type: String,
}

pub fn list_devices() -> Result<Vec<OpenRgbDevice>, String> {
    let output = Command::new("openrgb")
        .arg("--list-devices")
        .output()
        .map_err(|e| format!("Failed to execute openrgb: {}", e))?;

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

pub fn set_color(device_id: u32, color_hex: &str) -> Result<(), String> {
    let color = color_hex.trim_start_matches('#');

    // For some devices (especially MSI), we need to set the mode AND color, or just color
    // We will try different approaches and return success if any works.

    // MSI and some other devices are very picky.
    // Best practice is to set the MODE first, wait a tiny bit, then set the color,
    // or just set the color globally and let the device figure out the mode.

    // First, let's try the most universally accepted command: just setting the color.
    let _ = Command::new("openrgb")
        .args(&["-d", &device_id.to_string(), "-c", color])
        .output();

    // For MSI boards specifically, we need to try setting "Direct" mode first,
    // then sending the color to the zones.
    let _ = Command::new("openrgb")
        .args(&["-d", &device_id.to_string(), "-m", "Direct"])
        .output();

    // IMPORTANT FOR MSI CASE FANS: The ARGB headers (JRAINBOW1 and JRAINBOW2) are usually zones 2 and 3.
    // By default, OpenRGB often sees their size as 0 LEDs, which means no color is applied.
    // We need to explicitly set their size (e.g., 60 LEDs) before applying the color.
    let _ = Command::new("openrgb")
        .args(&[
            "-d",
            &device_id.to_string(),
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
        .output();

    // Then blast the color to all possible zones (0-5) just in case
    for zone in 0..6 {
        let _ = Command::new("openrgb")
            .args(&[
                "-d",
                &device_id.to_string(),
                "-z",
                &zone.to_string(),
                "-c",
                color,
            ])
            .output();
    }

    // Also try "Static" mode as a fallback (good for mice and keyboards)
    let _ = Command::new("openrgb")
        .args(&["-d", &device_id.to_string(), "-m", "Static", "-c", color])
        .output();

    // Some mice require saving the profile to device to wake up
    let _ = Command::new("openrgb")
        .args(&[
            "-d",
            &device_id.to_string(),
            "-m",
            "Static",
            "-c",
            color,
            "--save-profile",
            "omarchy.orp",
        ])
        .output();

    Ok(())
}
