use std::future::Future;
use std::sync::{Arc, Mutex};

use launcher_core::{Job, JobPlan, LogLine, LogSink, LogStream};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::commands::process::{emit_debug, emit_warn, make_sink};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Clone, Copy)]
pub enum HeavyJobKind {
    Install,
    Uninstall,
    InventorySync,
    Diagnostics,
    UpdateCheck,
    EnvironmentExport,
    Launch,
    ProfileMutation,
}

impl HeavyJobKind {
    fn label(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
            Self::InventorySync => "inventory-sync",
            Self::Diagnostics => "diagnostics",
            Self::UpdateCheck => "update-check",
            Self::EnvironmentExport => "environment-export",
            Self::Launch => "launch",
            Self::ProfileMutation => "profile-mutation",
        }
    }

    fn is_launch(self) -> bool {
        matches!(self, Self::Launch)
    }
}

#[derive(Default)]
pub struct InstanceJobGate {
    state: AsyncMutex<GateState>,
    notify: Notify,
}

#[derive(Default)]
struct GateState {
    active: Option<HeavyJobKind>,
    waiting_launches: usize,
}

pub async fn run_instance_job<T, F, Fut>(
    state: &AppState,
    app: &AppHandle,
    instance_id: &str,
    kind: HeavyJobKind,
    work: F,
) -> Result<T, AppError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, AppError>>,
{
    let gate = instance_gate(state, instance_id)?;
    let _permit = JobPermit::acquire(gate, app, instance_id, kind).await;
    emit_debug(app, &format!("{instance_id} · running {}", kind.label()));
    let result = work().await;
    match &result {
        Ok(_) => emit_debug(app, &format!("{instance_id} · finished {}", kind.label())),
        Err(e) => emit_warn(
            app,
            &format!("{instance_id} · failed {}: {e}", kind.label()),
        ),
    }
    result
}

struct JobPermit {
    gate: Arc<InstanceJobGate>,
}

impl JobPermit {
    async fn acquire(
        gate: Arc<InstanceJobGate>,
        app: &AppHandle,
        instance_id: &str,
        kind: HeavyJobKind,
    ) -> Self {
        if kind.is_launch() {
            {
                let mut state = gate.state.lock().await;
                state.waiting_launches += 1;
            }
            wait_for_turn(&gate, app, instance_id, kind, true).await;
            let mut state = gate.state.lock().await;
            state.waiting_launches = state.waiting_launches.saturating_sub(1);
            state.active = Some(kind);
        } else {
            wait_for_turn(&gate, app, instance_id, kind, false).await;
            let mut state = gate.state.lock().await;
            state.active = Some(kind);
        }
        Self { gate }
    }
}

impl Drop for JobPermit {
    fn drop(&mut self) {
        let gate = self.gate.clone();
        tauri::async_runtime::spawn(async move {
            let mut state = gate.state.lock().await;
            state.active = None;
            gate.notify.notify_waiters();
        });
    }
}

async fn wait_for_turn(
    gate: &Arc<InstanceJobGate>,
    app: &AppHandle,
    instance_id: &str,
    kind: HeavyJobKind,
    is_launch: bool,
) {
    let mut logged = false;
    loop {
        let should_wait = {
            let state = gate.state.lock().await;
            state.active.is_some() || (!is_launch && state.waiting_launches > 0)
        };
        if !should_wait {
            return;
        }
        if !logged {
            emit_debug(
                app,
                &format!(
                    "{instance_id} · queued {} behind another heavy task",
                    kind.label()
                ),
            );
            logged = true;
        }
        gate.notify.notified().await;
    }
}

fn instance_gate(state: &AppState, instance_id: &str) -> Result<Arc<InstanceJobGate>, AppError> {
    let mut locks = state
        .heavy_jobs
        .lock()
        .map_err(|_| AppError::msg("heavy job queue lock poisoned"))?;
    Ok(locks
        .entry(instance_id.to_string())
        .or_insert_with(|| Arc::new(InstanceJobGate::default()))
        .clone())
}

// ---------------------------------------------------------------------------
// Install job executor (Stage 8: Install Center backend persistence).
//
// Install commands no longer `await` their whole install and stream fake
// progress from the frontend. They now write a `waiting` row into the SQLite
// `install_jobs` ledger and return the `Job` immediately; a per-instance
// "drainer" task claims waiting rows oldest-first, serializes them behind the
// same `run_instance_job` gate launch/refresh use, and pushes a `job-updated`
// event on every row change so a window reload or app restart can recover both
// the live queue and the terminal history.
// ---------------------------------------------------------------------------

/// Frontend event carrying a serialized [`Job`] whenever a row changes.
pub(crate) const JOB_EVENT: &str = "job-updated";

pub(crate) fn emit_job(app: &AppHandle, job: &Job) {
    let _ = app.emit(JOB_EVENT, job);
}

/// Progress/log/exit-code handle a `*_job` install body writes through. Each
/// body emits stage boundaries (which the backend now owns — the frontend no
/// longer fabricates progress) and captures sub-process stderr + exit code so a
/// failed row keeps an actionable error detail.
pub struct JobCtx {
    app: AppHandle,
    job_id: i64,
    exit_code: Arc<Mutex<Option<i64>>>,
}

impl JobCtx {
    pub(crate) fn new(app: &AppHandle, job_id: i64) -> Self {
        Self {
            app: app.clone(),
            job_id,
            exit_code: Arc::new(Mutex::new(None)),
        }
    }

    /// Advance to a named stage at a coarse percentage. Progress is anchored to
    /// real stage boundaries (download vs dsh-install vs inventory-sync); git /
    /// pnpm sub-processes have no true percentage, so this never ticks on its
    /// own. (Skill SKILL.md fetches are a few KB — byte-streaming would emit a
    /// single useless update, so those are coarse too.)
    pub(crate) fn progress(&self, stage: &str, pct: i64) {
        let app = self.app.clone();
        let state = app.state::<AppState>();
        match state.jobs.update_progress(self.job_id, stage, pct) {
            Ok(job) => {
                let _ = app.emit(JOB_EVENT, job);
            }
            Err(e) => tracing::warn!(target: "install", "job {} progress: {e}", self.job_id),
        }
    }

    /// Sink that forwards child output to Activity and appends stderr to the
    /// job's tail for the Install Center log panel.
    pub(crate) fn sink(&self) -> LogSink {
        job_sink(self.app.clone(), self.job_id)
    }

    /// Remember a sub-process exit code so a failed install row stores it.
    pub(crate) fn set_exit_code(&self, code: i64) {
        if let Ok(mut guard) = self.exit_code.lock() {
            *guard = Some(code);
        }
    }

    /// Drain the recorded exit code once, at failure-marking time.
    pub(crate) fn take_exit_code(&self) -> Option<i64> {
        self.exit_code.lock().ok().and_then(|mut guard| guard.take())
    }
}

/// Create a `waiting` job row and hand it to the instance's drainer.
pub(crate) async fn enqueue_install(
    state: &AppState,
    app: &AppHandle,
    instance_id: &str,
    key: &str,
    label: &str,
    plan: JobPlan,
) -> Result<Job, AppError> {
    let job = state.jobs.create(instance_id, key, label, &plan)?;
    emit_job(app, &job);
    kick_drainer(app, instance_id).await;
    Ok(job)
}

/// Ensure exactly one drainer is running for `instance_id` (marker in
/// `AppState.drainers`), spawning one only if none is draining yet.
async fn kick_drainer(app: &AppHandle, instance_id: &str) {
    let spawn = match app.state::<AppState>().drainers.lock() {
        Ok(mut active) => active.insert(instance_id.to_string()),
        // Poisoned marker set — can't coordinate safely; leave the job waiting
        // rather than risk double-running it.
        Err(_) => return,
    };
    if spawn {
        let app = app.clone();
        let instance_id = instance_id.to_string();
        tauri::async_runtime::spawn(async move {
            drain_instance(&app, &instance_id).await;
        });
    }
}

/// Boot-time recovery: after an app restart, drain any instance that still had
/// `waiting` rows when we exited.
pub(crate) async fn resume_pending_jobs(app: &AppHandle) {
    let instance_ids = match app.state::<AppState>().jobs.waiting_instance_ids() {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(target: "install", "resume pending jobs: {e}");
            return;
        }
    };
    for instance_id in instance_ids {
        kick_drainer(app, &instance_id).await;
    }
}

/// Drain one instance's waiting queue oldest-first. Claims are atomic
/// (`waiting` → `running` under the store lock), and each job still runs inside
/// `run_instance_job`, so installs never collide with a launch or an
/// inventory-refresh on the same instance.
async fn drain_instance(app: &AppHandle, instance_id: &str) {
    loop {
        let claimed = match app.state::<AppState>().jobs.claim_next(instance_id) {
            Ok(next) => next,
            Err(e) => {
                // DB trouble — drop the marker; the next enqueue re-spawns us.
                tracing::warn!(target: "install", "claim next job for {instance_id}: {e}");
                return;
            }
        };
        match claimed {
            Some(job) => execute_job(app, instance_id, job.id).await,
            None => {
                // Nothing to claim right now. Under the marker lock, double-check
                // that nothing enqueued between the claim and now; only remove
                // the marker when the queue is truly empty. If a job sneaks in,
                // keep draining instead of exiting — the enqueuer saw us draining
                // (marker present) and won't spawn a replacement.
                let done = {
                    let state = app.state::<AppState>();
                    let drained = state.drainers.lock();
                    match drained {
                        Ok(mut active) => {
                            let empty = state.jobs.waiting_count(instance_id).unwrap_or(1) == 0;
                            if empty {
                                active.remove(instance_id);
                            }
                            empty
                        }
                        Err(_) => true,
                    }
                };
                if done {
                    return;
                }
            }
        }
    }
}

/// Claim → run → mark. The outcome is written to the row regardless of result.
async fn execute_job(app: &AppHandle, instance_id: &str, job_id: i64) {
    let state = app.state::<AppState>();
    let plan = match state.jobs.plan(job_id) {
        Ok(plan) => plan,
        Err(e) => {
            tracing::warn!(target: "install", "job {job_id} plan unreadable: {e}");
            if let Ok(job) = state
                .jobs
                .mark_failed(job_id, &format!("install plan unreadable: {e}"), None)
            {
                emit_job(app, &job);
            }
            return;
        }
    };

    let ctx = JobCtx::new(app, job_id);
    let outcome =
        run_instance_job(&state, app, instance_id, HeavyJobKind::Install, || {
            dispatch_plan(&state, app, instance_id, plan, &ctx)
        })
        .await;

    match outcome {
        Ok(()) => {
            if let Ok(job) = state.jobs.mark_done(job_id) {
                emit_job(app, &job);
            }
        }
        Err(e) => {
            let exit_code = ctx.take_exit_code();
            match state.jobs.mark_failed(job_id, &e.to_string(), exit_code) {
                Ok(job) => emit_job(app, &job),
                Err(store_err) => {
                    tracing::error!(target: "install", "mark job {job_id} failed: {store_err}");
                }
            }
        }
    }
}

/// Route a persisted retry plan to the matching `*_job` install body. The five
/// variants cover the install surface Stage 8 tracks (uninstall / toggle /
/// update / environment-import deliberately stay outside the job store).
async fn dispatch_plan(
    state: &AppState,
    app: &AppHandle,
    instance_id: &str,
    plan: JobPlan,
    ctx: &JobCtx,
) -> Result<(), AppError> {
    match plan {
        JobPlan::Market { entry } => {
            crate::commands::content::market_install_job(state, app, instance_id, &entry, ctx)
                .await
        }
        JobPlan::Plugin { target, entry } => {
            crate::commands::plugins::plugin_install_job(
                state,
                app,
                instance_id,
                &target,
                entry.as_ref(),
                ctx,
            )
            .await
        }
        JobPlan::Skill { entry } => {
            crate::commands::content::skill_install_job(state, app, instance_id, &entry, ctx).await
        }
        JobPlan::Mcp { entry } => {
            crate::commands::content::mcp_install_job(state, app, instance_id, &entry, ctx).await
        }
        JobPlan::Bundle { manifest } => {
            crate::commands::content::bundle_import_job(state, app, instance_id, &manifest, ctx)
                .await
        }
        JobPlan::Environment { manifest } => {
            crate::commands::environment::environment_import_job(
                state,
                app,
                instance_id,
                &manifest,
                ctx,
            )
            .await
        }
    }
}

/// Child-output sink: forward to Activity like [`make_sink`], and additionally
/// append stderr lines to the job row (capped tail) + emit `job-updated`.
fn job_sink(app: AppHandle, job_id: i64) -> LogSink {
    let forward = make_sink(app.clone());
    Arc::new(move |line: LogLine| {
        if line.stream == LogStream::Stderr {
            let state = app.state::<AppState>();
            match state.jobs.append_stderr(job_id, &line.line) {
                Ok(job) => {
                    let _ = app.emit(JOB_EVENT, job);
                }
                Err(e) => tracing::warn!(target: "install", "append stderr to job {job_id}: {e}"),
            }
        }
        forward(line);
    })
}
