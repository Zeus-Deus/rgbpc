# RGBPC

A sleek Terminal User Interface (TUI) application designed to sync your PC's RGB lighting (Motherboard, Mouse, Keyboard, etc.) seamlessly with Omarchy themes via OpenRGB.

## Features

- **Safe Device Management:** Auto-detects all RGB controllers. Easily disable sensitive or incompatible devices (like specific GPUs) so they are left completely untouched.
- **Omarchy Theme Sync:** Automatically reads your `colors.toml` and applies your current theme's accent color to your PC hardware dynamically!
- **Auto-Hook Installation:** One-click install for the `~/.config/omarchy/hooks/theme-set` hook.
- **Fail-Safe Application:** Implements fallbacks for MSI motherboards and various OpenRGB mode quirks (`Static`, `Direct`, and default modes).

## Installation

### Prerequisites
Ensure you have `openrgb` installed, configured, and running.
```bash
sudo pacman -S openrgb
```

### From AUR (Recommended)
You can easily install `rgbpc` from the Arch User Repository using your favorite helper:
```bash
yay -S rgbpc
```

### Build from source
```bash
git clone https://github.com/Zeus-Deus/rgbpc.git
cd rgbpc
cargo build --release
sudo cp target/release/rgbpc /usr/local/bin/
sudo cp assets/rgbpc.desktop /usr/share/applications/
```

## How It Works
- By enabling the **Omarchy Sync**, `rgbpc` generates a hook script that listens to Omarchy's theme changes.
- Upon a theme change, it executes `rgbpc --sync-theme` silently in the background.
- It parses your devices, ignores the blacklisted ones, and pushes the exact hexadecimal color to the active components.

## Support
Designed for Arch Linux and the Omarchy desktop environment.

## Omarchy / Hyprland Window Rules
Since `rgbpc` is a terminal app, it will normally tile like a regular terminal window. If you want it to open as a floating, centered app (like a GUI settings manager) when launched from Walker, add the following to your `~/.config/hypr/hyprland.conf` or Omarchy's window rules config:

```conf
windowrulev2 = float, title:^(RGBPC)$
windowrulev2 = center, title:^(RGBPC)$
windowrulev2 = size 800 600, title:^(RGBPC)$
```
