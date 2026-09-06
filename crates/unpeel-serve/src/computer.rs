//! Compatibility status for the retired built-in computer-use domain.
//! No engine is resolved, installed, launched, or polled.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub const RETIRED_REASON: &str =
    "Unpeel computer use has been retired. Configure desktop tools in your agent or environment.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputerAdapterStatus {
    pub available: bool,
    pub ready: bool,
    pub reason: Option<String>,
}

impl ComputerAdapterStatus {
    pub fn wire(&self) -> Value {
        let mut value = json!({
            "computerUseAvailable": self.available,
            "computerUseReady": self.ready,
        });
        if let Some(reason) = self.reason.as_deref() {
            value["computerUseUnavailableReason"] = reason.into();
        }
        value
    }
}

pub(crate) type SharedComputerStatus = Arc<Mutex<ComputerAdapterStatus>>;

pub struct ComputerAdapter {
    status: ComputerAdapterStatus,
    shared_status: SharedComputerStatus,
}

impl Default for ComputerAdapter {
    fn default() -> Self {
        let status = ComputerAdapterStatus {
            available: false,
            ready: false,
            reason: Some(RETIRED_REASON.into()),
        };
        Self {
            shared_status: Arc::new(Mutex::new(status.clone())),
            status,
        }
    }
}

impl ComputerAdapter {
    pub fn status(&self) -> &ComputerAdapterStatus {
        &self.status
    }

    pub(crate) fn shared_status(&self) -> SharedComputerStatus {
        Arc::clone(&self.shared_status)
    }

    pub fn decorate_workspace_settings(&self, bootstrap: &mut Value) {
        decorate_workspace_settings(&self.status, bootstrap);
    }

    pub(crate) fn decorate_shared_workspace_settings(
        status: &SharedComputerStatus,
        bootstrap: &mut Value,
    ) {
        if let Ok(status) = status.lock() {
            decorate_workspace_settings(&status, bootstrap);
        }
    }
}

fn decorate_workspace_settings(status: &ComputerAdapterStatus, bootstrap: &mut Value) {
    let Some(experimental) = bootstrap
        .get_mut("workspaceSettings")
        .and_then(|settings| settings.get_mut("experimentalSettings"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    experimental.insert("computerUse".into(), false.into());
    let wire = status.wire();
    let Some(wire) = wire.as_object() else { return };
    for (key, value) in wire {
        experimental.insert(key.clone(), value.clone());
    }
    if status.reason.is_none() {
        experimental.remove("computerUseUnavailableReason");
    }
}
