//! Command line surface.

use clap::{Parser, ValueEnum};

/// Display uniformity compensation: a transparent overlay that evens out
/// smooth brightness defects on a monitor or TV.
#[derive(Debug, Parser)]
#[command(name = "unburn", version, about, long_about = None)]
pub struct Args {
    /// Run without the calibration window; overlays only.
    #[arg(long)]
    pub no_gui: bool,

    /// Use a named profile instead of the default configuration.
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// Tell a running instance to remove every overlay, then exit.
    #[arg(long, conflicts_with_all = ["enable", "bypass", "quit", "status"])]
    pub disable: bool,

    /// Tell a running instance to restore compensation, then exit.
    #[arg(long, conflicts_with_all = ["bypass", "quit", "status"])]
    pub enable: bool,

    /// Tell a running instance to flip its bypass state, then exit.
    ///
    /// Bind this to a compositor shortcut when no global hotkey is available.
    #[arg(long, conflicts_with_all = ["quit", "status"])]
    pub bypass: bool,

    /// Tell a running instance to shut down, then exit.
    #[arg(long, conflicts_with = "status")]
    pub quit: bool,

    /// Print what a running instance is currently doing, then exit.
    #[arg(long)]
    pub status: bool,

    /// Show a fullscreen calibration pattern at startup.
    ///
    /// Accepts a grey level in percent (`0`, `5`, `10`, `25`, `50`, `75`,
    /// `100`) or a colour name (`black`, `white`, `red`, `green`, `blue`).
    #[arg(long, value_name = "PATTERN")]
    pub test_pattern: Option<String>,

    /// List the connected monitors as unburn sees them, then exit.
    #[arg(long)]
    pub list_displays: bool,

    /// Report which overlay backends this session supports, then exit.
    #[arg(long)]
    pub check: bool,

    /// Force a particular overlay backend instead of detecting one.
    #[arg(long, value_enum, default_value_t = BackendChoice::Auto)]
    pub backend: BackendChoice,

    /// Install or remove the "start on login" entry, then exit.
    #[arg(long, value_name = "on|off")]
    pub autostart: Option<OnOff>,

    /// Start with compensation bypassed.
    #[arg(long)]
    pub start_bypassed: bool,

    /// Increase log verbosity; repeat for more.
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendChoice {
    Auto,
    Wayland,
    X11,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

impl Args {
    /// True when the invocation only talks to a running instance.
    pub fn is_remote_control(&self) -> bool {
        self.disable || self.enable || self.bypass || self.quit || self.status
    }

    pub fn log_filter(&self) -> &'static str {
        match self.verbose {
            0 => "unburn=info,warn",
            1 => "unburn=debug,info",
            _ => "unburn=trace,debug",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn accepts_the_documented_invocations() {
        assert!(!Args::parse_from(["unburn"]).no_gui);
        assert!(Args::parse_from(["unburn", "--no-gui"]).no_gui);
        assert_eq!(
            Args::parse_from(["unburn", "--profile", "living-room"])
                .profile
                .as_deref(),
            Some("living-room")
        );
        assert!(Args::parse_from(["unburn", "--disable"]).disable);
        assert_eq!(
            Args::parse_from(["unburn", "--test-pattern", "50"])
                .test_pattern
                .as_deref(),
            Some("50")
        );
    }

    #[test]
    fn remote_control_flags_are_recognised() {
        assert!(Args::parse_from(["unburn", "--disable"]).is_remote_control());
        assert!(Args::parse_from(["unburn", "--status"]).is_remote_control());
        assert!(!Args::parse_from(["unburn"]).is_remote_control());
    }
}
