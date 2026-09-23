//! Live tool and subagent progress: the `task|role|phase|detail` protocol,
//! the inspector-panel job cards, and the status lines they feed.

use crate::view_model::{ConversationEntry, EntryKind, LiveJob, WorkspaceState};

use super::streaming::{set_streaming_status, tool_call_label};

/// The `ToolStarted` transition: one tool call (or nested subagent) began.
pub(super) fn tool_started(
    state: &mut WorkspaceState,
    call_id: String,
    name: String,
    target: String,
) {
    let label = tool_call_label(&name, &target);
    set_streaming_status(state, &format!("Running {label}"));
    if name == "task" {
        upsert_live_job(state, &call_id, "", "starting", false);
    }
    if let Some(conversation) = state.active.as_mut() {
        conversation.entries.push(ConversationEntry {
            event_id: format!("call-{call_id}"),
            kind: EntryKind::ToolCall,
            text: label.into(),
            call_id: Some(call_id),
            thinking: String::new(),
        });
    }
}

/// The `ToolProgress` transition: incremental tool or subagent progress for
/// the live status line.
pub(super) fn tool_progress(
    state: &mut WorkspaceState,
    call_id: String,
    name: String,
    message: String,
) {
    apply_live_job_progress(state, &call_id, &name, &message);
    let running = state.live_jobs.iter().filter(|job| !job.done).count();
    let status = if running > 1 {
        format!("{running} subagents")
    } else if name == "task" || message.starts_with("task|") {
        live_job_status(state, &call_id)
    } else if name.is_empty() {
        message
    } else {
        format!("{name}: {message}")
    };
    set_streaming_status(state, &status);
}

fn parse_task_progress(message: &str) -> Option<(&str, &str, &str)> {
    let mut parts = message.splitn(4, '|');
    if parts.next()? != "task" {
        return None;
    }
    Some((parts.next()?, parts.next()?, parts.next().unwrap_or("")))
}

fn upsert_live_job(state: &mut WorkspaceState, call_id: &str, role: &str, step: &str, done: bool) {
    if let Some(job) = state
        .live_jobs
        .iter_mut()
        .find(|job| !call_id.is_empty() && job.call_id == call_id)
    {
        if !role.is_empty() {
            job.role = role.to_owned();
        }
        if !step.is_empty() {
            push_job_step(job, step);
        }
        job.done = done;
        return;
    }
    if let Some(job) = state.live_jobs.iter_mut().rev().find(|job| !job.done)
        && (call_id.is_empty() || job.call_id.is_empty())
    {
        if !call_id.is_empty() {
            job.call_id = call_id.to_owned();
        }
        if !role.is_empty() {
            job.role = role.to_owned();
        }
        if !step.is_empty() {
            push_job_step(job, step);
        }
        job.done = done;
        return;
    }
    let step = if step.is_empty() {
        "starting".to_owned()
    } else {
        step.to_owned()
    };
    state.live_jobs.push(LiveJob {
        call_id: call_id.to_owned(),
        role: role.to_owned(),
        label: String::new(),
        prompt: String::new(),
        path: String::new(),
        log: vec![step.clone()],
        step,
        done,
    });
}

fn push_job_step(job: &mut LiveJob, step: &str) {
    job.step = step.to_owned();
    if job.log.last().is_none_or(|last| last != step) {
        job.log.push(step.to_owned());
        if job.log.len() > 48 {
            job.log.remove(0);
        }
    }
}

fn apply_live_job_progress(state: &mut WorkspaceState, call_id: &str, name: &str, message: &str) {
    if name != "task" && !message.starts_with("task|") {
        return;
    }
    if let Some((role, phase, detail)) = parse_task_progress(message) {
        match phase {
            "queued" => {
                upsert_live_job(state, call_id, role, "queued", false);
                if let Some(job) = live_job_mut(state, call_id) {
                    job.label = detail.to_owned();
                }
            }
            "prompt" => {
                upsert_live_job(state, call_id, role, "starting", false);
                if let Some(job) = live_job_mut(state, call_id) {
                    job.prompt = detail.chars().take(8_000).collect();
                }
            }
            "path" => {
                upsert_live_job(state, call_id, role, "starting", false);
                if let Some(job) = live_job_mut(state, call_id) {
                    job.path = detail.to_owned();
                }
            }
            "done" => upsert_live_job(state, call_id, role, "done", true),
            "tool" => upsert_live_job(state, call_id, role, &format!("running {detail}"), false),
            "step" => upsert_live_job(state, call_id, role, detail, false),
            other => upsert_live_job(state, call_id, role, other, false),
        }
        return;
    }
    upsert_live_job(state, call_id, "", message, false);
}

pub(super) fn live_job_mut<'a>(
    state: &'a mut WorkspaceState,
    call_id: &str,
) -> Option<&'a mut LiveJob> {
    if call_id.is_empty() {
        return state.live_jobs.iter_mut().rev().find(|job| !job.done);
    }
    state
        .live_jobs
        .iter_mut()
        .find(|job| job.call_id == call_id)
}

fn live_job_status(state: &WorkspaceState, call_id: &str) -> String {
    let job = if call_id.is_empty() {
        state.live_jobs.iter().rev().find(|job| !job.done)
    } else {
        state.live_jobs.iter().find(|job| job.call_id == call_id)
    };
    let Some(job) = job else {
        return "subagent".to_owned();
    };
    let who = if job.role.is_empty() {
        "subagent"
    } else {
        job.role.as_str()
    };
    let detail = if matches!(job.step.as_str(), "queued" | "starting") && !job.label.is_empty() {
        job.label.as_str()
    } else {
        job.step.as_str()
    };
    format!("{who} · {detail}")
}

pub(super) fn finish_live_job(state: &mut WorkspaceState, call_id: &str) {
    if let Some(job) = live_job_mut(state, call_id) {
        job.done = true;
        if job.step.is_empty() || job.step == "starting" || job.step == "queued" {
            job.step = "done".to_owned();
        }
    }
}
