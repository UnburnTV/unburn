//! Command line surface.

use clap::{Parser, Subcommand, ValueEnum};

/// Display uniformity compensation: a transparent overlay that evens out
/// smooth brightness defects on a monitor or TV.
#[derive(Debug, Parser)]
#[command(name = "unburn", version, about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Use a named profile instead of the default configuration.
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    /// Show a fullscreen calibration pattern at startup.
    ///
    /// Accepts a grey level in percent (`0`, `5`, `10`, `25`, `50`, `75`,
    /// `100`) or a colour name (`black`, `white`, `red`, `green`, `blue`).
    #[arg(long, global = true, value_name = "PATTERN")]
    pub test_pattern: Option<String>,

    /// Force a particular overlay backend instead of detecting one.
    #[arg(long, global = true, value_enum, default_value_t = BackendChoice::Auto)]
    pub backend: BackendChoice,

    /// Increase log verbosity; repeat for more.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Start overlays only, without the calibration window.
    Start,
    /// Hide compensation on a running instance, then exit.
    Hide,
    /// Show compensation on a running instance, then exit.
    Show,
    /// Print what a running instance is currently doing, then exit.
    Status,
    /// Tell a running instance to shut down, then exit.
    Quit,
    /// Report which overlay backends this session supports, then exit.
    Check,
    /// List the connected monitors as unburn sees them, then exit.
    ListDisplays,
    /// Install or remove the "start on login" entry, then exit.
    Autostart {
        #[arg(value_enum, value_name = "on|off")]
        state: OnOff,
    },
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
        matches!(
            self.command,
            Some(Command::Hide | Command::Show | Command::Status | Command::Quit)
        )
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
        assert_eq!(Args::parse_from(["unburn"]).command, None);
        assert_eq!(
            Args::parse_from(["unburn", "start"]).command,
            Some(Command::Start)
        );
        assert_eq!(
            Args::parse_from(["unburn", "--profile", "living-room"])
                .profile
                .as_deref(),
            Some("living-room")
        );
        assert_eq!(
            Args::parse_from(["unburn", "hide", "--profile", "bedroom"]).command,
            Some(Command::Hide)
        );
        assert_eq!(
            Args::parse_from(["unburn", "--test-pattern", "50"])
                .test_pattern
                .as_deref(),
            Some("50")
        );
        assert_eq!(
            Args::parse_from(["unburn", "autostart", "on"]).command,
            Some(Command::Autostart { state: OnOff::On })
        );
        assert_eq!(
            Args::parse_from(["unburn", "check"]).command,
            Some(Command::Check)
        );
        assert_eq!(
            Args::parse_from(["unburn", "list-displays"]).command,
            Some(Command::ListDisplays)
        );
    }

    #[test]
    fn remote_control_verbs_are_recognised() {
        assert!(Args::parse_from(["unburn", "hide"]).is_remote_control());
        assert!(Args::parse_from(["unburn", "show"]).is_remote_control());
        assert!(Args::parse_from(["unburn", "status"]).is_remote_control());
        assert!(Args::parse_from(["unburn", "quit"]).is_remote_control());
        assert!(!Args::parse_from(["unburn"]).is_remote_control());
        assert!(!Args::parse_from(["unburn", "start"]).is_remote_control());
        assert!(!Args::parse_from(["unburn", "check"]).is_remote_control());
    }
}
