//! QA probes (DESIGN §15.3): read-only accessors by key (`list.count`,
//! `viewer.items`, `focus`, `toast.last`, …). WP7 (feature `qa`).
//!
//! The app-level keys live here (`clipboard`, `stdout`, `stderr`,
//! `calls.*`, `dialog`, `theme`, `toast.last`, `window.*`); the pane keys
//! (`list.*`, `viewer.*`, `deep.*`, `agent.*`, `focus`) are answered by the
//! main window's `Workspace::qa_probe`, which delegates to each pane's own
//! `qa_probe`. A key nobody answers reads `None`; `expect` then fails and
//! `timing` skips with "probe not available".

use std::collections::BTreeMap;

use gpui_kit::base::Root;
use gpui_kit::{App, Global, Window};

use crate::globals::{BusySpan, UiBusy, last_toast};
use crate::services::{ServiceCall, services};
use crate::single_file::SingleFileWindow;
use crate::theme_tokens;
use crate::workspace::Workspace;
use crate::workspace::startup::last_dialog;

/// What the last `idle` step saw (X-007; probes `idle.*`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IdleReport {
    /// Frames rendered in the window, the ticker's excluded.
    pub frames: u64,
    /// Every frame rendered meanwhile.
    pub frames_total: u64,
    /// Runs of the list's 30 s ticker meanwhile.
    pub ticks: u64,
    /// UI-thread time of those frames (render, layout, paint), milliseconds:
    /// wall clock, and the thread's own CPU time where measured.
    pub busy_ms: f64,
    pub busy_cpu_ms: Option<f64>,
    /// CPU time of the whole process (every thread) over the window: busy
    /// polling shows here even when no frame is drawn (X-007).
    pub process_cpu_ms: Option<f64>,
    pub seconds: f64,
    /// The wait for the ticker run the window was aligned after (up to 300 ms
    /// past the run; earlier work settling shows here too): its frames and their UI
    /// time (wall, milliseconds); `None` when not aligned.
    pub ticker_frames: Option<u64>,
    pub ticker_frame_ms: Option<f64>,
}

impl Global for IdleReport {}

/// Working sets remembered by `mark-memory` (bytes, by name).
#[derive(Clone, Debug, Default)]
pub struct MemoryMarks(pub BTreeMap<String, u64>);

impl Global for MemoryMarks {}

/// Remember the working set `bytes` as `name`.
pub fn mark_memory(cx: &mut App, name: String, bytes: u64) {
    cx.default_global::<MemoryMarks>().0.insert(name, bytes);
}

/// The process's memory (bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkingSet {
    pub current: u64,
    pub peak: u64,
}

/// This process's working set (Windows: `K32GetProcessMemoryInfo`; Linux:
/// `VmRSS`/`VmHWM`; `None` elsewhere). Read-only, like Task Manager.
#[cfg(windows)]
pub fn working_set() -> Option<WorkingSet> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// `PROCESS_MEMORY_COUNTERS` (psapi.h).
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn K32GetProcessMemoryInfo(
            process: HANDLE,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        ..ProcessMemoryCounters::default()
    };
    // SAFETY: the pseudo handle of this process and a correctly sized struct.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (ok != 0).then_some(WorkingSet {
        current: counters.working_set_size as u64,
        peak: counters.peak_working_set_size as u64,
    })
}

/// This process's working set (Linux: `/proc/self/status`).
#[cfg(not(windows))]
pub fn working_set() -> Option<WorkingSet> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<u64> {
        let line = status.lines().find(|line| line.starts_with(name))?;
        let kib: u64 = line[name.len()..]
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        Some(kib * 1024)
    };
    Some(WorkingSet {
        current: field("VmRSS:")?,
        peak: field("VmHWM:")?,
    })
}

/// CPU time of this whole process, every thread (user + kernel), in
/// milliseconds: what busy polling costs even when no frame is drawn.
#[cfg(windows)]
pub fn process_cpu_ms() -> Option<f64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the pseudo handle of this process and four valid out structs.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    let ticks = |time: FILETIME| ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
    // 100 ns units.
    (ok != 0).then(|| (ticks(kernel) + ticks(user)) as f64 / 10_000.0)
}

/// CPU time of this whole process (`/proc/self/stat` utime + stime).
#[cfg(not(windows))]
pub fn process_cpu_ms() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // After the `(comm)` field: state is field 3, utime 14, stime 15.
    let rest = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let (utime, stime): (f64, f64) = (fields.get(11)?.parse().ok()?, fields.get(12)?.parse().ok()?);
    // SAFETY: sysconf has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    (hz > 0).then(|| (utime + stime) * 1000.0 / hz as f64)
}

/// Milliseconds with three decimals.
fn ms(duration: std::time::Duration) -> String {
    format!("{:.3}", duration.as_secs_f64() * 1000.0)
}

/// `busy.<span>` (last run, wall clock), `busy.<span>_max` and
/// `busy.<span>_cpu` (the UI thread's own CPU time of the last run; §15.8,
/// X-038).
fn busy_key(key: &str, cx: &App) -> Option<String> {
    let name = key.strip_prefix("busy.")?;
    let (name, field) = if let Some(name) = name.strip_suffix("_max") {
        (name, "max")
    } else if let Some(name) = name.strip_suffix("_parts") {
        (name, "parts")
    } else if let Some(name) = name.strip_suffix("_median") {
        (name, "median")
    } else if let Some(name) = name.strip_suffix("_runs") {
        (name, "runs")
    } else if let Some(name) = name.strip_suffix("_cpu") {
        (name, "cpu")
    } else {
        (name, "last")
    };
    let span = BusySpan::ALL.into_iter().find(|span| span.name() == name)?;
    let record = UiBusy::of(cx, span)?;
    match field {
        "max" => Some(ms(record.max)),
        "median" => Some(ms(UiBusy::median(cx, span)?)),
        "runs" => Some(UiBusy::recent_count(cx, span).to_string()),
        "parts" => {
            let window = record.last_window?;
            Some(
                cx.try_global::<crate::globals::SubSpans>()
                    .map(|parts| parts.within(window).join(", "))
                    .unwrap_or_default(),
            )
        }
        "cpu" => Some(format!(
            "{:.3}",
            crate::globals::cycles_to_ms(record.last_cycles?)?
        )),
        _ => Some(ms(record.last)),
    }
}

/// `idle.*`, `process.*` and `memory.<name>.*`.
fn measurement_key(key: &str, cx: &App) -> Option<String> {
    const MIB: f64 = 1024.0 * 1024.0;
    if let Some(field) = key.strip_prefix("idle.") {
        let report = cx.try_global::<IdleReport>()?;
        return Some(match field {
            "frames" => report.frames.to_string(),
            "frames_total" => report.frames_total.to_string(),
            "ticks" => report.ticks.to_string(),
            "busy_ms" => format!("{:.3}", report.busy_ms),
            "busy_cpu_ms" => format!("{:.3}", report.busy_cpu_ms?),
            "process_cpu_ms" => format!("{:.3}", report.process_cpu_ms?),
            "ticker_frames" => report.ticker_frames?.to_string(),
            "ticker_frame_ms" => format!("{:.3}", report.ticker_frame_ms?),
            _ => return None,
        });
    }
    match key {
        "menus.rebuilds" => {
            return Some(
                cx.try_global::<crate::menus::RebuildStats>()
                    .map(|stats| stats.count)
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        "menus.rebuild_ms_last" => {
            let stats = cx.try_global::<crate::menus::RebuildStats>()?;
            return Some(ms(stats.last.wall));
        }
        "process.cpu_ms" => return Some(format!("{:.3}", process_cpu_ms()?)),
        "process.working_set_mb" => {
            return Some(format!("{:.1}", working_set()?.current as f64 / MIB));
        }
        "process.peak_working_set_mb" => {
            return Some(format!("{:.1}", working_set()?.peak as f64 / MIB));
        }
        _ => {}
    }
    let rest = key.strip_prefix("memory.")?;
    let (name, field) = rest.rsplit_once('.')?;
    let mark = *cx.try_global::<MemoryMarks>()?.0.get(name)?;
    Some(match field {
        "mb" => format!("{:.1}", mark as f64 / MIB),
        // |now - mark| / mark, percent.
        "delta_pct" => {
            let now = working_set()?.current as f64;
            format!(
                "{:.2}",
                (now - mark as f64).abs() / (mark as f64).max(1.0) * 100.0
            )
        }
        // (now - mark) / mark, percent (negative: shrank).
        "change_pct" => {
            let now = working_set()?.current as f64;
            format!(
                "{:.2}",
                (now - mark as f64) / (mark as f64).max(1.0) * 100.0
            )
        }
        _ => return None,
    })
}

/// Every key this module answers itself (docs and the self-test).
pub const APP_KEYS: [&str; 14] = [
    "clipboard",
    "stdout",
    "stderr",
    "calls.terminal.count",
    "calls.terminal.last",
    "calls.open_url.last",
    "calls.reveal.last",
    "calls.open_with_system.last",
    "calls.prompt_save.count",
    "calls.prompt_open.count",
    "dialog",
    "theme",
    "toast.last",
    "window.title",
];

/// Read `key` in `window`. `None` = the key is unknown (or its pane is not
/// available yet).
pub fn read(key: &str, window: &mut Window, cx: &mut App) -> Option<String> {
    if let Some(value) = app_key(key, window, cx) {
        return Some(value);
    }
    if let Some(value) = busy_key(key, cx).or_else(|| measurement_key(key, cx)) {
        return Some(value);
    }
    // The pane keys come from the window's root: the main window's Workspace
    // or a single-file window (WP2's `SingleFileWindow::qa_probe`; integrator,
    // Phase A).
    let root = window.root::<Root>().flatten()?;
    let view = root.read(cx).view().clone();
    let view = match view.downcast::<Workspace>() {
        Ok(workspace) => {
            if key == "window.kind" {
                return Some("main".to_string());
            }
            return workspace.update(cx, |workspace, cx| workspace.qa_probe(key, window, cx));
        }
        Err(view) => view,
    };
    let single = view.downcast::<SingleFileWindow>().ok()?;
    single.update(cx, |single, cx| single.qa_probe(key, window, cx))
}

fn app_key(key: &str, window: &mut Window, cx: &mut App) -> Option<String> {
    let recording = services(cx).recording;
    let calls = || recording.as_ref().map(|recording| recording.calls());
    Some(match key {
        "clipboard" => recording.as_ref()?.clipboard().unwrap_or_default(),
        "stdout" => recording.as_ref()?.stdout(),
        "stderr" => recording.as_ref()?.stderr(),
        "calls.terminal.count" => calls()?
            .iter()
            .filter(|call| matches!(call, ServiceCall::Terminal(_)))
            .count()
            .to_string(),
        "calls.terminal.last" => calls()?
            .iter()
            .rev()
            .find_map(|call| match call {
                ServiceCall::Terminal(command) => Some(command.display()),
                _ => None,
            })
            .unwrap_or_default(),
        "calls.open_url.last" => calls()?
            .iter()
            .rev()
            .find_map(|call| match call {
                ServiceCall::OpenUrl(url) => Some(url.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        "calls.reveal.last" => calls()?
            .iter()
            .rev()
            .find_map(|call| match call {
                ServiceCall::Reveal(path) => Some(path.display().to_string()),
                _ => None,
            })
            .unwrap_or_default(),
        "calls.open_with_system.last" => calls()?
            .iter()
            .rev()
            .find_map(|call| match call {
                ServiceCall::OpenWithSystem(path) => Some(path.display().to_string()),
                _ => None,
            })
            .unwrap_or_default(),
        "calls.prompt_save.count" => calls()?
            .iter()
            .filter(|call| matches!(call, ServiceCall::PromptSave { .. }))
            .count()
            .to_string(),
        "calls.prompt_open.count" => calls()?
            .iter()
            .filter(|call| matches!(call, ServiceCall::PromptOpen(_)))
            .count()
            .to_string(),
        // WP6's ids (none|settings|shortcuts|palette) first; any other open
        // dialog reads as the id its owner registered with
        // `startup::register_dialog` (config errors use their title), else
        // `other` (integrator, Phase A).
        "dialog" => match crate::help::dialog_id(window, cx) {
            "other" => {
                let registered = last_dialog(cx);
                if registered.is_empty() {
                    "other".to_string()
                } else {
                    registered.to_string()
                }
            }
            id => id.to_string(),
        },
        "theme" => theme_tokens::preference(cx).as_str().to_string(),
        "toast.last" => last_toast(cx).to_string(),
        "window.title" => window_title(window),
        _ => return None,
    })
}

/// The window's title as the OS shows it (the screenshot handshake names it).
pub fn window_title(window: &Window) -> String {
    window.window_title()
}
