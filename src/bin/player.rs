#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use macroquad::prelude::*;
use protogine::bundle::discover_bundle;
use std::{cell::Cell, process::ExitCode, rc::Rc};

#[path = "player/capture.rs"]
mod capture;

use capture::PlayerOptions;

fn window_conf(options: &PlayerOptions) -> Conf {
    Conf {
        window_title: "Protogine Player".to_owned(),
        window_width: i32::from(options.width),
        window_height: i32::from(options.height),
        window_resizable: options.capture.is_none(),
        high_dpi: options.capture.is_none(),
        ..Default::default()
    }
}

struct StartupScreen {
    title: &'static str,
    description: &'static str,
}

impl StartupScreen {
    fn discover() -> Self {
        let result = std::env::current_exe().and_then(|path| discover_bundle(&path));
        match result {
            Ok(None) => Self {
                title: "Missing game data",
                description: "No game was found alongside this player.",
            },
            Ok(Some(bundle)) => {
                eprintln!("Game data detected at {}", bundle.root().display());
                Self {
                    title: "Game data detected",
                    description: "This player does not load games yet.",
                }
            }
            Err(error) => {
                eprintln!("Cannot access game data: {error}");
                Self {
                    title: "Game data unavailable",
                    description: "The game data could not be accessed.",
                }
            }
        }
    }

    fn draw(&self) {
        let background = Color::from_rgba(20, 24, 32, 255);
        let foreground = Color::from_rgba(235, 239, 245, 255);
        let muted = Color::from_rgba(154, 166, 185, 255);
        let accent = Color::from_rgba(112, 204, 177, 255);
        clear_background(background);

        let width = screen_width().max(1.0);
        let height = screen_height().max(1.0);
        let scale = (width / 800.0).min(height / 500.0).min(1.5);
        let center_y = height * 0.5;

        // A small tile motif drawn entirely in code; startup needs no game assets.
        let tile_size = 18.0 * scale;
        let pitch = 24.0 * scale;
        let left = (width - (pitch * 2.0 + tile_size)) * 0.5;
        let top = center_y - 130.0 * scale;
        for row in 0..3 {
            for column in 0..3 {
                let x = left + column as f32 * pitch;
                let y = top + row as f32 * pitch;
                if row == 2 && column == 2 {
                    draw_rectangle_lines(x, y, tile_size, tile_size, 2.0 * scale, muted);
                } else {
                    draw_rectangle(x, y, tile_size, tile_size, accent);
                }
            }
        }

        // Cache every line's glyphs before queuing any text. A font atlas resize
        // mid-draw would invalidate earlier UVs on startup or a window resize.
        let lines = [
            ("PROTOGINE", center_y - 35.0 * scale, 16, accent),
            (self.title, center_y + 20.0 * scale, 36, foreground),
            (self.description, center_y + 58.0 * scale, 20, muted),
            ("Escape to close", height - 30.0 * scale, 16, muted),
        ]
        .map(|(text, baseline, size, color)| {
            let font_size = ((size as f32 * scale).round() as u16).max(1);
            let dimensions = measure_text(text, None, font_size, 1.0);
            (text, baseline, font_size, color, dimensions.width)
        });
        for (text, baseline, font_size, color, text_width) in lines {
            draw_text_ex(
                text,
                (width - text_width) * 0.5,
                baseline,
                TextParams {
                    font_size,
                    color,
                    ..Default::default()
                },
            );
        }
    }
}

fn main() -> ExitCode {
    let options = match PlayerOptions::from_env() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("Invalid Player configuration: {error}");
            return ExitCode::from(2);
        }
    };

    // A window closed before the capture finishes must not report success.
    // The native event loop and its future run on the same thread.
    let succeeded = Rc::new(Cell::new(options.capture.is_none()));
    let run_succeeded = Rc::clone(&succeeded);
    macroquad::Window::from_config(window_conf(&options), async move {
        match run(options).await {
            Ok(()) => run_succeeded.set(true),
            Err(error) => eprintln!("Player capture failed: {error}"),
        }
    });
    if succeeded.get() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

async fn run(options: PlayerOptions) -> Result<(), String> {
    if options.capture.is_some() {
        prevent_quit();
    }
    let screen = StartupScreen::discover();
    let mut rendered_frames = 0;
    loop {
        if is_key_pressed(KeyCode::Escape) || is_quit_requested() {
            return if options.capture.is_some() {
                Err("window closed before the screenshot was saved".to_owned())
            } else {
                Ok(())
            };
        }
        screen.draw();
        if let Some(capture) = &options.capture {
            rendered_frames += 1;
            if rendered_frames == capture.frame {
                let screenshot = get_screen_data();
                if (screenshot.width, screenshot.height) != (options.width, options.height) {
                    return Err(format!(
                        "framebuffer is {}x{}, expected {}x{}; the platform resized the window",
                        screenshot.width, screenshot.height, options.width, options.height
                    ));
                }
                capture::save_png(screenshot, &capture.path)?;
                eprintln!("Screenshot saved to {}", capture.path.display());
                return Ok(());
            }
        }
        next_frame().await;
    }
}
