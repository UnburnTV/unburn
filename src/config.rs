//! Human-readable, hand-editable persistence of compensation profiles.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    compensation::{Composition, Defect, MaskParams, MaskQuality},
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
    #[error("{path} is not a valid unburn profile: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serializing the profile: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("profile version {found} is newer than this build understands ({CURRENT_VERSION})")]
    UnsupportedVersion { found: u32 },
    #[error("no home or XDG configuration directory is set")]
    NoConfigDir,
}

/// The whole on-disk configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    /// Written as repeated `[[display]]` tables.
    #[serde(default, rename = "display")]
    pub displays: Vec<DisplayProfile>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            displays: Vec::new(),
        }
    }
}

/// Per-monitor compensation settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayProfile {
    /// Friendly label chosen by the user.
    #[serde(default)]
    pub name: String,

    #[serde(flatten)]
    pub identity: DisplayIdentity,

    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_compensation")]
    pub compensation: f32,
    #[serde(default = "default_gamma")]
    pub gamma: f32,
    /// Desktop grey level at which a per-channel correction is exact.
    #[serde(default = "default_reference")]
    pub reference: f32,
    #[serde(default)]
    pub quality: MaskQuality,
    #[serde(default)]
    pub composition: Composition,
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
fn default_gamma() -> f32 {
    2.2
}
fn default_reference() -> f32 {
    0.5
}

impl DisplayProfile {
    pub fn new(identity: DisplayIdentity) -> Self {
        Self {
            name: identity.describe(),
            identity,
            enabled: true,
            compensation: default_compensation(),
            gamma: default_gamma(),
            reference: default_reference(),
            quality: MaskQuality::default(),
            composition: Composition::default(),
            dither: true,
            defects: Vec::new(),
        }
    }

    pub fn mask_params(&self) -> MaskParams {
        MaskParams {
            compensation: self.compensation.clamp(0.0, 1.0),
            gamma: self.gamma.clamp(0.1, 6.0),
            reference: self.reference.clamp(0.0, 1.0),
            composition: self.composition,
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

    /// A name like `Spot 3` that is not already taken.
    pub fn next_defect_name(&self) -> String {
        for n in 1.. {
            let candidate = format!("Spot {n}");
            if !self.defects.iter().any(|d| d.name() == candidate) {
                return candidate;
            }
        }
        unreachable!()
    }
}

impl Profile {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let profile: Profile = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;
        if profile.version > CURRENT_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: profile.version,
            });
        }
        Ok(profile)
    }

    /// Load `path`, or return an empty profile if the file does not exist yet.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match Profile::load(path) {
            Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                Ok(Profile::default())
            }
            other => other,
        }
    }

    /// Write atomically, so a crash mid-save cannot leave a truncated profile.
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
    pub fn find(&self, identity: &DisplayIdentity) -> Option<&DisplayProfile> {
        self.best_index(identity).map(|i| &self.displays[i])
    }

    pub fn find_mut(&mut self, identity: &DisplayIdentity) -> Option<&mut DisplayProfile> {
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
    pub fn entry(&mut self, identity: &DisplayIdentity) -> &mut DisplayProfile {
        match self.best_index(identity) {
            Some(i) => {
                // Refresh the identity so a monitor that moved to another port
                // keeps matching next time, without discarding identifiers this
                // session happens not to be able to read.
                self.displays[i].identity.refresh_from(identity);
                &mut self.displays[i]
            }
            None => {
                self.displays.push(DisplayProfile::new(identity.clone()));
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

/// Path of the default profile, or of a named one when `--profile` was given.
pub fn profile_path(name: Option<&str>) -> Result<PathBuf, ConfigError> {
    let dir = config_dir()?;
    Ok(match name {
        None => dir.join("config.toml"),
        Some(name) => dir
            .join("profiles")
            .join(format!("{}.toml", sanitize(name))),
    })
}

/// Names of the profiles saved next to the default one.
pub fn list_profiles() -> Vec<String> {
    let Ok(dir) = config_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir.join("profiles")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "toml").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();
    names
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
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
pub fn set_autostart(enabled: bool, profile: Option<&str>) -> Result<(), ConfigError> {
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
    let mut command = format!("{exe} start");
    if let Some(profile) = profile {
        command.push_str(&format!(" --profile {}", sanitize(profile)));
    }

    let desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=unburn\n\
         Comment=Display uniformity compensation overlay\n\
         Exec={command}\n\
         Terminal=false\n\
         Categories=Utility;\n\
         X-GNOME-Autostart-enabled=true\n"
    );

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: parent.to_owned(),
            source,
        })?;
    }
    fs::write(&path, desktop).map_err(|source| ConfigError::Write { path, source })
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
        let profile: Profile = toml::from_str(SPEC_EXAMPLE).unwrap();
        assert_eq!(profile.version, 1);
        assert_eq!(profile.displays.len(), 1);

        let display = &profile.displays[0];
        assert_eq!(display.name, "Living Room TV");
        assert_eq!(display.identity.connector.as_deref(), Some("HDMI-A-1"));
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
        let profile: Profile =
            toml::from_str("version = 1\n[[display]]\nconnector = \"DP-1\"\n").unwrap();
        let display = &profile.displays[0];
        assert!(display.enabled);
        assert_eq!(display.compensation, 1.0);
        assert_eq!(display.gamma, 2.2);
        assert_eq!(display.reference, 0.5);
        assert_eq!(display.quality, MaskQuality::Normal);
        assert!(display.defects.is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let mut profile = Profile::default();
        let display = profile.entry(&DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            model: Some("QN90".into()),
            ..Default::default()
        });
        display.compensation = 0.82;
        display.defects.push(Defect::Radial(RadialDefect {
            name: "Spot 1".into(),
            center: Vec2::new(0.62, 0.43),
            ..Default::default()
        }));

        let text = toml::to_string(&profile).unwrap();
        assert!(text.contains("center = [0.62, 0.43]"), "{text}");
        let back: Profile = toml::from_str(&text).unwrap();
        assert_eq!(profile, back);
    }

    #[test]
    fn refuses_profiles_from_the_future() {
        let dir = std::env::temp_dir().join(format!("unburn-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("future.toml");
        fs::write(&path, "version = 99\n").unwrap();
        assert!(matches!(
            Profile::load(&path),
            Err(ConfigError::UnsupportedVersion { found: 99 })
        ));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_is_an_empty_profile() {
        let path = std::env::temp_dir().join("unburn-definitely-absent.toml");
        let profile = Profile::load_or_default(&path).unwrap();
        assert!(profile.displays.is_empty());
    }

    #[test]
    fn saving_and_loading_preserves_everything() {
        let dir = std::env::temp_dir().join(format!("unburn-save-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut profile = Profile::default();
        profile.entry(&DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            ..Default::default()
        });
        profile.save(&path).unwrap();
        assert_eq!(Profile::load(&path).unwrap(), profile);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn entry_reuses_a_known_display_and_refreshes_its_identity() {
        let mut profile = Profile::default();
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
            serial: Some("SN12345".into()),
            edid_hash: Some("aaaaaaaaaaaaaaaa".into()),
        }
    }

    #[test]
    fn swapping_the_monitor_on_a_port_leaves_the_old_profile_alone() {
        let mut profile = Profile::default();
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
        let mut profile = Profile::default();
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
        assert_eq!(identity.serial.as_deref(), Some("SN12345"));
        assert_eq!(identity.edid_hash.as_deref(), Some("aaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn defect_names_do_not_collide() {
        let mut display = DisplayProfile::new(DisplayIdentity::default());
        for _ in 0..3 {
            let name = display.next_defect_name();
            display.defects.push(Defect::Radial(RadialDefect {
                name,
                ..Default::default()
            }));
        }
        assert_eq!(display.defects[2].name(), "Spot 3");
    }

    #[test]
    fn profile_names_cannot_escape_the_config_directory() {
        let path = profile_path(Some("../../etc/passwd")).unwrap();
        assert!(path
            .to_str()
            .unwrap()
            .ends_with("profiles/------etc-passwd.toml"));
    }
}
