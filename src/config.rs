//! `streamwm` configuration schema and TOML file deserialization.
//!
//! Provides customizable parameters for keybindings, layout ratios, gaps, borders,
//! floating app filtering, and ACPI lid-switch display profile rules.

use serde::Deserialize;

/// Top-level configuration structure for `streamwm`, deserialized from TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Modifier key name: `"super"`, `"alt"`, `"ctrl"` (maps to MOD4/MOD1/CTRL).
    #[serde(default = "default_mod")]
    pub modifier: String,

    /// Terminal executable command (spawned by default terminal hotkey `Mod+Return`).
    #[serde(default = "default_terminal")]
    pub terminal: String,

    /// Application launcher command (spawned by launcher hotkey `Mod+d`, e.g. `fuzzel` or `rofi`).
    #[serde(default = "default_launcher")]
    pub launcher: String,

    /// Outer and inner gaps between windows and screen edges, in logical pixels.
    #[serde(default = "default_gap")]
    pub gap: u32,

    /// Window border width, in logical pixels.
    #[serde(default = "default_border")]
    pub border_width: u32,

    /// Unfocused window border color formatted as hex string (`"RRGGBB"` or `"#RRGGBB"`).
    #[serde(default = "default_border_color")]
    pub border_color: String,

    /// Focused window border color formatted as hex string (`"RRGGBB"` or `"#RRGGBB"`).
    #[serde(default = "default_focused_border_color")]
    pub focused_border_color: String,

    /// Whether pointer motion into a window automatically shifts seat focus to that window (`true` by default).
    #[serde(default = "default_true")]
    pub focus_follows_mouse: bool,

    /// Whether to request server-side decorations (SSD titlebars) from client windows (`true` by default).
    #[serde(default = "default_true")]
    pub use_ssd: bool,

    /// Custom user keybindings declaration list.
    #[serde(default)]
    pub bindings: Vec<Binding>,

    /// ACPI lid switch and display topology monitor configuration.
    #[serde(default)]
    pub lid: Lid,

    /// Security flag: allows arbitrary command execution requested over the JSON control socket.
    /// Disabled (`false`) by default to prevent untrusted socket clients from executing commands.
    #[serde(default)]
    pub allow_spawn: bool,

    /// App IDs of windows that should never be tiled (e.g. password prompts, calculators).
    /// Matching windows automatically launch in floating mode.
    #[serde(default)]
    pub floating_app_ids: Vec<String>,

    /// Initial master window width fraction for tiling layouts (`0.1..=0.9`, default `0.55`).
    #[serde(default = "default_master_fraction")]
    pub master_fraction: f64,

    /// Incremental step size applied to the master fraction per arrow key press in resize mode.
    #[serde(default = "default_resize_step")]
    pub resize_step: f64,
}

/// A keybinding entry mapping a physical key combination to an internal WM action.
#[derive(Debug, Clone, Deserialize)]
pub struct Binding {
    /// xkbcommon keysym name (e.g. `"j"`, `"Return"`, `"XF86AudioRaiseVolume"`).
    pub keysym: String,
    /// Comma- or plus-separated modifier list (e.g. `"shift,ctrl"`, `"super+alt"`, `"none"`).
    #[serde(default)]
    pub modifiers: String,
    /// Internal action identifier (e.g. `"focus_next"`, `"spawn"`, `"close"`, `"fullscreen"`).
    pub action: String,
    /// Optional command string or parameter argument (e.g. command for `"spawn"` action).
    #[serde(default)]
    pub arg: Option<String>,
}

/// ACPI lid switch and display topology configuration for clamshell laptop setups.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Lid {
    /// Master toggle enabling lid state polling and automatic kanshi profile switching.
    #[serde(default)]
    pub enable: bool,
    /// kanshi profile name to activate when the laptop lid closes.
    #[serde(default = "default_lid_close_profile")]
    pub close_profile: String,
    /// kanshi profile name to activate when the laptop lid opens while docked to external monitors.
    #[serde(default = "default_lid_open_profile")]
    pub open_profile: String,
    /// kanshi profile name to activate when operating undocked with only the internal panel.
    #[serde(default = "default_lid_undocked_profile")]
    pub undocked_profile: String,
    /// Internal laptop panel DRM output identifier (e.g. `"eDP-1"`).
    #[serde(default = "default_lid_internal_output")]
    pub internal_output: String,
}

fn default_mod() -> String {
    "super".to_string()
}
fn default_terminal() -> String {
    "alacritty".to_string()
}
fn default_launcher() -> String {
    "alacritty".to_string()
}
fn default_gap() -> u32 {
    8
}
fn default_border() -> u32 {
    2
}
fn default_border_color() -> String {
    "4c4c4c".to_string()
}
fn default_focused_border_color() -> String {
    "5294e2".to_string()
}
fn default_true() -> bool {
    true
}
fn default_master_fraction() -> f64 {
    0.55
}
fn default_resize_step() -> f64 {
    0.05
}
fn default_lid_close_profile() -> String {
    "office_clamshell".to_string()
}
fn default_lid_open_profile() -> String {
    "office".to_string()
}
fn default_lid_undocked_profile() -> String {
    "undocked".to_string()
}
fn default_lid_internal_output() -> String {
    "eDP-1".to_string()
}

impl Config {
    pub fn load(path: &str) -> Config {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
            log::warn!("failed to read config {path}: {e}; using defaults");
            String::new()
        });
        toml::from_str(&text).unwrap_or_else(|e| {
            log::warn!("failed to parse config {path}: {e}; using defaults");
            Config::default()
        })
    }

    /// Number of tags (fixed 1..=9).
    pub fn num_tags(&self) -> u32 {
        9
    }

    /// Whether a window with this app id should be floating (never tiled).
    pub fn is_floating_app(&self, app_id: &str) -> bool {
        self.floating_app_ids.iter().any(|id| id == app_id)
    }

    /// Parse a hex color like "4c4c4c" into (r,g,b) 0..255.
    pub fn color(&self, hex: &str) -> (u8, u8, u8) {
        let s = hex.trim_start_matches('#');
        let v = u32::from_str_radix(s, 16).unwrap_or(0);
        (
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
        )
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            modifier: default_mod(),
            terminal: default_terminal(),
            launcher: default_launcher(),
            gap: default_gap(),
            border_width: default_border(),
            border_color: default_border_color(),
            focused_border_color: default_focused_border_color(),
            focus_follows_mouse: true,
            use_ssd: true,
            bindings: vec![],
            lid: Lid::default(),
            allow_spawn: false,
            floating_app_ids: vec![],
            master_fraction: default_master_fraction(),
            resize_step: default_resize_step(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let c: Config = toml::from_str("modifier = \"super\"\ngap = 4\n").unwrap();
        assert_eq!(c.modifier, "super");
        assert_eq!(c.gap, 4);
        assert_eq!(c.num_tags(), 9);
        assert!(!c.allow_spawn);
    }

    #[test]
    fn color_parse() {
        let c = Config::default();
        assert_eq!(c.color("5294e2"), (0x52, 0x94, 0xe2));
        assert_eq!(c.color("#4c4c4c"), (0x4c, 0x4c, 0x4c));
        assert_eq!(c.color("not-hex"), (0, 0, 0));
    }

    #[test]
    fn default_matches_serde_security_defaults() {
        assert!(!Config::default().allow_spawn);
    }

    #[test]
    fn parse_floating_app_ids_and_match() {
        let c: Config = toml::from_str(
            r#"
floating_app_ids = ["polkit-gnome-authentication-agent-1", "gnome-calculator", "google-meet"]
"#,
        )
        .unwrap();
        assert_eq!(c.floating_app_ids.len(), 3);
        assert!(c.is_floating_app("gnome-calculator"));
        assert!(c.is_floating_app("google-meet"));
        assert!(!c.is_floating_app("alacritty"));
        assert!(Config::default().floating_app_ids.is_empty());
    }

    #[test]
    fn default_and_parse_master_fraction_and_resize_step() {
        let d = Config::default();
        assert_eq!(d.master_fraction, 0.55);
        assert_eq!(d.resize_step, 0.05);

        let c: Config = toml::from_str(
            r#"
master_fraction = 0.6
resize_step = 0.02
"#,
        )
        .unwrap();
        assert_eq!(c.master_fraction, 0.6);
        assert_eq!(c.resize_step, 0.02);
    }

    #[test]
    fn parse_full_binding_and_lid_config() {
        let c: Config = toml::from_str(
            r#"
modifier = "alt"
terminal = "foot"
launcher = "fuzzel"
gap = 12
border_width = 3
border_color = "101112"
focused_border_color = "abcdef"
focus_follows_mouse = false
use_ssd = false
allow_spawn = true

[[bindings]]
keysym = "F1"
modifiers = "shift"
action = "spawn"
arg = "foot"

[lid]
enable = true
close_profile = "closed"
open_profile = "open"
"#,
        )
        .unwrap();

        assert_eq!(c.modifier, "alt");
        assert_eq!(c.terminal, "foot");
        assert_eq!(c.launcher, "fuzzel");
        assert_eq!(c.gap, 12);
        assert_eq!(c.border_width, 3);
        assert_eq!(c.color(&c.border_color), (0x10, 0x11, 0x12));
        assert_eq!(c.color(&c.focused_border_color), (0xab, 0xcd, 0xef));
        assert!(!c.focus_follows_mouse);
        assert!(!c.use_ssd);
        assert!(c.allow_spawn);
        assert_eq!(c.bindings.len(), 1);
        assert_eq!(c.bindings[0].arg.as_deref(), Some("foot"));
        assert!(c.lid.enable);
        assert_eq!(c.lid.close_profile, "closed");
        assert_eq!(c.lid.open_profile, "open");
        assert_eq!(c.lid.undocked_profile, "undocked");
        assert_eq!(c.lid.internal_output, "eDP-1");
    }

    #[test]
    fn parse_lid_recovery_config() {
        let c: Config = toml::from_str(
            r#"
[lid]
enable = true
close_profile = "office_clamshell"
open_profile = "office"
undocked_profile = "mobile"
internal_output = "eDP-2"
"#,
        )
        .unwrap();

        assert!(c.lid.enable);
        assert_eq!(c.lid.close_profile, "office_clamshell");
        assert_eq!(c.lid.open_profile, "office");
        assert_eq!(c.lid.undocked_profile, "mobile");
        assert_eq!(c.lid.internal_output, "eDP-2");
    }
}
