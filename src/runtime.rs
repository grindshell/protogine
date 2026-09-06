//! One headless game session: Luau, kernel systems, fixed clock, and input.

use crate::{
    drawing::DrawCommand,
    input::{InputQueue, InputSnapshot},
    kernel::{FIXED_DT, Kernel},
    manifest::GameManifest,
    scripting::{EngineContext, ScriptError, ScriptHost, ScriptLimits, ScriptState},
};
use std::path::Path;

const MAX_FRAME_SECONDS: f64 = 0.25;
const MAX_FRAME_TICKS: u32 = 5;
// In tick units (about 17 femtoseconds): absorb rounding from elapsed-time
// conversion and accumulation, without rounding ordinary sub-tick durations.
const TICK_ROUNDING_TOLERANCE: f64 = 1e-12;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameReport {
    /// Completed update and system passes, excluding a callback that stops early.
    pub ticks: u32,
    pub dropped_ticks: u32,
    /// Wall time discarded by the 250 ms clamp: elapsed minus accepted time.
    /// For a 1.0-second frame this is 0.75 seconds, before catch-up tick dropping.
    pub clamped_seconds: f64,
    pub alpha: f64,
}

pub struct GameRuntime {
    // Field order also guarantees VM destruction before native teardown on Drop.
    scripts: Option<ScriptHost>,
    state: ScriptState,
    last_error: Option<ScriptError>,
    kernel: Kernel,
    input: InputQueue,
    draw_input: InputSnapshot,
    // Fraction of a fixed tick, rather than an approximated nanosecond duration.
    accumulator: f64,
    completed_ticks: u64,
    overloads: u64,
    logs: Vec<String>,
    #[cfg(feature = "native-plugins")]
    plugins: Option<crate::plugins::PluginSet>,
}

impl GameRuntime {
    pub fn load(root: &Path, limits: ScriptLimits) -> Result<Self, ScriptError> {
        Self::load_inner(root, None, limits, None, false)
    }

    pub fn load_seeded(root: &Path, limits: ScriptLimits, seed: i32) -> Result<Self, ScriptError> {
        Self::load_inner(root, None, limits, Some(seed), false)
    }

    pub fn load_with_data_root(
        root: &Path,
        data_root: &Path,
        limits: ScriptLimits,
    ) -> Result<Self, ScriptError> {
        Self::load_inner(root, Some(data_root), limits, None, false)
    }

    /// Load a shipped bundle, including its explicitly declared native plugins.
    ///
    /// # Safety
    /// Libraries and their dependencies must be trusted and obey the SDK's ABI,
    /// pointer, lifetime, thread and unwind contracts for the entire session.
    /// Keep bundle/dependency files stable. See PluginSet::load_trusted.
    #[cfg(feature = "native-plugins")]
    pub unsafe fn load_trusted(
        root: &Path,
        data_root: Option<&Path>,
        limits: ScriptLimits,
        seed: Option<i32>,
    ) -> Result<Self, ScriptError> {
        Self::load_inner(root, data_root, limits, seed, true)
    }

    fn load_inner(
        root: &Path,
        data_root: Option<&Path>,
        limits: ScriptLimits,
        seed: Option<i32>,
        trusted: bool,
    ) -> Result<Self, ScriptError> {
        let manifest = GameManifest::load(root).map_err(|e| ScriptError {
            phase: "load",
            message: e.to_string(),
        })?;
        if !manifest.plugins.is_empty() && !trusted {
            return Err(ScriptError { phase: "load", message: "native plugin declarations require the native-plugins feature and explicit load_trusted".into() });
        }
        #[cfg(feature = "native-plugins")]
        let mut plugins = if trusted {
            // SAFETY: Only the unsafe public entry sets trusted=true; it accepts
            // all native execution obligations through teardown.
            Some(
                unsafe { crate::plugins::PluginSet::load_trusted(root, &manifest) }.map_err(
                    |e| ScriptError {
                        phase: "plugins.load",
                        message: e.to_string(),
                    },
                )?,
            )
        } else {
            None
        };
        let scripts = match ScriptHost::load_with_roots(root, data_root, limits, seed) {
            Ok(scripts) => scripts,
            Err(failure) => {
                #[cfg(feature = "native-plugins")]
                let mut failure = failure;
                #[cfg(feature = "native-plugins")]
                if let Some(plugins) = &mut plugins {
                    if let Err(cleanup) = plugins.shutdown() {
                        failure.message.push_str(&format!("\ncleanup: {cleanup}"));
                    }
                    for log in plugins.take_logs() {
                        failure.message.push_str(&format!("\n{log}"));
                    }
                }
                return Err(failure);
            }
        };
        Ok(Self {
            scripts: Some(scripts),
            state: ScriptState::Loaded,
            last_error: None,
            kernel: Kernel::new(),
            input: InputQueue::default(),
            draw_input: InputSnapshot::default(),
            accumulator: 0.0,
            completed_ticks: 0,
            overloads: 0,
            logs: Vec::new(),
            #[cfg(feature = "native-plugins")]
            plugins,
        })
    }

    pub fn state(&self) -> ScriptState {
        self.state
    }

    pub fn last_error(&self) -> Option<&ScriptError> {
        self.last_error.as_ref()
    }

    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    pub fn draw_commands(&self) -> &[DrawCommand] {
        self.scripts.as_ref().map_or(&[], ScriptHost::draw_commands)
    }

    pub fn completed_ticks(&self) -> u64 {
        self.completed_ticks
    }

    pub fn overloads(&self) -> u64 {
        self.overloads
    }

    /// Logs from the last public lifecycle call or frame, in callback order.
    /// At most six script callback budgets per frame, plus bounded native
    /// startup/cleanup logs when applicable. Replaced on the next call.
    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    pub fn init(&mut self) -> Result<(), ScriptError> {
        self.require_state("init", ScriptState::Loaded)?;
        self.logs.clear();
        #[cfg(feature = "native-plugins")]
        if let Some(plugins) = &mut self.plugins {
            self.logs.extend(plugins.take_logs());
        }
        let engine = EngineContext::new(&mut self.kernel, InputSnapshot::default());
        #[cfg(feature = "native-plugins")]
        let engine = engine.with_plugins(self.plugins.as_mut());
        let result = self.scripts.as_mut().unwrap().init_in(Some(engine));
        self.finish_call(result)
    }

    /// Exactly one fixed update and system pass. Leaves the frame fraction intact.
    pub fn step(&mut self, input: InputSnapshot) -> Result<(), ScriptError> {
        self.require_running("update")?;
        self.logs.clear();
        self.draw_input = self.input.sample(input);
        self.tick()
    }

    pub fn draw(&mut self, alpha: f64) -> Result<(), ScriptError> {
        self.require_running("draw")?;
        self.logs.clear();
        self.draw_callback(alpha)
    }

    /// Poll input once, perform bounded catch-up, then draw once.
    /// Invalid time leaves the clock, queued input, and session untouched.
    pub fn frame(
        &mut self,
        elapsed_seconds: f64,
        input: InputSnapshot,
    ) -> Result<FrameReport, ScriptError> {
        self.require_running("frame")?;
        if !elapsed_seconds.is_finite() || elapsed_seconds < 0.0 {
            return Err(ScriptError {
                phase: "frame",
                message: "elapsed time must be finite and nonnegative".into(),
            });
        }
        self.logs.clear();
        self.draw_input = self.input.sample(input);
        let clamped = elapsed_seconds.min(MAX_FRAME_SECONDS);
        self.accumulator += clamped / FIXED_DT;
        let nearest = self.accumulator.round();
        if nearest > 0.0 && (self.accumulator - nearest).abs() <= TICK_ROUNDING_TOLERANCE {
            self.accumulator = nearest;
        }
        let due = self.accumulator.floor() as u32;
        self.accumulator -= f64::from(due);
        let ticks = due.min(MAX_FRAME_TICKS);
        let mut report = FrameReport {
            ticks: 0,
            dropped_ticks: due - ticks,
            clamped_seconds: elapsed_seconds - clamped,
            alpha: self.accumulator,
        };
        if report.dropped_ticks > 0 || report.clamped_seconds > 0.0 {
            self.overloads = self.overloads.saturating_add(1);
        }
        for _ in 0..ticks {
            self.tick()?;
            if self.state != ScriptState::Running {
                return Ok(report);
            }
            report.ticks += 1;
        }
        self.draw_callback(report.alpha)?;
        Ok(report)
    }

    pub fn shutdown(&mut self) -> Result<(), ScriptError> {
        self.logs.clear();
        if self.scripts.is_none() {
            return Ok(());
        }
        let engine = EngineContext::new(&mut self.kernel, InputSnapshot::default());
        #[cfg(feature = "native-plugins")]
        let engine = engine.with_plugins(self.plugins.as_mut());
        let result = self.scripts.as_mut().unwrap().shutdown_in(Some(engine));
        self.finish_call(result)
    }

    fn tick(&mut self) -> Result<(), ScriptError> {
        if self.state != ScriptState::Running {
            return Ok(());
        }
        let input = self.input.consume();
        let engine = EngineContext::new(&mut self.kernel, input);
        #[cfg(feature = "native-plugins")]
        let engine = engine.with_plugins(self.plugins.as_mut());
        let result = self.scripts.as_mut().unwrap().update_in(Some(engine));
        self.finish_tick(result)
    }

    fn finish_tick(&mut self, result: Result<(), ScriptError>) -> Result<(), ScriptError> {
        self.finish_call(result)?;
        // A successful callback may still stop the session and release the VM.
        if self.state != ScriptState::Running {
            return Ok(());
        }
        if let Err(error) = self.kernel.fixed_update() {
            let error = self
                .scripts
                .as_mut()
                .unwrap()
                .fault("systems", error.to_string());
            return self.finish_call(Err(error));
        }
        self.completed_ticks = self.completed_ticks.saturating_add(1);
        Ok(())
    }

    fn draw_callback(&mut self, alpha: f64) -> Result<(), ScriptError> {
        if self.state != ScriptState::Running {
            return Ok(());
        }
        let engine = EngineContext::new(&mut self.kernel, self.draw_input);
        #[cfg(feature = "native-plugins")]
        let engine = engine.with_plugins(self.plugins.as_mut());
        let result = self.scripts.as_mut().unwrap().draw_in(alpha, Some(engine));
        self.finish_call(result)
    }

    fn finish_call(&mut self, result: Result<(), ScriptError>) -> Result<(), ScriptError> {
        #[cfg(feature = "native-plugins")]
        if let Some(plugins) = &mut self.plugins {
            self.logs.extend(plugins.take_logs());
        }
        let scripts = self.scripts.as_mut().unwrap();
        self.logs.extend(scripts.take_logs());
        self.state = scripts.state();
        self.last_error = scripts.last_error().cloned();
        if matches!(self.state(), ScriptState::Stopped | ScriptState::Faulted) {
            self.kernel.stop();
            // Release every VM reference before calling native teardown/unloading.
            drop(self.scripts.take());
            #[cfg(feature = "native-plugins")]
            if let Some(mut plugins) = self.plugins.take() {
                let cleanup = plugins.shutdown();
                self.logs.extend(plugins.take_logs());
                if let Err(cleanup) = cleanup {
                    let error = ScriptError {
                        phase: "plugins.shutdown",
                        message: cleanup.to_string(),
                    };
                    if result.is_ok() {
                        self.state = ScriptState::Faulted;
                        self.last_error = Some(error.clone());
                        return Err(error);
                    }
                    // The original callback/system error remains primary.
                    self.logs.push(format!("cleanup: {error}"));
                }
            }
        }
        result
    }

    fn require_running(&self, phase: &'static str) -> Result<(), ScriptError> {
        self.require_state(phase, ScriptState::Running)
    }

    fn require_state(&self, phase: &'static str, expected: ScriptState) -> Result<(), ScriptError> {
        if self.state() == expected {
            Ok(())
        } else {
            Err(ScriptError {
                phase,
                message: format!("expected {expected:?}, session is {:?}", self.state()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_terminal_callback_skips_systems_and_remaining_callbacks() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.luau"), "return {}").unwrap();
        let mut runtime = GameRuntime::load(root.path(), ScriptLimits::default()).unwrap();
        runtime.init().unwrap();

        // Model a callback that returns Ok after stopping, without introducing
        // a script-stop API before its authoring contract is defined.
        let result = runtime.scripts.as_mut().unwrap().shutdown();
        runtime.finish_tick(result).unwrap();
        assert_eq!(runtime.state(), ScriptState::Stopped);
        assert!(runtime.scripts.is_none());
        assert!(runtime.last_error().is_none());
        assert_eq!(runtime.completed_ticks(), 0);

        for _ in 0..MAX_FRAME_TICKS {
            runtime.tick().unwrap();
        }
        runtime.draw_callback(0.0).unwrap();
        assert_eq!(runtime.completed_ticks(), 0);
        assert!(runtime.draw_commands().is_empty());
        // Public entry points still reject calls on a stopped session.
        assert!(runtime.step(InputSnapshot::default()).is_err());
        assert!(runtime.frame(FIXED_DT, InputSnapshot::default()).is_err());
        assert!(runtime.draw(0.0).is_err());
    }
}
