//! Generation-bound atomic Pack set preparation and publication.
use std::sync::Arc;

use mcode_config::{HomeLayout, PluginFamily};

use crate::generation::{GenerationActivity, GenerationCommitError};
use crate::pack_loading::{VerifiedPackCandidate, load_verified_pack};
use crate::pack_selection::{
    ConfiguredPackSelection, PackActivationError as SelectionActivationError, PackActivationTarget,
    PackSelectionClient, PackSelectionIssueError,
};
use crate::runtime::{PackInstance, PluginOwner, PluginRuntime, RuntimeError};

pub(crate) struct PackActivationClient {
    runtime: Arc<PluginRuntime>,
    home: HomeLayout,
    family: PluginFamily,
    selection: PackSelectionClient,
    active: Option<ActivePackSet>,
}

struct ActivePackSet {
    target: PackActivationTarget,
    packs: Vec<ActivePack>,
}

struct ActivePack {
    _candidate: VerifiedPackCandidate,
    _instance: PackInstance,
    _owner: PluginOwner,
}

impl PackActivationClient {
    pub(crate) const fn new(
        runtime: Arc<PluginRuntime>,
        home: HomeLayout,
        family: PluginFamily,
        selection: PackSelectionClient,
    ) -> Self {
        Self {
            runtime,
            home,
            family,
            selection,
            active: None,
        }
    }

    pub(crate) fn configured_selection(
        &mut self,
    ) -> Result<ConfiguredPackSelection, PackSelectionIssueError> {
        self.selection.issue()
    }

    /// Prepares and atomically publishes one exact configured Pack set.
    ///
    /// A matching already-active target is committed without rebuilding. Any
    /// preparation failure leaves the complete previous set active.
    pub(crate) async fn activate(
        &mut self,
        activity: &GenerationActivity,
        selection_stamp: &str,
    ) -> Result<String, PackActivationError> {
        let target = self
            .selection
            .begin_activation(selection_stamp)
            .map_err(PackActivationError::from)?;
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.target == target)
        {
            return self.commit(activity, target, None);
        }

        let mut packs = Vec::with_capacity(target.pack_ids().len());
        for pack_id in target.pack_ids() {
            let candidate = load_verified_pack(&self.runtime, &self.home, self.family, pack_id)
                .map_err(|_| PackActivationError::Failed)?;
            let mut owner = self.runtime.new_owner().map_err(map_runtime_error)?;
            let instance = owner
                .instantiate_pack(candidate.component())
                .await
                .map_err(map_runtime_error)?;
            packs.push(ActivePack {
                _candidate: candidate,
                _instance: instance,
                _owner: owner,
            });
        }
        self.commit(activity, target, Some(packs))
    }

    fn commit(
        &mut self,
        activity: &GenerationActivity,
        target: PackActivationTarget,
        replacement: Option<Vec<ActivePack>>,
    ) -> Result<String, PackActivationError> {
        let generation_commit = activity.begin_commit().map_err(|error| match error {
            GenerationCommitError::Stale => PackActivationError::StaleGeneration,
            GenerationCommitError::Unavailable => PackActivationError::Unavailable,
        })?;
        let selection_commit = self
            .selection
            .commit_activation(&target)
            .map_err(PackActivationError::from)?;
        let previous = replacement.and_then(|packs| {
            self.active.replace(ActivePackSet {
                target: target.clone(),
                packs,
            })
        });
        let selection_stamp = self
            .active
            .as_ref()
            .map_or_else(
                || target.selection_stamp(),
                |active| active.target.selection_stamp(),
            )
            .to_owned();
        drop(selection_commit);
        drop(generation_commit);
        drop(previous);
        Ok(selection_stamp)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackActivationError {
    InvalidSelection,
    StaleGeneration,
    Limit,
    Unavailable,
    Failed,
}

impl From<SelectionActivationError> for PackActivationError {
    fn from(error: SelectionActivationError) -> Self {
        match error {
            SelectionActivationError::InvalidSelection => Self::InvalidSelection,
            SelectionActivationError::Unavailable => Self::Unavailable,
        }
    }
}

fn map_runtime_error(error: RuntimeError) -> PackActivationError {
    match error {
        RuntimeError::Admission(_) => PackActivationError::Limit,
        RuntimeError::Engine
        | RuntimeError::RuntimeUninitialized
        | RuntimeError::EpochTicker
        | RuntimeError::Fuel => PackActivationError::Unavailable,
        _ => PackActivationError::Failed,
    }
}

#[cfg(test)]
#[path = "pack_activation/tests.rs"]
mod tests;
