#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use macroquad::prelude::*;
use protogine::{
    bundle::discover_bundle,
    input::{Button, Buttons, InputSnapshot},
    rendering::MacroquadRenderer,
    runtime::GameRuntime,
    scripting::{ScriptError, ScriptLimits},
};
use std::{
    cell::Cell,
    process::ExitCode,
    rc::Rc,
    time::{Duration, Instant},
};

/// Bounded wait for a capture's asset and upload drain, independent of any
/// script deadline. A timed-out drain fails the capture rather than saving an
/// incomplete image.
const DRAIN_WATCHDOG: Duration = Duration::from_secs(10);

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

struct PlayerSession {
    runtime: Option<GameRuntime>,
    screen: Option<StartupScreen>,
    faulted: bool,
}

impl PlayerSession {
    fn discover(capturing: bool) -> Self {
        let mut session = Self {
            runtime: None,
            screen: None,
            faulted: false,
        };
        match std::env::current_exe().and_then(|path| discover_bundle(&path)) {
            Ok(None) => {
                session.screen = Some(StartupScreen {
                    title: "Missing game data",
                    description: "No game was found alongside this player.",
                })
            }
            Err(error) => {
                eprintln!("Cannot access game data: {error}");
                session.screen = Some(StartupScreen {
                    title: "Game data unavailable",
                    description: "The game data could not be accessed.",
                });
            }
            Ok(Some(bundle)) => {
                // SAFETY: The shipped bundle and explicitly declared native plugins
                // are trusted application code. The Player supplies no DLL sandbox.
                let loaded = unsafe {
                    GameRuntime::load_trusted(
                        bundle.root(),
                        None,
                        ScriptLimits::default(),
                        capturing.then_some(0),
                    )
                };
                match loaded {
                    Ok(runtime) => {
                        session.runtime = Some(runtime);
                        session.call(GameRuntime::init);
                    }
                    Err(error) => session.fault(error),
                }
            }
        }
        session
    }

    fn fault(&mut self, error: ScriptError) {
        eprintln!("Game error: {error}");
        self.runtime = None; // Fault cleanup finished; no further Luau callbacks.
        self.faulted = true;
        self.screen = Some(StartupScreen {
            title: "Game error",
            description: "The game stopped. See stderr for details.",
        });
    }

    fn call(&mut self, call: impl FnOnce(&mut GameRuntime) -> Result<(), ScriptError>) {
        if let Some(runtime) = &mut self.runtime {
            let result = call(runtime);
            for message in runtime.take_logs() {
                eprintln!("{message}");
            }
            if let Err(error) = result {
                self.fault(error);
            }
        }
    }

    /// Give the renderer this session's images. The renderer's pool outlives
    /// every session and is never recreated, so attaching only rebinds it.
    fn attach(&mut self, renderer: &mut MacroquadRenderer) {
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        let Some(session) = runtime.assets().map(|store| store.session()) else {
            return;
        };
        renderer.attach(session);
        runtime.attach_gpu();
    }

    /// Admit newly ready images and run one bounded upload pass, then queue the
    /// acknowledgements for the next update boundary. No script runs here.
    fn service_uploads(&mut self, renderer: &mut MacroquadRenderer) {
        let outcome = {
            let Some(runtime) = &mut self.runtime else {
                return;
            };
            let ready = runtime.take_ready_images();
            let serviced = {
                let Some(store) = runtime.assets() else {
                    return;
                };
                for id in ready {
                    renderer.admit(id);
                }
                renderer.service_uploads(&store)
            };
            match serviced {
                Ok(acks) => {
                    runtime.acknowledge_uploads(acks);
                    Ok(())
                }
                Err(error) => Err(error.to_string()),
            }
        };
        if let Err(message) = outcome {
            self.presentation_fault(message);
        }
    }

    /// Settle every outstanding asset job and upload, then publish, so a
    /// capture draws against a readiness schedule that repeats. It advances no
    /// simulation and does not count as a rendered frame.
    fn drain(&mut self, renderer: &mut MacroquadRenderer) -> Result<(), String> {
        let deadline = Instant::now() + DRAIN_WATCHDOG;
        {
            let Some(runtime) = &mut self.runtime else {
                return Ok(());
            };
            let settled = runtime
                .drain_assets(DRAIN_WATCHDOG)
                .map_err(|error| error.to_string())?;
            if !settled {
                return Err("asset loading did not settle within the drain watchdog".into());
            }
        }
        // Service once unconditionally: the drain above settled every CPU job,
        // so nothing is admitted yet and a leading `pending_uploads` test would
        // skip the pass that admits them. Afterwards the ready queue is empty
        // and no new job can settle inside this loop, so the pending count is
        // the whole condition.
        loop {
            self.service_uploads(renderer);
            if self.runtime.is_none() {
                return Ok(()); // The fault path already reported it.
            }
            if renderer.pending_uploads() == 0 {
                break;
            }
            if Instant::now() >= deadline {
                return Err("image uploads did not settle within the drain watchdog".into());
            }
        }
        let Some(runtime) = &mut self.runtime else {
            return Ok(());
        };
        // Commit the acknowledgements before the draw that will be captured.
        runtime.publish_assets().map_err(|error| error.to_string())
    }

    fn presentation_fault(&mut self, message: String) {
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        let error = runtime.presentation_fault(message);
        self.fault(error);
    }

    fn draw(&mut self, renderer: &mut MacroquadRenderer) {
        if let Some(screen) = &self.screen {
            screen.draw();
            return;
        }
        let result = match &self.runtime {
            Some(runtime) => match runtime.assets() {
                Some(store) => renderer.render(&store, runtime.draw_commands()),
                None => Ok(()),
            },
            None => return,
        };
        if let Err(error) = result {
            // Validation precedes submission, so a refused list queued nothing;
            // present the game error screen for this frame instead.
            self.presentation_fault(error.to_string());
            if let Some(screen) = &self.screen {
                screen.draw();
            }
        }
    }

    fn exit_code(&self, capture_failed: bool) -> u8 {
        if self.faulted {
            3
        } else {
            u8::from(capture_failed)
        }
    }
}

fn poll_input() -> InputSnapshot {
    let keys = [
        (Button::Up, KeyCode::Up),
        (Button::Down, KeyCode::Down),
        (Button::Left, KeyCode::Left),
        (Button::Right, KeyCode::Right),
        (Button::Action, KeyCode::Space),
        (Button::Cancel, KeyCode::Backspace),
    ];
    let buttons = |check: fn(KeyCode) -> bool| {
        Buttons::new(
            keys.iter()
                .filter(|(_, key)| check(*key))
                .map(|(button, _)| *button),
        )
    };
    InputSnapshot {
        held: buttons(is_key_down),
        pressed: buttons(is_key_pressed),
        released: buttons(is_key_released),
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
    let status = Rc::new(Cell::new(u8::from(options.capture.is_some())));
    let run_status = Rc::clone(&status);
    macroquad::Window::from_config(window_conf(&options), async move {
        run(options, run_status).await;
    });
    ExitCode::from(status.get())
}

async fn run(options: PlayerOptions, status: Rc<Cell<u8>>) {
    prevent_quit(); // Intercept native close for orderly script shutdown too.
    // The pool belongs to the graphics context, not to a game session, and is
    // never recreated. It is dropped below, inside this future, with the
    // context still alive and after the final readback.
    let mut renderer = MacroquadRenderer::new();
    let mut session = PlayerSession::discover(options.capture.is_some());
    session.attach(&mut renderer);
    let mut rendered_frames = 0;
    let mut capture_failed = options.capture.is_some();
    let mut drain_error = None;
    loop {
        if is_key_pressed(KeyCode::Escape) || is_quit_requested() {
            session.call(GameRuntime::shutdown);
            if options.capture.is_some() {
                eprintln!("Player capture failed: window closed before the screenshot was saved");
            }
            break;
        }
        // Service uploads from the last committed snapshot, then queue the
        // acknowledgements, before the frame that may publish new commands.
        session.service_uploads(&mut renderer);
        if options.capture.is_some() {
            // Drain after init and after each measured step, so the captured
            // draw always sees a settled readiness schedule.
            if rendered_frames == 0
                && let Err(error) = session.drain(&mut renderer)
            {
                drain_error = Some(error);
                break;
            }
            session.call(|runtime| runtime.step(InputSnapshot::default()));
            if let Err(error) = session.drain(&mut renderer) {
                drain_error = Some(error);
                break;
            }
            session.call(|runtime| runtime.draw(0.0));
        } else {
            session.call(|runtime| {
                runtime
                    .frame(f64::from(get_frame_time()), poll_input())
                    .map(|_| ())
            });
        }
        session.draw(&mut renderer);
        if let Some(capture) = &options.capture {
            rendered_frames += 1;
            if rendered_frames == capture.frame {
                if let Some(runtime) = &session.runtime {
                    eprintln!(
                        "Game capture: {} completed ticks",
                        runtime.completed_ticks()
                    );
                }
                session.call(GameRuntime::shutdown);
                if session.faulted {
                    // A fault always sets the error screen, so this redraws
                    // that screen and never re-renders from a released store.
                    session.draw(&mut renderer);
                }
                // Textures stay populated through the readback: get_screen_data
                // is what flushes and reads the queued frame.
                let result = save_capture(&options, capture);
                if let Err(error) = &result {
                    eprintln!("Player capture failed: {error}");
                }
                capture_failed = result.is_err();
                break;
            }
        }
        status.set(session.exit_code(capture_failed));
        next_frame().await;
    }
    if let Some(error) = drain_error {
        eprintln!("Player capture failed: {error}");
    }
    // Only now is queued work submitted and read back, so the session's slots
    // can be cleared. The pool itself is dropped with the context still alive.
    renderer.retire();
    status.set(session.exit_code(capture_failed));
}

fn save_capture(options: &PlayerOptions, capture: &capture::Capture) -> Result<(), String> {
    let screenshot = get_screen_data();
    if (screenshot.width, screenshot.height) != (options.width, options.height) {
        return Err(format!(
            "framebuffer is {}x{}, expected {}x{}; the platform resized the window",
            screenshot.width, screenshot.height, options.width, options.height
        ));
    }
    capture::save_png(screenshot, &capture.path)?;
    eprintln!("Screenshot saved to {}", capture.path.display());
    Ok(())
}
