//! Human-readable, hand-editable persistence of compensation settings.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    compensation::{Defect, MaskParams, MaskQuality},
    display::DisplayIdentity,
};

/// Format version written to new files.
pub const CURRENT_VERSION: u32 = 1;

const APP_DIR: &str = "unburn";
const AUTOSTART_FILE: &str = "unburn.desktop";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not a valid unburn configuration: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serializing the configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error(
        "configuration version {found} is newer than this build understands ({CURRENT_VERSION})"
    )]
    UnsupportedVersion { found: u32 },
    #[error("no home or XDG configuration directory is set")]
    NoConfigDir,
}

/// The whole on-disk configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    /// Written as repeated `[[display]]` tables.
    #[serde(default, rename = "display")]
    pub displays: Vec<DisplayConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            displays: Vec::new(),
        }
    }
}

/// Per-monitor compensation settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Friendly label chosen by the user.
    #[serde(default)]
    pub name: String,

    #[serde(flatten)]
    pub identity: DisplayIdentity,

    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_compensation")]
    pub compensation: f32,
    #[serde(default)]
    pub quality: MaskQuality,
    #[serde(default = "default_true")]
    pub dither: bool,

    #[serde(default, rename = "defects")]
    pub defects: Vec<Defect>,
}

fn default_true() -> bool {
    true
}
fn default_compensation() -> f32 {
    1.0
}

impl DisplayConfig {
    pub fn new(identity: DisplayIdentity) -> Self {
        Self {
            name: identity.describe(),
            identity,
            enabled: true,
            compensation: default_compensation(),
            quality: MaskQuality::default(),
            dither: true,
            defects: Vec::new(),
        }
    }

    pub fn mask_params(&self) -> MaskParams {
        MaskParams {
            compensation: self.compensation.clamp(0.0, 1.0),
            quality: self.quality,
            dither: self.dither,
        }
    }

    pub fn label(&self) -> String {
        if self.name.is_empty() {
            self.identity.describe()
        } else {
            self.name.clone()
        }
    }

    pub fn defect_index(&self, id: Uuid) -> Option<usize> {
        self.defects.iter().position(|d| d.id() == id)
    }

    /// What to call the spot at `index`, which is the only name a spot has:
    /// its place in this list.
    pub fn defect_label(index: usize) -> String {
        format!("Spot {}", index + 1)
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let parsed: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;
        if parsed.version > CURRENT_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: parsed.version,
            });
        }
        Ok(parsed)
    }

    /// Load `path`, or return an empty configuration if the file does not exist yet.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match Config::load(path) {
            Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                Ok(Config::default())
            }
            other => other,
        }
    }

    /// Write atomically, so a crash mid-save cannot leave a truncated file.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        // Plain `to_string` keeps `center = [0.62, 0.43]` on one line; the
        // pretty printer explodes every array across four.
        let text = toml::to_string(self)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_owned(),
                source,
            })?;
        }
        let temp = path.with_extension("toml.tmp");
        fs::write(&temp, text.as_bytes()).map_err(|source| ConfigError::Write {
            path: temp.clone(),
            source,
        })?;
        fs::rename(&temp, path).map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })?;
        Ok(())
    }

    /// The stored settings for a monitor, if we have ever seen it.
    pub fn find(&self, identity: &DisplayIdentity) -> Option<&DisplayConfig> {
        self.best_index(identity).map(|i| &self.displays[i])
    }

    pub fn find_mut(&mut self, identity: &DisplayIdentity) -> Option<&mut DisplayConfig> {
        self.best_index(identity)
            .map(move |i| &mut self.displays[i])
    }

    fn best_index(&self, identity: &DisplayIdentity) -> Option<usize> {
        self.displays
            .iter()
            .enumerate()
            .map(|(i, d)| (d.identity.match_score(identity), i))
            .filter(|(score, _)| *score >= crate::display::MatchScore::WEAK)
            .max_by_key(|(score, _)| *score)
            .map(|(_, i)| i)
    }

    /// Get the settings for a monitor, creating defaults on first sight.
    pub fn entry(&mut self, identity: &DisplayIdentity) -> &mut DisplayConfig {
        match self.best_index(identity) {
            Some(i) => {
                // Refresh the identity so a monitor that moved to another port
                // keeps matching next time, without discarding identifiers this
                // session happens not to be able to read.
                self.displays[i].identity.refresh_from(identity);
                &mut self.displays[i]
            }
            None => {
                self.displays.push(DisplayConfig::new(identity.clone()));
                self.displays.last_mut().unwrap()
            }
        }
    }
}

/// `$XDG_CONFIG_HOME/unburn`, falling back to `~/.config/unburn`.
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join(APP_DIR));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or(ConfigError::NoConfigDir)?;
    Ok(PathBuf::from(home).join(".config").join(APP_DIR))
}

/// `$XDG_CONFIG_HOME/unburn/config.toml`. Every known monitor lives here.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join("config.toml"))
}

/// Path of the XDG autostart entry.
pub fn autostart_path() -> Result<PathBuf, ConfigError> {
    let base = if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(dir)
    } else {
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .ok_or(ConfigError::NoConfigDir)?;
        PathBuf::from(home).join(".config")
    };
    Ok(base.join("autostart").join(AUTOSTART_FILE))
}

pub fn autostart_enabled() -> bool {
    autostart_path().map(|p| p.exists()).unwrap_or(false)
}

/// Install or remove the `Start automatically on login` entry.
pub fn set_autostart(enabled: bool) -> Result<(), ConfigError> {
    let path = autostart_path()?;
    if !enabled {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(ConfigError::Write { path, source }),
        }
    }

    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unburn".to_string());
    let command = format!("{exe} start");
    let desktop = autostart_desktop(&command);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: parent.to_owned(),
            source,
        })?;
    }
    fs::write(&path, desktop).map_err(|source| ConfigError::Write { path, source })
}

fn autostart_desktop(command: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=unburn.tv\n\
         Comment=display defect compensation\n\
         Exec={command}\n\
         Terminal=false\n\
         Categories=Utility;\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

fn autostart_mentions_profile(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("Exec=") && line.contains("--profile")
    })
}

/// Rewrite a login entry left over from named profiles, which this build rejects.
pub fn repair_autostart() -> Result<(), ConfigError> {
    let path = autostart_path()?;
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(());
    };
    if !autostart_mentions_profile(&text) {
        return Ok(());
    }
    set_autostart(true)
}

/// Directory of leftover named-profile files, if any still sit next to `config.toml`.
pub fn leftover_named_profiles(dir: &Path) -> Option<PathBuf> {
    let profiles = dir.join("profiles");
    let entries = fs::read_dir(&profiles).ok()?;
    let has_toml = entries
        .flatten()
        .any(|e| e.path().extension().is_some_and(|ext| ext == "toml"));
    has_toml.then_some(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::{RadialDefect, Rgb, Vec2};

    const SPEC_EXAMPLE: &str = r#"
version = 1

[[display]]
name = "Living Room TV"
connector = "HDMI-A-1"
manufacturer = "SAM"
model = "QN90B"
serial = "SN0123456"
enabled = true
compensation = 0.82
gamma = 2.2

[[display.defects]]
id = "spot-1"
kind = "radial"
enabled = true
center = [0.62, 0.43]
radius = [0.075, 0.091]
rotation = 0.0
strength = 0.11
falloff = 1.0

[[display.defects]]
id = "spot-2"
kind = "radial"
enabled = true
center = [0.31, 0.68]
radius = [0.052, 0.057]
rotation = 0.0
strength = [0.065, 0.04, 0.04]
falloff = 1.3
"#;

    #[test]
    fn parses_the_specifications_example_file() {
        let profile: Config = toml::from_str(SPEC_EXAMPLE).unwrap();
        assert_eq!(profile.version, 1);
        assert_eq!(profile.displays.len(), 1);

        let display = &profile.displays[0];
        assert_eq!(display.name, "Living Room TV");
        assert_eq!(display.identity.connector.as_deref(), Some("HDMI-A-1"));
        assert_eq!(display.identity.manufacturer.as_deref(), Some("SAM"));
        assert_eq!(display.identity.model.as_deref(), Some("QN90B"));
        assert_eq!(display.identity.serial.as_deref(), Some("SN0123456"));
        assert_eq!(display.compensation, 0.82);
        assert_eq!(display.defects.len(), 2);

        let spot = display.defects[0].as_radial().unwrap();
        assert_eq!(spot.center, Vec2::new(0.62, 0.43));
        assert_eq!(spot.radius, Vec2::new(0.075, 0.091));
        assert_eq!(spot.strength, Rgb::splat(0.11));

        // A bare number means all three channels; a list is per channel.
        let tinted = display.defects[1].as_radial().unwrap();
        assert_eq!(tinted.strength, Rgb::new(0.065, 0.04, 0.04));
    }

    #[test]
    fn omitted_fields_take_sensible_defaults() {
        let profile: Config =
            toml::from_str("version = 1\n[[display]]\nconnector = \"DP-1\"\n").unwrap();
        let display = &profile.displays[0];
        assert!(display.enabled);
        assert_eq!(display.compensation, 1.0);
        assert_eq!(display.quality, MaskQuality::Normal);
        assert!(display.defects.is_empty());
    }

    #[test]
    fn retired_calibration_keys_are_ignored_rather_than_refused() {
        // Gamma, reference level and composition were once per-display settings.
        // They are fixed in the model now, but a file written before that has
        // to keep loading -- silently, and without losing anything else on the
        // way past.
        let profile: Config = toml::from_str(
            "version = 1\n\
             [[display]]\n\
             connector = \"DP-1\"\n\
             compensation = 0.6\n\
             gamma = 2.4\n\
             reference = 0.9\n\
             composition = \"multiplicative\"\n",
        )
        .unwrap();
        let display = &profile.displays[0];
        assert_eq!(display.compensation, 0.6);
        assert_eq!(display.identity.connector.as_deref(), Some("DP-1"));

        // And they do not come back when it is written out again.
        let text = toml::to_string(&profile).unwrap();
        assert!(!text.contains("gamma"), "{text}");
        assert!(!text.contains("reference"), "{text}");
        assert!(!text.contains("composition"), "{text}");
    }

    #[test]
    fn round_trips_through_toml() {
        let mut profile = Config::default();
        let display = profile.entry(&DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            model: Some("QN90".into()),
            ..Default::default()
        });
        display.compensation = 0.82;
        display.defects.push(Defect::Radial(RadialDefect {
            center: Vec2::new(0.62, 0.43),
            ..Default::default()
        }));

        let text = toml::to_string(&profile).unwrap();
        assert!(text.contains("center = [0.62, 0.43]"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        // Compared as written rather than by value: defect ids are runtime
        // handles, minted afresh on load and absent from the file.
        assert_eq!(toml::to_string(&back).unwrap(), text);
    }

    #[test]
    fn refuses_configurations_from_the_future() {
        let dir = std::env::temp_dir().join(format!("unburn-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("future.toml");
        fs::write(&path, "version = 99\n").unwrap();
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::UnsupportedVersion { found: 99 })
        ));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_is_an_empty_configuration() {
        let path = std::env::temp_dir().join(format!(
            "unburn-missing-{}-{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let profile = Config::load_or_default(&path).unwrap();
        assert!(profile.displays.is_empty());
    }

    #[test]
    fn saving_and_loading_preserves_everything() {
        let dir = std::env::temp_dir().join(format!("unburn-save-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut profile = Config::default();
        profile.entry(&DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            ..Default::default()
        });
        profile.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), profile);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn entry_reuses_a_known_display_and_refreshes_its_identity() {
        let mut profile = Config::default();
        profile.entry(&DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            serial: Some("ABC".into()),
            ..Default::default()
        });
        profile.entry(&DisplayIdentity {
            connector: Some("HDMI-A-2".into()),
            serial: Some("ABC".into()),
            ..Default::default()
        });
        assert_eq!(profile.displays.len(), 1);
        assert_eq!(
            profile.displays[0].identity.connector.as_deref(),
            Some("HDMI-A-2")
        );
    }

    fn living_room_tv() -> DisplayIdentity {
        DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            manufacturer: Some("SAM".into()),
            model: Some("QN90B".into()),
            serial: Some("SN0123456".into()),
            edid_hash: Some("aaaaaaaaaaaaaaaa".into()),
        }
    }

    #[test]
    fn swapping_the_monitor_on_a_port_leaves_the_old_settings_alone() {
        let mut profile = Config::default();
        let tv = profile.entry(&living_room_tv());
        tv.name = "Living Room TV".into();
        tv.compensation = 0.82;

        profile.entry(&DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            manufacturer: Some("DEL".into()),
            model: Some("U2723QE".into()),
            serial: Some("CN-0ABCDE".into()),
            edid_hash: Some("bbbbbbbbbbbbbbbb".into()),
        });

        assert_eq!(profile.displays.len(), 2);
        assert_eq!(profile.displays[0].name, "Living Room TV");
        assert_eq!(profile.displays[0].compensation, 0.82);
        assert_eq!(profile.displays[0].identity, living_room_tv());
        assert_eq!(profile.displays[1].compensation, 1.0);
    }

    #[test]
    fn a_session_that_cannot_read_edid_keeps_what_an_earlier_one_learned() {
        let mut profile = Config::default();
        profile.entry(&living_room_tv());

        // The same panel as a Wayland client sees it: no serial, no EDID, and a
        // spelled-out vendor name.
        profile.entry(&DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            manufacturer: Some("Samsung Electric Company".into()),
            model: Some("QN90B".into()),
            serial: None,
            edid_hash: None,
        });

        assert_eq!(profile.displays.len(), 1);
        let identity = &profile.displays[0].identity;
        assert_eq!(identity.serial.as_deref(), Some("SN0123456"));
        assert_eq!(identity.edid_hash.as_deref(), Some("aaaaaaaaaaaaaaaa"));
    }

    /// The order defects are written in is the only thing identifying them, so a
    /// round trip must not disturb it.
    #[test]
    fn saving_and_loading_preserves_the_order_of_defects() {
        let mut profile = Config::default();
        let display = profile.entry(&DisplayIdentity::default());
        for x in [0.1, 0.5, 0.9] {
            display.defects.push(Defect::Radial(RadialDefect {
                center: Vec2::new(x, 0.5),
                ..Default::default()
            }));
        }

        let back: Config = toml::from_str(&toml::to_string(&profile).unwrap()).unwrap();
        let centers: Vec<f32> = back.displays[0]
            .defects
            .iter()
            .map(|d| d.center().x)
            .collect();
        assert_eq!(centers, vec![0.1, 0.5, 0.9]);
    }

    #[test]
    fn every_monitor_lives_in_a_single_config_file() {
        let path = config_path().unwrap();
        assert_eq!(path, config_dir().unwrap().join("config.toml"));
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("config.toml")
        );
        assert!(
            !path.components().any(|c| c.as_os_str() == "profiles"),
            "{path:?}"
        );
    }

    #[test]
    fn leftover_named_profile_files_are_detected() {
        let dir = std::env::temp_dir().join(format!("unburn-legacy-{}", uuid::Uuid::new_v4()));
        let profiles = dir.join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        assert_eq!(leftover_named_profiles(&dir), None);
        fs::write(profiles.join("tv.toml"), "version = 1\n").unwrap();
        assert_eq!(leftover_named_profiles(&dir), Some(profiles));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_login_entry_must_not_name_a_profile() {
        let desktop = autostart_desktop("/usr/bin/unburn start");
        assert!(
            desktop.contains("Exec=/usr/bin/unburn start\n"),
            "{desktop}"
        );
        assert!(!autostart_mentions_profile(&desktop));
        assert!(autostart_mentions_profile(
            "Exec=/usr/bin/unburn start --profile living-room\n"
        ));
    }
}
