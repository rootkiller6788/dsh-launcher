use std::future::Future;
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::commands::process::emit_log;
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Clone, Copy)]
pub enum HeavyJobKind {
    Install,
    Uninstall,
    InventorySync,
    Diagnostics,
    UpdateCheck,
    EnvironmentImport,
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
            Self::EnvironmentImport => "environment-import",
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
    emit_log(app, &format!("{instance_id} · running {}", kind.label()));
    let result = work().await;
    match &result {
        Ok(_) => emit_log(app, &format!("{instance_id} · finished {}", kind.label())),
        Err(e) => emit_log(
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
            emit_log(
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
