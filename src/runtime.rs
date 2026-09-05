//! One headless game session: Luau, kernel systems, fixed clock, and input.

use crate::{
    input::{InputQueue, InputSnapshot},
    kernel::{FIXED_DT, Kernel},
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
    pub ticks: u32,
    pub dropped_ticks: u32,
    pub clamped_seconds: f64,
    pub alpha: f64,
}

pub struct GameRuntime {
    scripts: ScriptHost,
    kernel: Kernel,
    input: InputQueue,
    draw_input: InputSnapshot,
    // Fraction of a fixed tick, rather than an approximated nanosecond duration.
    accumulator: f64,
    completed_ticks: u64,
    overloads: u64,
    logs: Vec<String>,
}

impl GameRuntime {
    pub fn load(root: &Path, limits: ScriptLimits) -> Result<Self, ScriptError> {
        Ok(Self::new(ScriptHost::load(root, limits)?))
    }

    pub fn load_with_data_root(
        root: &Path,
        data_root: &Path,
        limits: ScriptLimits,
    ) -> Result<Self, ScriptError> {
        Ok(Self::new(ScriptHost::load_with_data_root(
            root, data_root, limits,
        )?))
    }

    fn new(scripts: ScriptHost) -> Self {
        Self {
            scripts,
            kernel: Kernel::new(),
            input: InputQueue::default(),
            draw_input: InputSnapshot::default(),
            accumulator: 0.0,
            completed_ticks: 0,
            overloads: 0,
            logs: Vec::new(),
        }
    }

    pub fn state(&self) -> ScriptState {
        self.scripts.state()
    }
    pub fn last_error(&self) -> Option<&ScriptError> {
        self.scripts.last_error()
    }
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }
    pub fn completed_ticks(&self) -> u64 {
        self.completed_ticks
    }
    pub fn overloads(&self) -> u64 {
        self.overloads
    }

    /// Logs from the last public lifecycle call or frame, in callback order.
    /// At most six callback log budgets per frame; replaced on the next call.
    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    pub fn init(&mut self) -> Result<(), ScriptError> {
        self.logs.clear();
        let result = self.scripts.init_in(Some(EngineContext::new(
            &mut self.kernel,
            InputSnapshot::default(),
        )));
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
        let report = FrameReport {
            ticks,
            dropped_ticks: due - ticks,
            clamped_seconds: elapsed_seconds - clamped,
            alpha: self.accumulator,
        };
        if report.dropped_ticks > 0 || report.clamped_seconds > 0.0 {
            self.overloads = self.overloads.saturating_add(1);
        }
        for _ in 0..ticks {
            self.tick()?;
        }
        self.draw_callback(report.alpha)?;
        Ok(report)
    }

    pub fn shutdown(&mut self) -> Result<(), ScriptError> {
        self.logs.clear();
        let result = self.scripts.shutdown_in(Some(EngineContext::new(
            &mut self.kernel,
            InputSnapshot::default(),
        )));
        self.finish_call(result)
    }

    fn tick(&mut self) -> Result<(), ScriptError> {
        let input = self.input.consume();
        let result = self
            .scripts
            .update_in(Some(EngineContext::new(&mut self.kernel, input)));
        self.finish_call(result)?;
        if let Err(error) = self.kernel.fixed_update() {
            self.kernel.stop();
            return Err(self.scripts.fault("systems", error.to_string()));
        }
        self.completed_ticks = self.completed_ticks.saturating_add(1);
        Ok(())
    }

    fn draw_callback(&mut self, alpha: f64) -> Result<(), ScriptError> {
        let result = self.scripts.draw_in(
            alpha,
            Some(EngineContext::new(&mut self.kernel, self.draw_input)),
        );
        self.finish_call(result)
    }

    fn finish_call(&mut self, result: Result<(), ScriptError>) -> Result<(), ScriptError> {
        self.logs.extend(self.scripts.take_logs());
        if matches!(self.state(), ScriptState::Stopped | ScriptState::Faulted) {
            self.kernel.stop();
        }
        result
    }

    fn require_running(&self, phase: &'static str) -> Result<(), ScriptError> {
        if self.state() == ScriptState::Running {
            Ok(())
        } else {
            Err(ScriptError {
                phase,
                message: format!("expected Running, session is {:?}", self.state()),
            })
        }
    }
}
