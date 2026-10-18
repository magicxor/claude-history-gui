//! The in-process QA runner (DESIGN §15.3): runs a scenario on the UI thread
//! after the first frame and writes `report.txt` / `report.json`. WP7
//! (feature `qa`).
//!
//! No OS input is ever injected: keys are `Window::dispatch_keystroke`,
//! actions `Window::dispatch_action`, mouse events
//! `Window::dispatch_event(PlatformInput::Mouse…)` at the centre of an
//! element registered with `qa_id` — all inside this process. Side effects
//! go to `RecordingServices` (the QA services, `Services::for_launch`).
//!
//! Report: one `PASS|FAIL|SKIP <line> <step> [detail]` line per step plus a
//! summary; exit code 0 without a FAIL, else 1. `expect` re-checks for up to
//! [`EXPECT_GRACE`] so asynchronous effects have settled; `shot` writes
//! `<out>/NAME.request` and waits up to [`SHOT_TIMEOUT`] for `<out>/NAME.png`
//! (the screenshot watcher, `tools/qa/qa-shot-watch.ps1`), else SKIP.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::{
    App, AsyncWindowContext, Global, Keystroke, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PlatformInput, Task, Window, px, size,
};

use super::dsl::{self, ClickKind, Cmp, Line, Step};
use super::probe;
use crate::exit;
use crate::globals::{
    BusyMeasure, BusySpan, BusyTimer, SubSpans, UiBusy, calibrate_cycles, cycles_to_ms,
};
use crate::model::QaArgs;
use crate::qa_ext::registry;
use crate::services::services;
use crate::theme_tokens;

/// How long `expect` keeps re-checking before it fails.
pub const EXPECT_GRACE: Duration = Duration::from_millis(1000);
/// How long `shot` waits for the screenshot watcher.
pub const SHOT_TIMEOUT: Duration = Duration::from_secs(15);
/// Pause after an input step so deferred effects settle.
const SETTLE: Duration = Duration::from_millis(40);
/// How recent a registered element's bounds must be for `click`.
const BOUNDS_MAX_AGE: Duration = Duration::from_millis(1500);
/// The poll interval of waits.
const POLL: Duration = Duration::from_millis(40);

/// The outcome of one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Skip,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

/// One reported step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepResult {
    pub line: usize,
    pub step: String,
    pub status: Status,
    pub detail: String,
}

/// The finished run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub scenario: String,
    pub steps: Vec<StepResult>,
}

impl Report {
    pub fn count(&self, status: Status) -> usize {
        self.steps
            .iter()
            .filter(|step| step.status == status)
            .count()
    }

    /// 0 without a failed step, else 1.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.count(Status::Fail) > 0)
    }

    /// `report.txt`: one line per step plus a summary.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for step in &self.steps {
            text.push_str(&format!(
                "{} {} {}{}\n",
                step.status.as_str(),
                step.line,
                step.step,
                if step.detail.is_empty() {
                    String::new()
                } else {
                    format!(" -- {}", step.detail)
                }
            ));
        }
        text.push_str(&format!(
            "# {}: {} pass, {} fail, {} skip\n",
            self.scenario,
            self.count(Status::Pass),
            self.count(Status::Fail),
            self.count(Status::Skip)
        ));
        text
    }

    /// `report.json`.
    pub fn to_json(&self) -> String {
        let steps: Vec<serde_json::Value> = self
            .steps
            .iter()
            .map(|step| {
                serde_json::json!({
                    "line": step.line,
                    "step": step.step,
                    "status": step.status.as_str(),
                    "detail": step.detail,
                })
            })
            .collect();
        let value = serde_json::json!({
            "scenario": self.scenario,
            "pass": self.count(Status::Pass),
            "fail": self.count(Status::Fail),
            "skip": self.count(Status::Skip),
            "exit_code": self.exit_code(),
            "steps": steps,
        });
        serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
    }

    /// Write both report files into `dir`.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("report.txt"), self.to_text())?;
        std::fs::write(dir.join("report.json"), self.to_json())
    }
}

/// Compare a probe reading with the expected value.
pub fn judge(actual: Option<&str>, cmp: Cmp, expected: &str) -> (Status, String) {
    match actual {
        None => (Status::Fail, "probe not available".to_string()),
        Some(actual) if cmp.holds(actual, expected) => (Status::Pass, String::new()),
        Some(actual) => (
            Status::Fail,
            format!("got '{actual}', wanted {} '{expected}'", cmp.symbol()),
        ),
    }
}

/// The key of a typed character (`type` goes through keystrokes).
pub fn keystroke_for_char(c: char) -> Keystroke {
    let mut modifiers = Modifiers::none();
    let mut key = c.to_string();
    let mut key_char = c.to_string();
    if c == ' ' {
        key = "space".to_string();
    } else if c.is_uppercase() {
        modifiers.shift = true;
        key = c.to_lowercase().collect();
    } else if c == '\n' {
        key = "enter".to_string();
    } else if c == '\t' {
        key = "tab".to_string();
    }
    if c == '\n' {
        key_char = "\n".to_string();
    }
    Keystroke {
        modifiers,
        key,
        key_char: Some(key_char),
    }
}

/// The runner's task (kept alive for the process).
struct QaRunner {
    _task: Task<()>,
}

impl Global for QaRunner {}

/// Start the scenario of `args` in `window` (called once after the first frame).
pub fn start(args: QaArgs, window: &mut Window, cx: &mut App) {
    let scenario_name = args
        .scenario
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| args.scenario.display().to_string());
    let lines = match std::fs::read_to_string(&args.scenario) {
        Ok(text) => match dsl::parse(&text) {
            Ok(lines) => lines,
            Err(errors) => {
                let report = Report {
                    scenario: scenario_name,
                    steps: errors
                        .into_iter()
                        .map(|error| StepResult {
                            line: error.number,
                            step: "parse".to_string(),
                            status: Status::Fail,
                            detail: error.message,
                        })
                        .collect(),
                };
                finish(&report, &args.out_dir, 0, cx);
                return;
            }
        },
        Err(error) => {
            let report = Report {
                scenario: scenario_name,
                steps: vec![StepResult {
                    line: 0,
                    step: "read scenario".to_string(),
                    status: Status::Fail,
                    detail: format!("{}: {error}", args.scenario.display()),
                }],
            };
            finish(&report, &args.out_dir, 0, cx);
            return;
        }
    };
    // Thread cycles per millisecond on this machine, for the UI-thread CPU
    // time of the `busy.*` spans (Windows; 3 × 4 ms once per run).
    calibrate_cycles(Duration::from_millis(4));
    let task = window.spawn(cx, async move |cx| {
        let (report, quit_code) = run(&lines, &scenario_name, &args, cx).await;
        let out_dir = args.out_dir.clone();
        cx.update(|_, cx| finish(&report, &out_dir, quit_code, cx))
            .ok();
    });
    cx.set_global(QaRunner { _task: task });
}

/// Write the report and quit.
fn finish(report: &Report, out_dir: &Path, quit_code: i32, cx: &mut App) {
    if let Err(error) = report.write(out_dir) {
        services(cx).write_stderr(&format!(
            "QA: could not write the report to {}: {error}\n",
            out_dir.display()
        ));
    }
    let code = if report.exit_code() != 0 {
        report.exit_code()
    } else {
        quit_code
    };
    exit::quit_with_code(code, cx);
}

async fn sleep(cx: &mut AsyncWindowContext, duration: Duration) {
    cx.background_executor().timer(duration).await;
}

/// Run every step; returns the report and the exit code a `quit` asked for.
async fn run(
    lines: &[Line],
    scenario: &str,
    args: &QaArgs,
    cx: &mut AsyncWindowContext,
) -> (Report, i32) {
    let mut report = Report {
        scenario: scenario.to_string(),
        steps: Vec::new(),
    };
    let mut quit_code = 0;
    for line in lines {
        let (status, detail) = match &line.step {
            Step::Quit(code) => {
                quit_code = *code;
                report.steps.push(StepResult {
                    line: line.number,
                    step: line.text.clone(),
                    status: Status::Pass,
                    detail: String::new(),
                });
                break;
            }
            step => execute(step, args, cx).await,
        };
        report.steps.push(StepResult {
            line: line.number,
            step: line.text.clone(),
            status,
            detail,
        });
    }
    (report, quit_code)
}

fn pass() -> (Status, String) {
    (Status::Pass, String::new())
}

fn fail(detail: impl Into<String>) -> (Status, String) {
    (Status::Fail, detail.into())
}

/// Read a probe (one poll).
fn read_probe(cx: &mut AsyncWindowContext, key: &str) -> Option<String> {
    cx.update(|window, cx| probe::read(key, window, cx))
        .ok()
        .flatten()
}

async fn execute(step: &Step, args: &QaArgs, cx: &mut AsyncWindowContext) -> (Status, String) {
    match step {
        Step::WaitLoaded(timeout) => {
            let deadline = Instant::now() + *timeout;
            loop {
                let loaded = read_probe(cx, "list.loaded");
                let loading = read_probe(cx, "list.loading");
                match (loaded.as_deref(), loading.as_deref()) {
                    (Some("true"), Some("false")) => return pass(),
                    (None, _) | (_, None) => return fail("list probes not available"),
                    _ => {}
                }
                if Instant::now() >= deadline {
                    return fail(format!("the list was not loaded within {timeout:?}"));
                }
                sleep(cx, POLL).await;
            }
        }
        Step::Type(text) => {
            for c in text.chars() {
                let keystroke = keystroke_for_char(c);
                cx.update(|window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                })
                .ok();
            }
            sleep(cx, SETTLE).await;
            pass()
        }
        Step::Paste(text) => {
            let text = text.clone();
            let (status, detail) = with_recording(cx, move |recording| {
                recording.set_clipboard(Some(text));
            });
            if status != Status::Pass {
                return (status, detail);
            }
            let keystroke = match Keystroke::parse("secondary-v") {
                Ok(keystroke) => keystroke,
                Err(error) => return fail(format!("bad paste keystroke: {error}")),
            };
            // One outermost update: the paste, the change event and every
            // subscriber it reaches (`busy.paste`, X-038).
            let timer = BusyTimer::start();
            let dispatched = cx
                .update(|window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                })
                .is_ok();
            let measured = timer.stop();
            if !dispatched {
                return fail("the window is gone");
            }
            cx.update(|_, cx| UiBusy::record(cx, BusySpan::Paste, measured))
                .ok();
            // The parts that ran inside it (diagnostics in the report).
            let parts = cx
                .update(
                    |_, cx| match (cx.try_global::<SubSpans>(), measured.window) {
                        (Some(subs), Some(window)) => subs.within(window).join(", "),
                        _ => String::new(),
                    },
                )
                .unwrap_or_default();
            sleep(cx, SETTLE).await;
            (
                Status::Pass,
                format!("measured {} [{parts}]", describe(measured)),
            )
        }
        Step::Idle(duration) => idle(cx, *duration).await,
        Step::ClearToasts => {
            use gpui_kit::component::WindowExt as _;
            cx.update(|window, cx| window.clear_notifications(cx)).ok();
            // The exit transition (gpui-kit: 200 ms) ends before the next step.
            sleep(cx, Duration::from_millis(400)).await;
            pass()
        }
        Step::MarkMemory(name) => match probe::working_set() {
            Some(memory) => {
                let name = name.clone();
                let bytes = memory.current;
                cx.update(|_, cx| probe::mark_memory(cx, name, bytes)).ok();
                (
                    Status::Pass,
                    format!("measured {:.1} MB", bytes as f64 / MIB),
                )
            }
            None => (
                Status::Skip,
                "the working set is not available on this platform".to_string(),
            ),
        },
        Step::Key(keys) => {
            for key in keys {
                match Keystroke::parse(key) {
                    Ok(keystroke) => {
                        cx.update(|window, cx| {
                            window.dispatch_keystroke(keystroke, cx);
                        })
                        .ok();
                        sleep(cx, SETTLE).await;
                    }
                    Err(error) => return fail(format!("bad keystroke '{key}': {error}")),
                }
            }
            pass()
        }
        Step::Hold {
            key,
            count,
            interval,
        } => {
            let keystroke = match Keystroke::parse(key) {
                Ok(keystroke) => keystroke,
                Err(error) => return fail(format!("bad keystroke '{key}': {error}")),
            };
            for _ in 0..*count {
                cx.update(|window, cx| {
                    window.dispatch_keystroke(keystroke.clone(), cx);
                })
                .ok();
                sleep(cx, *interval).await;
            }
            pass()
        }
        Step::Action(name) => {
            let result = cx.update(|window, cx| match cx.build_action(name, None) {
                Ok(action) => {
                    window.dispatch_action(action, cx);
                    Ok(())
                }
                Err(error) => Err(format!("unknown action '{name}': {error}")),
            });
            sleep(cx, SETTLE).await;
            match result {
                Ok(Ok(())) => pass(),
                Ok(Err(message)) => fail(message),
                Err(_) => fail("the window is gone"),
            }
        }
        Step::Click { id, kind } => click(cx, id, *kind).await,
        Step::AnswerSave(answer) => {
            let answer = Ok(answer.clone());
            with_recording(cx, move |recording| recording.answer_save(answer))
        }
        Step::AnswerOpen(answer) => {
            let answer = Ok(answer.clone());
            with_recording(cx, move |recording| recording.answer_open(answer))
        }
        Step::SetStdout(kind) => {
            let kind = *kind;
            with_recording(cx, move |recording| recording.set_stdout_kind(kind))
        }
        Step::Wait(duration) => {
            sleep(cx, *duration).await;
            pass()
        }
        Step::WaitFor {
            probe: key,
            cmp,
            value,
            timeout,
        } => {
            let deadline = Instant::now() + *timeout;
            loop {
                let actual = read_probe(cx, key);
                let (status, detail) = judge(actual.as_deref(), *cmp, value);
                if status == Status::Pass {
                    return (status, detail);
                }
                if Instant::now() >= deadline {
                    return (status, format!("after {timeout:?}: {detail}"));
                }
                sleep(cx, POLL).await;
            }
        }
        Step::Expect {
            probe: key,
            cmp,
            value,
        } => {
            let deadline = Instant::now() + EXPECT_GRACE;
            loop {
                let actual = read_probe(cx, key);
                let (status, detail) = judge(actual.as_deref(), *cmp, value);
                if status == Status::Pass || Instant::now() >= deadline {
                    return (status, detail);
                }
                sleep(cx, POLL).await;
            }
        }
        Step::Timing {
            probe: key,
            cmp,
            value,
        } => match read_probe(cx, key) {
            None => (Status::Skip, "probe not available".to_string()),
            // A passing timing step keeps its measurement (§15.8 evidence).
            Some(actual) => match judge(Some(&actual), *cmp, value) {
                (Status::Pass, _) => (Status::Pass, format!("measured {actual}")),
                failed => failed,
            },
        },
        Step::Shot(name) => shot(cx, name, &args.out_dir).await,
        Step::SetTheme(pref) => {
            let pref = *pref;
            cx.update(|window, cx| theme_tokens::apply_theme(pref, window, cx))
                .ok();
            sleep(cx, SETTLE * 3).await;
            pass()
        }
        Step::Resize { width, height } => {
            let (width, height) = (*width, *height);
            cx.update(|window, _| window.resize(size(px(width), px(height))))
                .ok();
            sleep(cx, SETTLE * 3).await;
            pass()
        }
        Step::Quit(_) => pass(),
    }
}

/// Bytes per MiB (working-set reports).
const MIB: f64 = 1024.0 * 1024.0;

/// `<wall> ms (<cpu> ms UI-thread CPU)`.
fn describe(measured: BusyMeasure) -> String {
    let wall = measured.wall.as_secs_f64() * 1000.0;
    match measured.cycles.and_then(cycles_to_ms) {
        Some(cpu) => format!("{wall:.3} ms wall, {cpu:.3} ms UI-thread CPU"),
        None => format!("{wall:.3} ms wall"),
    }
}

/// A probe read as a number (`None`: missing or not a number).
fn probe_number(cx: &mut AsyncWindowContext, key: &str) -> Option<f64> {
    read_probe(cx, key).and_then(|value| dsl::number(&value))
}

/// The list's ticker period (`list::TICK_INTERVAL`).
const TICKER: Duration = Duration::from_secs(30);

/// Attempts of the `idle` step when input from outside the run (the OS
/// activating another window, the mouse crossing this one) lands in the window.
const IDLE_ATTEMPTS: usize = 3;

/// The `idle` step (X-007): wait without input and record what the window did
/// meanwhile — frames rendered (those the list's 30 s ticker caused are
/// counted apart) and the UI time those frames took. A window shorter than
/// the ticker period starts right after a ticker run, so it holds none (the
/// ticker repaints only when a painted time label changed; the frames of the
/// wait for that run — earlier work settling included — are reported as
/// `idle.ticker_frames` / `idle.ticker_frame_ms`).
///
/// A window that received input from outside (an activation change, mouse
/// events: the desktop the run shares) is not idle; it is reported and measured
/// again, up to [`IDLE_ATTEMPTS`] times. The last attempt counts either way.
async fn idle(cx: &mut AsyncWindowContext, duration: Duration) -> (Status, String) {
    let mut disturbed = Vec::new();
    for attempt in 1..=IDLE_ATTEMPTS {
        let (report, detail, outside) = match idle_once(cx, duration).await {
            Ok(measured) => measured,
            Err(failed) => return failed,
        };
        if outside && attempt < IDLE_ATTEMPTS {
            disturbed.push(format!("attempt {attempt}: {detail}"));
            continue;
        }
        let earlier = if disturbed.is_empty() {
            String::new()
        } else {
            format!(
                " [measured again: input from outside in {}]",
                disturbed.join("; ")
            )
        };
        cx.update(|_, cx| cx.set_global(report)).ok();
        return (Status::Pass, format!("{detail}{earlier}"));
    }
    unreachable!("the last attempt returns")
}

/// One idle window: the report, its description, and whether input from
/// outside landed in it.
async fn idle_once(
    cx: &mut AsyncWindowContext,
    duration: Duration,
) -> Result<(probe::IdleReport, String, bool), (Status, String)> {
    let mut ticker = None;
    if duration < TICKER
        && let (Some(ticks), Some(frames), Some(ms)) = (
            probe_number(cx, "list.ticks"),
            probe_number(cx, "window.frames"),
            probe_number(cx, "window.frame_ms_total"),
        )
    {
        let deadline = Instant::now() + TICKER + Duration::from_secs(2);
        while probe_number(cx, "list.ticks").is_some_and(|now| now <= ticks)
            && Instant::now() < deadline
        {
            sleep(cx, POLL).await;
        }
        // The run's repaint (if any) lands within a frame or two.
        sleep(cx, Duration::from_millis(300)).await;
        if let (Some(frames_after), Some(ms_after)) = (
            probe_number(cx, "window.frames"),
            probe_number(cx, "window.frame_ms_total"),
        ) {
            ticker = Some((
                (frames_after - frames).max(0.0) as u64,
                (ms_after - ms).max(0.0),
            ));
        }
    }
    let before = (
        probe_number(cx, "window.frames"),
        probe_number(cx, "window.frame_ms_total"),
        probe_number(cx, "list.tick_repaints").unwrap_or(0.0),
    );
    let (Some(frames_before), Some(ms_before), ticks_before) = before else {
        return Err(fail("the window has no frame probes (not the main window)"));
    };
    let cpu_before = probe_number(cx, "window.frame_cpu_ms_total");
    let process_before = probe::process_cpu_ms();
    let outside_before = outside_counts(cx);
    let rebuilds_before = probe_number(cx, "menus.rebuilds");
    sleep(cx, duration).await;
    let (Some(frames_after), Some(ms_after)) = (
        probe_number(cx, "window.frames"),
        probe_number(cx, "window.frame_ms_total"),
    ) else {
        return Err(fail("the frame probes went away"));
    };
    let ticks_after = probe_number(cx, "list.tick_repaints").unwrap_or(ticks_before);
    let cpu_after = probe_number(cx, "window.frame_cpu_ms_total");
    let process_after = probe::process_cpu_ms();
    let ticks = (ticks_after - ticks_before).max(0.0) as u64;
    let frames_total = (frames_after - frames_before).max(0.0) as u64;
    let mut notes = Vec::new();
    // Input from outside the run meanwhile.
    let mut outside = false;
    if let (Some((a0, i0)), Some((a1, i1))) = (outside_before, outside_counts(cx))
        && (a1 > a0 || i1 > i0)
    {
        outside = true;
        notes.push(format!(
            "{} activation change(s), {} mouse event(s) from outside",
            a1 - a0,
            i1 - i0
        ));
    }
    if let (Some(before), Some(after)) = (rebuilds_before, probe_number(cx, "menus.rebuilds"))
        && after > before
    {
        notes.push(format!("{} menu rebuild(s)", after - before));
    }
    // When the last frame came (a toast's timer, a late task...).
    if frames_total > 0
        && let Some(age) = probe_number(cx, "window.frame_age_ms")
    {
        notes.push(format!(
            "the last frame {:.1} s into the window ({:.3} ms)",
            (duration.as_secs_f64() - age / 1000.0).max(0.0),
            probe_number(cx, "window.frame_last_ms").unwrap_or_default()
        ));
    }
    let report = probe::IdleReport {
        frames_total,
        ticks,
        // Each ticker run repaints once.
        frames: frames_total.saturating_sub(ticks),
        busy_ms: (ms_after - ms_before).max(0.0),
        busy_cpu_ms: match (cpu_before, cpu_after) {
            (Some(before), Some(after)) => Some((after - before).max(0.0)),
            _ => None,
        },
        process_cpu_ms: match (process_before, process_after) {
            (Some(before), Some(after)) => Some((after - before).max(0.0)),
            _ => None,
        },
        seconds: duration.as_secs_f64(),
        ticker_frames: ticker.map(|(frames, _)| frames),
        ticker_frame_ms: ticker.map(|(_, ms)| ms),
    };
    let aligned = match ticker {
        Some((frames, ms)) => {
            format!("after waiting for a ticker run ({frames} frame(s), {ms:.3} ms meanwhile), ")
        }
        None => String::new(),
    };
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!("; {}", notes.join(", "))
    };
    let detail = format!(
        "measured {aligned}{} frames ({} from the ticker), {:.3} ms UI wall{} over {:?}{notes}",
        report.frames_total,
        report.ticks,
        report.busy_ms,
        report
            .busy_cpu_ms
            .map(|cpu| format!(", {cpu:.3} ms UI-thread CPU"))
            .unwrap_or_default(),
        duration
    );
    Ok((report, detail, outside))
}

/// Activation changes and mouse events the main window received so far.
fn outside_counts(cx: &mut AsyncWindowContext) -> Option<(f64, f64)> {
    Some((
        probe_number(cx, "window.activations")?,
        probe_number(cx, "window.input_events")?,
    ))
}

/// Run `f` on the recording services (QA runs always record).
fn with_recording(
    cx: &mut AsyncWindowContext,
    f: impl FnOnce(&crate::services::RecordingServices),
) -> (Status, String) {
    let recording = cx.update(|_, cx| services(cx).recording).ok().flatten();
    match recording {
        Some(recording) => {
            f(&recording);
            pass()
        }
        None => fail("the services are not recording (not a QA run)"),
    }
}

async fn click(cx: &mut AsyncWindowContext, id: &str, kind: ClickKind) -> (Status, String) {
    // Repaint so the registry holds this frame's bounds.
    cx.update(|window, _| window.refresh()).ok();
    sleep(cx, POLL * 3).await;
    let Some(bounds) = registry::lookup(id, BOUNDS_MAX_AGE) else {
        let known = registry::ids(BOUNDS_MAX_AGE);
        return fail(format!(
            "no element with qa id '{id}' on screen (known: {})",
            known.join(", ")
        ));
    };
    let position = bounds.center();
    cx.update(|window, cx| dispatch_click(window, cx, position, kind))
        .ok();
    sleep(cx, SETTLE * 2).await;
    pass()
}

/// Press the mouse at `position` inside this process (`Window::dispatch_event`:
/// a hover move, then down/up; twice for a double click). Never an OS event.
pub fn dispatch_click(
    window: &mut Window,
    cx: &mut App,
    position: gpui_kit::Point<gpui_kit::Pixels>,
    kind: ClickKind,
) {
    let button = match kind {
        ClickKind::Right => MouseButton::Right,
        ClickKind::Single | ClickKind::Double => MouseButton::Left,
    };
    let presses = if kind == ClickKind::Double { 2 } else { 1 };
    let modifiers = Modifiers::none();
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        }),
        cx,
    );
    for click_count in 1..=presses {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button,
                position,
                modifiers,
                click_count,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button,
                position,
                modifiers,
                click_count,
            }),
            cx,
        );
    }
}
/// The screenshot handshake (§15.3): `NAME.request` out, `NAME.png` in.
async fn shot(cx: &mut AsyncWindowContext, name: &str, out_dir: &Path) -> (Status, String) {
    cx.update(|window, _| window.refresh()).ok();
    sleep(cx, POLL * 5).await;
    let title = cx
        .update(|window, _| probe::window_title(window))
        .unwrap_or_default();
    let png: PathBuf = out_dir.join(format!("{name}.png"));
    // A capture left by an earlier run into the same folder must not count
    // as this step's screenshot.
    if png.is_file() && std::fs::remove_file(&png).is_err() {
        return (
            Status::Skip,
            format!("could not replace the old {name}.png"),
        );
    }
    let request = serde_json::json!({
        "name": name,
        "pid": std::process::id(),
        "title": title,
    });
    if let Err(error) = std::fs::create_dir_all(out_dir).and_then(|()| {
        std::fs::write(
            out_dir.join(format!("{name}.request")),
            serde_json::to_string(&request).unwrap_or_default(),
        )
    }) {
        return (
            Status::Skip,
            format!("could not write the request: {error}"),
        );
    }
    let deadline = Instant::now() + SHOT_TIMEOUT;
    while Instant::now() < deadline {
        if png.is_file() {
            return pass();
        }
        sleep(cx, Duration::from_millis(100)).await;
    }
    (
        Status::Skip,
        format!("no screenshot within {SHOT_TIMEOUT:?} (start tools/qa/qa-shot-watch.ps1)"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            scenario: "smoke.qa".to_string(),
            steps: vec![
                StepResult {
                    line: 3,
                    step: "wait-loaded 15s".into(),
                    status: Status::Pass,
                    detail: String::new(),
                },
                StepResult {
                    line: 4,
                    step: "expect focus == list".into(),
                    status: Status::Fail,
                    detail: "got 'viewer', wanted == 'list'".into(),
                },
                StepResult {
                    line: 5,
                    step: "shot a".into(),
                    status: Status::Skip,
                    detail: "no screenshot".into(),
                },
            ],
        }
    }

    #[test]
    fn report_text_has_one_line_per_step_and_a_summary() {
        let text = report().to_text();
        assert_eq!(
            text,
            "PASS 3 wait-loaded 15s\n\
             FAIL 4 expect focus == list -- got 'viewer', wanted == 'list'\n\
             SKIP 5 shot a -- no screenshot\n\
             # smoke.qa: 1 pass, 1 fail, 1 skip\n"
        );
    }

    #[test]
    fn report_json_carries_the_counts_and_steps() {
        let value: serde_json::Value = serde_json::from_str(&report().to_json()).unwrap();
        assert_eq!(value["scenario"], "smoke.qa");
        assert_eq!(value["pass"], 1);
        assert_eq!(value["fail"], 1);
        assert_eq!(value["skip"], 1);
        assert_eq!(value["exit_code"], 1);
        assert_eq!(value["steps"][1]["status"], "FAIL");
        assert_eq!(value["steps"][1]["line"], 4);
    }

    #[test]
    fn exit_code_is_one_only_with_a_failed_step() {
        let mut report = report();
        assert_eq!(report.exit_code(), 1);
        report.steps.retain(|step| step.status != Status::Fail);
        assert_eq!(report.exit_code(), 0, "skips do not fail a run");
    }

    #[test]
    fn report_files_are_written() {
        let dir = tempfile::tempdir().unwrap();
        report().write(&dir.path().join("out")).unwrap();
        assert!(dir.path().join("out/report.txt").is_file());
        assert!(dir.path().join("out/report.json").is_file());
    }

    #[test]
    fn judge_reports_what_was_seen() {
        assert_eq!(
            judge(Some("list"), Cmp::Eq, "list"),
            (Status::Pass, String::new())
        );
        let (status, detail) = judge(Some("viewer"), Cmp::Eq, "list");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("got 'viewer'"), "{detail}");
        let (status, detail) = judge(None, Cmp::Eq, "list");
        assert_eq!(
            (status, detail.as_str()),
            (Status::Fail, "probe not available")
        );
    }

    #[test]
    fn typed_characters_become_keystrokes_with_their_text() {
        let a = keystroke_for_char('a');
        assert_eq!((a.key.as_str(), a.key_char.as_deref()), ("a", Some("a")));
        assert!(!a.modifiers.shift);
        let upper = keystroke_for_char('Q');
        assert_eq!(
            (upper.key.as_str(), upper.key_char.as_deref()),
            ("q", Some("Q"))
        );
        assert!(upper.modifiers.shift);
        let space = keystroke_for_char(' ');
        assert_eq!(
            (space.key.as_str(), space.key_char.as_deref()),
            ("space", Some(" "))
        );
        let dash = keystroke_for_char('-');
        assert_eq!(
            (dash.key.as_str(), dash.key_char.as_deref()),
            ("-", Some("-"))
        );
    }
}

/// X-027: the click path (registry bounds → in-process mouse events) reaches a
/// real element, and the registry forgets stale ids.
#[cfg(test)]
mod click_tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
        StatefulInteractiveElement as _, Styled as _, TestAppContext, Window, div, px,
    };

    use super::dispatch_click;
    use crate::qa::dsl::ClickKind;
    use crate::qa_ext::{QaExt as _, registry};

    #[derive(Default)]
    struct Counts {
        single: Cell<usize>,
        double: Cell<usize>,
        right: Cell<usize>,
    }

    struct Target {
        counts: Rc<Counts>,
    }

    impl Render for Target {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let counts = self.counts.clone();
            let right = self.counts.clone();
            div().size_full().child(
                div()
                    .id("qa-click-target")
                    .w(px(120.0))
                    .h(px(40.0))
                    .mt(px(30.0))
                    .ml(px(50.0))
                    .qa_id("test:target-1")
                    .on_click(move |event, _, _| {
                        if event.click_count() >= 2 {
                            counts.double.set(counts.double.get() + 1);
                        } else {
                            counts.single.set(counts.single.get() + 1);
                        }
                    })
                    .on_mouse_down(gpui_kit::MouseButton::Right, move |_, _, _| {
                        right.right.set(right.right.get() + 1);
                    }),
            )
        }
    }

    #[gpui_kit::test]
    fn a_registered_element_is_found_and_clicked(cx: &mut TestAppContext) {
        let counts = Rc::new(Counts::default());
        let window = cx.add_window({
            let counts = counts.clone();
            move |_, _| Target { counts }
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let bounds = registry::lookup("test:target-1", Duration::from_secs(30))
            .expect("the element registered its bounds");
        assert_eq!(f32::from(bounds.size.width), 120.0);
        assert_eq!(f32::from(bounds.size.height), 40.0);
        assert_eq!(f32::from(bounds.origin.x), 50.0);
        assert_eq!(f32::from(bounds.origin.y), 30.0);
        assert!(registry::ids(Duration::from_secs(30)).contains(&"test:target-1".to_string()));

        let center = bounds.center();
        cx.update_window(handle, |_, window, cx| {
            dispatch_click(window, cx, center, ClickKind::Single);
        })
        .unwrap();
        assert_eq!(counts.single.get(), 1, "a left click");
        cx.update_window(handle, |_, window, cx| {
            dispatch_click(window, cx, center, ClickKind::Right);
        })
        .unwrap();
        assert_eq!(counts.right.get(), 1, "a right click");
        cx.update_window(handle, |_, window, cx| {
            dispatch_click(window, cx, center, ClickKind::Double);
        })
        .unwrap();
        assert_eq!(counts.double.get(), 1, "the second press of a double click");
        // A click outside the element does nothing.
        cx.update_window(handle, |_, window, cx| {
            dispatch_click(
                window,
                cx,
                gpui_kit::point(px(400.0), px(400.0)),
                ClickKind::Single,
            );
        })
        .unwrap();
        assert_eq!(
            counts.single.get(),
            2,
            "only the double click's first press counted"
        );
        // A stale registration is not returned.
        assert!(registry::lookup("test:target-1", Duration::ZERO).is_none());
        assert!(registry::lookup("test:nothing", Duration::from_secs(30)).is_none());
    }
}
