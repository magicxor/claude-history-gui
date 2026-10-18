//! Delete a conversation (DESIGN §5.7, §11.9, K4). WP3.
//!
//! Plans with core `maintenance::plan_session_delete` (off the UI thread),
//! confirms with `widgets::open_destructive_confirm` + `widgets::PathList`
//! (Enter ignored, `y`/`Y` confirm, `n`/`N`/Esc cancel), executes with
//! `execute_session_delete` and broadcasts `CorpusNotice::Deleted`. The
//! source comes from the captured target, never from the live selection
//! (A2), and the plan lists every file the execution removes.

use std::cell::RefCell;
use std::rc::Rc;

use claude_history_core::history::Source;
use claude_history_core::maintenance::{
    DeletePlan, delete_by_uuid_message, execute_session_delete, plan_session_delete,
};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{App, IntoElement as _, ParentElement as _, Styled as _, Window, div};

use crate::globals::{
    BusySpan, BusyTimer, CorpusBus, CorpusNotice, OpenTargets, UiBusy, app_state,
};
use crate::model::{ConversationTarget, short_session_id};
use crate::notify::Toast;
use crate::ops::copy::{can_act, clear_dialog, live_target};
use crate::widgets::{
    ConfirmControl, ConfirmOptions, ConfirmOutcome, PathItem, PathList, open_destructive_confirm,
};
use crate::workspace::startup::register_dialog;

/// The QA / probe id of the dialog.
pub const DIALOG_ID: &str = "delete";

/// `Claude Code`, `Pi` or `OMP`.
pub fn source_label(source: Source) -> &'static str {
    match source {
        Source::Claude => "Claude Code",
        Source::Pi => "Pi",
        Source::Omp => "OMP",
    }
}

/// The confirmation title: it names the captured target (PD15).
pub fn dialog_title(target: &ConversationTarget) -> String {
    format!("Delete \"{}\"?", target.title)
}

/// `N copies across projects. This can't be undone.` / `This can't be undone.`
pub fn consequence_text(plan: &DeletePlan) -> String {
    if plan.source == Source::Claude && plan.files.len() > 1 {
        format!(
            "{} copies across projects. This can't be undone.",
            plan.files.len()
        )
    } else {
        "This can't be undone.".to_string()
    }
}

/// The rows of the path list: every transcript, then every folder.
pub fn plan_items(plan: &DeletePlan) -> Vec<PathItem> {
    plan.files
        .iter()
        .map(PathItem::file)
        .chain(plan.dirs.iter().map(PathItem::folder))
        .collect()
}

/// Plan and confirm the deletion of the captured `target`.
pub fn open_delete_dialog(target: ConversationTarget, window: &mut Window, cx: &mut App) {
    if !can_act(&target) {
        return;
    }
    let Some(source) = target.source else {
        return;
    };
    let env = app_state(cx).env_with_workspace();
    let path = target.path.clone();
    let task = cx
        .background_executor()
        .spawn(async move { plan_session_delete(&env, &path, source) });
    window
        .spawn(&*cx, async move |cx| {
            let result = task.await;
            cx.update(|window, cx| match result {
                Ok(plan) => {
                    // The captured title may predate a rename (this runs outside
                    // the list's update, so the live entry can be read).
                    let target = live_target(target, cx);
                    open_confirmation(target, plan, window, cx)
                }
                Err(error) => Toast::error(error.to_string()).show(window, cx),
            })
            .ok();
        })
        .detach();
}

/// The confirmation of §5.7 for an already computed `plan`.
fn open_confirmation(
    target: ConversationTarget,
    plan: DeletePlan,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    let slot: Rc<RefCell<Option<ConfirmControl>>> = Rc::new(RefCell::new(None));
    let ids: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));

    // Every copy the plan removes counts as "held" by this dialog: a deletion
    // of one of them (any route) closes it (PD15, §7.2).
    let mut paths = vec![target.path.clone()];
    for file in &plan.files {
        if !paths.iter().any(|p| crate::globals::same_path(p, file)) {
            paths.push(file.clone());
        }
    }
    for path in paths {
        let slot = slot.clone();
        let id = OpenTargets::register(
            cx,
            handle,
            path,
            target.title.clone(),
            Box::new(move |_, cx| {
                let control = slot.borrow().clone();
                if let Some(control) = control {
                    control.close(cx);
                }
            }),
        );
        ids.borrow_mut().push(id);
    }
    register_dialog(cx, DIALOG_ID);

    let heading = {
        let mut parts = vec![format!("{} conversation", source_label_of(&target))];
        if let Some(project) = &target.project_name {
            parts.push(project.to_string());
        }
        parts.push(short_session_id(&plan.session_id));
        parts.join(" · ")
    };
    let items = plan_items(&plan);
    let note = consequence_text(&plan);
    let options = ConfirmOptions::destructive(dialog_title(&target), "Delete")
        .width_rem(38.0)
        .body(move |_, cx| {
            let theme = cx.theme();
            let (danger, muted) = (theme.danger, theme.muted_foreground);
            v_flex()
                .gap_2()
                .w_full()
                .child(
                    h_flex()
                        .gap_1()
                        .text_sm()
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .small()
                                .text_color(danger),
                        )
                        .child(heading.clone()),
                )
                .child(div().text_sm().child("These will be removed from disk:"))
                .child(PathList::new("delete-paths").items(items.clone()))
                .child(div().text_sm().text_color(muted).child(note.clone()))
                .into_any_element()
        })
        .on_closed({
            let ids = ids.clone();
            move |cx| {
                for id in ids.borrow_mut().drain(..) {
                    OpenTargets::unregister(cx, id);
                }
                clear_dialog(cx, DIALOG_ID);
            }
        });

    let control = {
        let plan = plan.clone();
        let target = target.clone();
        open_destructive_confirm(options, window, cx, move |control, window, cx| {
            execute(control, &target, &plan, window, cx);
            ConfirmOutcome::Busy
        })
    };
    *slot.borrow_mut() = Some(control);
}

fn source_label_of(target: &ConversationTarget) -> &'static str {
    target.source.map(source_label).unwrap_or("Unknown")
}

/// Run the deletion in the background and finish in the dialog's window.
fn execute(
    control: &ConfirmControl,
    target: &ConversationTarget,
    plan: &DeletePlan,
    window: &mut Window,
    cx: &mut App,
) {
    let env = app_state(cx).env_with_workspace();
    let task = {
        let plan = plan.clone();
        cx.background_executor().spawn(async move {
            // A failure carries the partial report: the transcripts core
            // already removed must still leave the list.
            match execute_session_delete(&env, &plan) {
                Ok(_) => (Ok(()), plan.files.clone()),
                Err(failure) => {
                    let mut gone = failure.partial.removed_files;
                    // A transcript that vanished meanwhile is gone as well.
                    gone.extend(
                        failure
                            .partial
                            .missing
                            .into_iter()
                            .filter(|path| plan.files.contains(path)),
                    );
                    (Err(failure.error), gone)
                }
            }
        })
    };
    let control = control.clone();
    let plan = plan.clone();
    let title = target.title.clone();
    window
        .spawn(&*cx, async move |cx| {
            let (result, gone) = task.await;
            // The removal is timed around the whole update: the broadcast and
            // every pane, dialog and menu it reaches (X-038, `UiBusy`).
            let timer = BusyTimer::start();
            let removed = result.is_ok();
            cx.update(|window, cx| {
                // Close first: it forgets this dialog's `OpenTargets` entries,
                // so the broadcast below does not report it as closed by a
                // deletion.
                crate::globals::sub_span(cx, "delete.close_dialog", |cx| control.close(cx));
                match result {
                    Ok(()) => {
                        CorpusBus::broadcast(CorpusNotice::Deleted(gone), cx);
                        let message = if plan.files.len() > 1 {
                            delete_by_uuid_message(&plan.session_id, plan.files.len())
                        } else {
                            format!("Deleted \"{title}\"")
                        };
                        Toast::success(message).show(window, cx);
                    }
                    // Core already appended the debug.log line (LIST quirk 7 fix).
                    Err(error) => {
                        if !gone.is_empty() {
                            CorpusBus::broadcast(CorpusNotice::Deleted(gone), cx);
                        }
                        Toast::error(format!(
                            "Failed to delete session {}: {error}",
                            plan.path.display()
                        ))
                        .show(window, cx);
                    }
                }
            })
            .ok();
            if removed {
                let measured = timer.stop();
                cx.update(|_, cx| UiBusy::record(cx, BusySpan::DeleteRemoval, measured))
                    .ok();
            }
        })
        .detach();
}
