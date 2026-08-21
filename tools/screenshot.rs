//! Renders the calibration window to a PNG for the README.
//!
//! The window is the real one: this drives `gui::main_window::draw` with an
//! [`App::offline`], so the picture cannot drift away from the program the way
//! a hand-drawn mock-up would. What it invents is only the situation being
//! photographed -- one monitor, three spots, the second one's Edit panel open --
//! so the result does not depend on whose machine it was taken on.
//!
//! ```text
//! cargo run --features screenshot --bin unburn-screenshot -- docs/edit-mode.png
//! ```
//!
//! It needs a display server to render on, because egui draws through OpenGL.

use std::{
    fs::File,
    io::BufWriter,
    path::PathBuf,
    time::{Duration, Instant},
};

use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};

use unburn::{
    app::App,
    compensation::{Defect, RadialDefect, Rgb, Vec2},
    config::Config,
    display::{DisplayIdentity, OutputId, OutputInfo, Transform},
    gui::{icons, main_window, UiState},
    platform::{BackendKind, BackendReport, Support},
};

/// Where the picture goes when the command line does not say.
const DEFAULT_OUTPUT: &str = "docs/edit-mode.png";

/// Window size in points. Tall enough that the whole window fits without a
/// scrollbar, which is the only thing that would misrepresent it.
const DEFAULT_SIZE: [f32; 2] = [1040.0, 700.0];

/// Time to let egui settle before the shutter. The enable switch animates when
/// it first appears, and catching it mid-travel would look like a rendering
/// fault rather than a toggle.
const SETTLE: Duration = Duration::from_millis(700);

/// Abandoned after this long, so a session that cannot present a frame fails
/// instead of hanging a build.
const DEADLINE: Duration = Duration::from_secs(20);

/// Config directory the window is made to report, in place of whatever the
/// machine taking the screenshot happens to use.
const SHOWN_CONFIG_HOME: &str = "/home/you/.config";

fn main() -> Result<(), String> {
    let (output, size) = parse_args()?;

    // Read back by `config::config_dir` while the window draws, so the
    // configuration path on screen is the one a reader would have rather than
    // this machine's.
    std::env::set_var("XDG_CONFIG_HOME", SHOWN_CONFIG_HOME);

    let (config, outputs) = staged_config();
    let mut app = App::offline(
        config,
        PathBuf::from(SHOWN_CONFIG_HOME).join("unburn/config.toml"),
        outputs,
        vec![BackendReport {
            kind: BackendKind::X11,
            support: Support::Full,
        }],
    );

    // The spot whose parameters are on show: the Edit button both opens the
    // panel and puts the calibration disc on the monitor.
    let spot = app
        .selected_display()
        .and_then(|display| display.defects.get(1))
        .map(|defect| defect.id())
        .ok_or("the staged configuration lost its second spot")?;
    app.select_defect(Some(spot));
    app.set_calibration_disc(Some(spot));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("unburn - display compensation")
            .with_inner_size(size)
            .with_icon(icons::window_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "unburn-screenshot",
        options,
        Box::new(move |cc| {
            icons::install(&cc.egui_ctx);
            cc.egui_ctx.all_styles_mut(main_window::apply_ui_scale);
            Ok(Box::new(Shot {
                app,
                ui: UiState {
                    params_open: Some(spot),
                    ..Default::default()
                },
                output,
                started: Instant::now(),
                asked: false,
            }))
        }),
    )
    .map_err(|error| format!("could not open a window to render into: {error}"))
}

fn parse_args() -> Result<(PathBuf, [f32; 2]), String> {
    let mut output = None;
    let mut size = DEFAULT_SIZE;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "usage: unburn-screenshot [PATH] [--size WIDTHxHEIGHT]\n\n\
                     Renders the calibration window with a spot's Edit panel open\n\
                     and writes it to PATH (default {DEFAULT_OUTPUT})."
                );
                std::process::exit(0);
            }
            "--size" => {
                let value = args.next().ok_or("--size needs a WIDTHxHEIGHT argument")?;
                size = parse_size(&value)?;
            }
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            path => output = Some(PathBuf::from(path)),
        }
    }

    Ok((
        output.unwrap_or_else(|| PathBuf::from(DEFAULT_OUTPUT)),
        size,
    ))
}

fn parse_size(value: &str) -> Result<[f32; 2], String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{value} is not a WIDTHxHEIGHT size"))?;
    let parse = |side: &str| {
        side.trim()
            .parse::<f32>()
            .map_err(|_| format!("{value} is not a WIDTHxHEIGHT size"))
    };
    Ok([parse(width)?, parse(height)?])
}

/// The situation being photographed: a TV with three modelled blemishes, the
/// middle one tinted so the per-channel sliders are the ones on show.
fn staged_config() -> (Config, Vec<OutputInfo>) {
    let identity = DisplayIdentity {
        connector: Some("HDMI-A-1".into()),
        manufacturer: Some("SAM".into()),
        model: Some("QN90B".into()),
        serial: Some("SN0123456".into()),
        edid_hash: Some("6f1c4a90b2d35e78".into()),
    };

    let spots = [
        RadialDefect {
            center: Vec2::new(0.62, 0.43),
            radius: Vec2::new(0.075, 0.13),
            strength: Rgb::splat(0.11),
            falloff: 1.0,
            ..Default::default()
        },
        // The one on show, tinted so the panel displays the per-channel
        // sliders rather than the single neutral one.
        RadialDefect {
            center: Vec2::new(0.31, 0.68),
            radius: Vec2::new(0.052, 0.09),
            strength: Rgb::new(0.094, 0.061, 0.048),
            falloff: 1.25,
            ..Default::default()
        },
        RadialDefect {
            center: Vec2::new(0.84, 0.22),
            radius: Vec2::new(0.04, 0.07),
            strength: Rgb::splat(0.05),
            falloff: 0.8,
            enabled: false,
            ..Default::default()
        },
    ];

    let mut config = Config::default();
    let display = config.entry(&identity);
    display.name = "Living Room TV".into();
    display.defects = spots.into_iter().map(Defect::Radial).collect();

    let output = OutputInfo {
        id: OutputId(1),
        identity,
        width: 3840,
        height: 2160,
        position: (0, 0),
        scale: 1.0,
        transform: Transform::Normal,
        refresh_mhz: Some(59_940),
    };
    (config, vec![output])
}

struct Shot {
    app: App,
    ui: UiState,
    output: PathBuf,
    started: Instant,
    /// Whether the screenshot has already been asked for; the reply arrives a
    /// frame or two later and must not be requested again in between.
    asked: bool,
}

impl eframe::App for Shot {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            main_window::draw(ui, &mut self.app, &mut self.ui);
        });

        let ctx = ui.ctx().clone();
        if let Some(image) = delivered_screenshot(&ctx) {
            match write_png(&self.output, &image) {
                Ok(()) => println!(
                    "[ok] wrote {} ({}x{})",
                    self.output.display(),
                    image.size[0],
                    image.size[1]
                ),
                Err(error) => eprintln!("[error] {error}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        let waited = self.started.elapsed();
        if !self.asked && waited >= SETTLE {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.asked = true;
        }
        if waited >= DEADLINE {
            eprintln!("[error] the display server never returned a frame to capture");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}

fn delivered_screenshot(ctx: &egui::Context) -> Option<std::sync::Arc<egui::ColorImage>> {
    ctx.input(|input| {
        input.events.iter().find_map(|event| match event {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    })
}

fn write_png(path: &PathBuf, image: &egui::ColorImage) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }

    let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b(), pixel.a()]);
    }

    let file =
        File::create(path).map_err(|error| format!("creating {}: {error}", path.display()))?;
    PngEncoder::new(BufWriter::new(file))
        .write_image(
            &bytes,
            image.size[0] as u32,
            image.size[1] as u32,
            ExtendedColorType::Rgba8,
        )
        .map_err(|error| format!("writing {}: {error}", path.display()))
}
