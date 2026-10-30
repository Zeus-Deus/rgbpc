//! Parse argv without the executable name, before invoking any backend or hardware.
//! Device values are opaque: each --device consumes exactly the next argv entry.
//! The caller resolves selectors and rejects missing, ambiguous, or disabled matches.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Tui,
    ListDevices,
    Color(String),
    Off,
    Rainbow,
    SyncTheme,
    RestoreLast,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub action: Action,
    pub standalone: bool,
    pub json: bool,
    pub devices: Vec<String>,
}

pub const HELP: &str = "RGBPC — OpenRGB lighting control
Usage: rgbpc [ACTION] [--standalone | --fast] [--json] [--device VALUE ...]

No action opens the interactive TUI.
  --list-devices       List devices
  --color RRGGBB       Set color; #RRGGBB is also accepted (quote # in shells)
  --off               Turn enabled devices off
  --rainbow           Apply rainbow mode
  --sync-theme        Apply the current supported desktop theme
  --restore-last      Restore remembered device states
  --help, -h          Show this plain-text help

Automatic acceleration is the default, with standalone fallback.
  --standalone        Diagnostic escape: use the standalone backend
  --fast              Legacy alias for default automatic mode
  --json              JSON output for noninteractive actions, except help
  --device VALUE      Repeatable selector for color/off/rainbow/list only;
                      consumes the next argument verbatim. Selectors never
                      override disabled devices. Omit to use enabled devices.
Use exactly one action; --fast and --standalone conflict.
Flags may precede or follow the action. Help cannot be combined with --json.

Examples:
  rgbpc
  rgbpc --json --list-devices
  rgbpc --device 'Motherboard' --color '#aAbB09' --json
  rgbpc --off --device 'Case lights' --standalone
  rgbpc --fast --restore-last

Case lights: enable motherboard sync on the case controller and connect its
ARGB sync cable to a compatible motherboard header; consult its manual.

Exit codes: 0 accepted command; 1 operation error or partial failure;
2 invalid arguments. Success does not verify that physical LEDs changed.
";

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        action: Action::Tui,
        standalone: false,
        json: false,
        devices: Vec::new(),
    };
    let mut action = None;
    let mut fast = false;
    let mut index = 0;

    while let Some(arg) = args.get(index) {
        let next_action = match arg.as_str() {
            "--list-devices" => Some(Action::ListDevices),
            "--off" => Some(Action::Off),
            "--rainbow" => Some(Action::Rainbow),
            "--sync-theme" => Some(Action::SyncTheme),
            "--restore-last" => Some(Action::RestoreLast),
            "--help" | "-h" => Some(Action::Help),
            "--color" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or("--color requires RRGGBB or #RRGGBB")?;
                let color = value.strip_prefix('#').unwrap_or(value);
                if color.len() != 6 || !color.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(
                        "--color requires exactly six ASCII hex digits, with at most one leading #"
                            .into(),
                    );
                }
                Some(Action::Color(color.to_owned()))
            }
            "--standalone" => {
                options.standalone = true;
                None
            }
            "--fast" => {
                fast = true;
                None
            }
            "--json" => {
                options.json = true;
                None
            }
            "--device" => {
                index += 1;
                // Even flag-like or empty argv entries remain literal selectors.
                // The caller must resolve them without broadening the target set.
                let value = args.get(index).ok_or("--device requires a value")?;
                options.devices.push(value.clone());
                None
            }
            _ => return Err(format!("Unexpected argument {arg:?}; use --help")),
        };

        if let Some(next_action) = next_action {
            if action.is_some() {
                return Err(format!(
                    "Duplicate or conflicting action {arg:?}; choose exactly one action"
                ));
            }
            action = Some(next_action);
        }
        index += 1;
    }

    options.action = action.unwrap_or(Action::Tui);
    if fast && options.standalone {
        return Err("--fast and --standalone cannot be combined".into());
    }
    if options.json && matches!(options.action, Action::Tui | Action::Help) {
        return Err("--json requires a noninteractive action other than help".into());
    }
    if !options.devices.is_empty()
        && !matches!(
            options.action,
            Action::ListDevices | Action::Color(_) | Action::Off | Action::Rainbow
        )
    {
        return Err(
            "--device is only supported with --list-devices, --color, --off, or --rainbow".into(),
        );
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn parsed(args: &[&str]) -> Options {
        parse(&argv(args)).unwrap_or_else(|error| panic!("{args:?}: {error}"))
    }

    fn rejected(args: &[&str]) {
        let error = parse(&argv(args)).expect_err(&format!("accepted {args:?}"));
        assert!(!error.is_empty(), "empty diagnostic for {args:?}");
    }

    #[test]
    fn exact_defaults() {
        assert_eq!(
            parsed(&[]),
            Options {
                action: Action::Tui,
                standalone: false,
                json: false,
                devices: Vec::new(),
            }
        );
    }

    #[test]
    fn all_actions() {
        for (flag, expected) in [
            ("--list-devices", Action::ListDevices),
            ("--off", Action::Off),
            ("--rainbow", Action::Rainbow),
            ("--sync-theme", Action::SyncTheme),
            ("--restore-last", Action::RestoreLast),
            ("--help", Action::Help),
            ("-h", Action::Help),
        ] {
            assert_eq!(
                parsed(&[flag]),
                Options {
                    action: expected,
                    standalone: false,
                    json: false,
                    devices: Vec::new(),
                }
            );
        }
    }

    #[test]
    fn color_preserves_case() {
        for color in ["000000", "FFFFFF", "aAbB09", "123456"] {
            assert_eq!(
                parsed(&["--color", color]).action,
                Action::Color(color.into())
            );
        }
    }

    #[test]
    fn color_strips_one_optional_hash() {
        assert_eq!(
            parsed(&["--color", "#aAbB09"]).action,
            Action::Color("aAbB09".into())
        );
    }

    #[test]
    fn color_rejects_wrong_lengths_and_repeated_hash() {
        for color in [
            "", "#", "123", "12345", "1234567", "##123456", "0x123456", "#1234567",
        ] {
            rejected(&["--color", color]);
        }
    }

    #[test]
    fn color_rejects_non_ascii_or_non_hex() {
        for color in [
            "gg0000",
            "ABCDEX",
            "１２３４５６",
            "é12345",
            "AABB🙂",
            "aabb\0c",
        ] {
            rejected(&["--color", color]);
        }
    }

    #[test]
    fn color_rejects_whitespace_without_repair() {
        for color in [
            " 123456", "123456 ", "# 123456", "123 56", "12345\n", "\t123456",
        ] {
            rejected(&["--color", color]);
        }
    }

    #[test]
    fn missing_color_value() {
        rejected(&["--color"]);
        rejected(&["--color", "--off"]);
    }

    #[test]
    fn missing_device_value() {
        rejected(&["--off", "--device"]);
    }

    #[test]
    fn fast_alias_has_exact_default() {
        assert_eq!(parsed(&["--fast"]), parsed(&[]));
    }

    #[test]
    fn legacy_fast_actions_in_either_order() {
        for flag in ["--sync-theme", "--restore-last"] {
            assert_eq!(parsed(&["--fast", flag]), parsed(&[flag, "--fast"]));
            assert_eq!(parsed(&[flag, "--fast"]), parsed(&[flag]));
        }
    }

    #[test]
    fn standalone_tui() {
        assert_eq!(
            parsed(&["--standalone"]),
            Options {
                action: Action::Tui,
                standalone: true,
                json: false,
                devices: Vec::new(),
            }
        );
    }

    #[test]
    fn standalone_noninteractive() {
        assert!(parsed(&["--standalone", "--off"]).standalone);
        assert!(parsed(&["--off", "--standalone"]).standalone);
    }

    #[test]
    fn fast_and_standalone_conflict_in_either_order() {
        rejected(&["--fast", "--standalone"]);
        rejected(&["--standalone", "--fast", "--off"]);
        rejected(&["--help", "--fast", "--standalone"]);
    }

    #[test]
    fn json_for_every_operational_action() {
        for action in [
            vec!["--list-devices"],
            vec!["--color", "aAbB09"],
            vec!["--off"],
            vec!["--rainbow"],
            vec!["--sync-theme"],
            vec!["--restore-last"],
        ] {
            let mut before = vec!["--json"];
            before.extend_from_slice(&action);
            let mut after = action.clone();
            after.push("--json");
            let mut expected = parsed(&action);
            expected.json = true;
            assert_eq!(parsed(&before), expected);
            assert_eq!(parsed(&after), expected);
        }
    }

    #[test]
    fn json_rejects_tui() {
        rejected(&["--json"]);
        rejected(&["--fast", "--json"]);
        rejected(&["--json", "--standalone"]);
    }

    #[test]
    fn help_is_plain_text_only() {
        rejected(&["--json", "--help"]);
        rejected(&["-h", "--json"]);
    }

    #[test]
    fn selectors_only_for_supported_actions() {
        for action in [
            vec!["--list-devices"],
            vec!["--color", "112233"],
            vec!["--off"],
            vec!["--rainbow"],
        ] {
            let mut args = vec!["--device", "Motherboard"];
            args.extend_from_slice(&action);
            assert_eq!(parsed(&args).devices, vec!["Motherboard"]);
        }
        for action in ["--sync-theme", "--restore-last", "--help", "-h"] {
            rejected(&[action, "--device", "Motherboard"]);
        }
        rejected(&["--device", "Motherboard"]);
    }

    #[test]
    fn selectors_are_opaque_ordered_and_repeatable() {
        let values = [
            "  Board #1  ",
            "GPU:00/Ä",
            "a,b",
            "*",
            "",
            "--off",
            "--all",
            "\tCase\n",
            "Board",
            "Board",
        ];
        let mut args = vec!["--list-devices"];
        for value in values {
            args.extend(["--device", value]);
        }
        assert_eq!(parsed(&args).devices, values);
        assert_eq!(parsed(&args).action, Action::ListDevices);
    }

    #[test]
    fn selectors_are_not_split_or_normalized() {
        assert_eq!(
            parsed(&["--off", "--device", "A, B; C"]).devices,
            vec!["A, B; C"]
        );
    }

    #[test]
    fn argument_ordering_is_flexible() {
        let expected = Options {
            action: Action::Color("aAbB09".into()),
            standalone: true,
            json: true,
            devices: vec!["A".into(), " B ".into()],
        };
        for args in [
            vec![
                "--device",
                "A",
                "--json",
                "--color",
                "#aAbB09",
                "--device",
                " B ",
                "--standalone",
            ],
            vec![
                "--standalone",
                "--color",
                "#aAbB09",
                "--device",
                "A",
                "--device",
                " B ",
                "--json",
            ],
            vec![
                "--json",
                "--standalone",
                "--device",
                "A",
                "--device",
                " B ",
                "--color",
                "#aAbB09",
            ],
        ] {
            assert_eq!(parsed(&args), expected);
        }
    }

    #[test]
    fn unknown_options_rejected() {
        for flag in [
            "--all",
            "--force",
            "--colour",
            "--JSON",
            "-j",
            "--",
            "--device=X",
            "--color=112233",
            "--fast=true",
        ] {
            rejected(&[flag]);
            rejected(&["--off", flag]);
        }
    }

    #[test]
    fn positional_values_rejected() {
        for value in ["rgbpc", "off", "112233", "", " ", "--off "] {
            rejected(&[value]);
            rejected(&["--off", value]);
        }
        rejected(&["--color", "112233", "445566"]);
    }

    #[test]
    fn every_duplicate_action_rejected() {
        for action in [
            vec!["--list-devices"],
            vec!["--color", "112233"],
            vec!["--off"],
            vec!["--rainbow"],
            vec!["--sync-theme"],
            vec!["--restore-last"],
            vec!["--help"],
            vec!["-h"],
        ] {
            let args: Vec<&str> = action.iter().chain(&action).copied().collect();
            rejected(&args);
        }
        rejected(&["--help", "-h"]);
        rejected(&["-h", "--help"]);
    }

    #[test]
    fn every_distinct_action_pair_rejected() {
        let actions = [
            vec!["--list-devices"],
            vec!["--color", "112233"],
            vec!["--off"],
            vec!["--rainbow"],
            vec!["--sync-theme"],
            vec!["--restore-last"],
            vec!["--help"],
        ];
        for (i, first) in actions.iter().enumerate() {
            for (j, second) in actions.iter().enumerate() {
                if i != j {
                    let args: Vec<&str> = first.iter().chain(second).copied().collect();
                    rejected(&args);
                }
            }
        }
    }

    #[test]
    fn help_does_not_bypass_validation() {
        rejected(&["--help", "--unknown"]);
        rejected(&["--unknown", "--help"]);
        rejected(&["--help", "--color"]);
        rejected(&["--help", "--device"]);
        rejected(&["--help", "unexpected"]);
    }

    #[test]
    fn modifiers_are_idempotent() {
        assert_eq!(parsed(&["--fast", "--fast"]), parsed(&[]));
        assert_eq!(
            parsed(&["--standalone", "--standalone"]),
            parsed(&["--standalone"])
        );
        assert_eq!(
            parsed(&["--json", "--off", "--json"]),
            parsed(&["--off", "--json"])
        );
    }

    #[test]
    fn no_action_is_inferred_from_selector_value() {
        rejected(&["--device", "--off"]);
        rejected(&["--device", "--help", "--json"]);
    }

    #[test]
    fn no_implicit_all_selector() {
        assert!(parsed(&["--off"]).devices.is_empty());
        rejected(&["--off", "--all"]);
        assert_eq!(
            parsed(&["--off", "--device", "--all"]).devices,
            vec!["--all"]
        );
    }

    #[test]
    fn parse_does_not_mutate_arguments() {
        let args = argv(&["--device", "  A  ", "--color", "#aAbB09"]);
        let original = args.clone();
        assert!(parse(&args).is_ok());
        assert_eq!(args, original);
    }

    #[test]
    fn help_documents_contract() {
        for text in [
            "--list-devices",
            "--color",
            "--off",
            "--rainbow",
            "--sync-theme",
            "--restore-last",
            "--help",
            "-h",
            "--standalone",
            "--fast",
            "--json",
            "--device",
            "Automatic acceleration",
            "standalone fallback",
            "motherboard sync",
            "0 accepted",
            "1 operation error",
            "2 invalid arguments",
            "physical LEDs",
        ] {
            assert!(HELP.contains(text), "missing help text: {text}");
        }
    }
}
