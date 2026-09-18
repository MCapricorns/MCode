//! Generation-bound issuance for exact configured Pack selections.
use std::collections::HashSet;
use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, Mutex, MutexGuard};

use mcode_config::{
    AuthorityRevision, PackId, PluginFamily, RootComposition, RootCompositionDocument,
};

const FAMILY_COUNT: usize = PluginFamily::ALL.len();

const PACK_SELECTION_PREFIX: &str = "psel1-";
// Match the repository's Host-issued transaction entropy while keeping the
// complete wire token at a fixed 38-byte size.
const PACK_SELECTION_RANDOM_BYTES: usize = 16;
// Bound collision recovery so a broken random source cannot spin in a Hostcall.
// Changing this value changes the maximum number of random-source invocations.
const PACK_SELECTION_MINT_ATTEMPTS: usize = 8;
const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

type FamilyProjection = [Vec<PackId>; FAMILY_COUNT];
type RandomFill = dyn Fn(&mut [u8; PACK_SELECTION_RANDOM_BYTES]) -> Result<(), ()> + Send + Sync;

/// Reports one stable, non-sensitive Pack configuration publication failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackConfigurationError {
    /// The director has begun its final shutdown.
    Closed,
    /// The supplied root composition revision was older than accepted state.
    RevisionRegression,
    /// The supplied same-revision root composition differed from accepted state.
    RevisionConflict,
    /// Pack configuration synchronization is unavailable.
    Unavailable,
}

impl Display for PackConfigurationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("Pack configuration authority is closed"),
            Self::RevisionRegression => {
                formatter.write_str("Pack configuration revision regressed")
            }
            Self::RevisionConflict => {
                formatter.write_str("Pack configuration revision conflicts with accepted state")
            }
            Self::Unavailable => formatter.write_str("Pack configuration authority is unavailable"),
        }
    }
}

impl std::error::Error for PackConfigurationError {}

pub(crate) struct PackSelectionAuthority {
    state: Mutex<AuthorityState>,
    random_fill: Box<RandomFill>,
}

struct AuthorityState {
    closed: bool,
    document: Option<RootCompositionDocument>,
    projection: FamilyProjection,
    live_stamps: HashSet<String>,
}

impl PackSelectionAuthority {
    pub(crate) fn new() -> Arc<Self> {
        Self::with_random(|bytes| getrandom::fill(bytes).map_err(|_| ()))
    }

    fn with_random(
        random_fill: impl Fn(&mut [u8; PACK_SELECTION_RANDOM_BYTES]) -> Result<(), ()>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AuthorityState {
                closed: false,
                document: None,
                projection: empty_projection(),
                live_stamps: HashSet::new(),
            }),
            random_fill: Box::new(random_fill),
        })
    }

    pub(crate) fn publish(
        &self,
        document: Option<RootCompositionDocument>,
    ) -> Result<(), PackConfigurationError> {
        let target_revision = document
            .as_ref()
            .map_or(AuthorityRevision::ABSENT, RootCompositionDocument::revision);
        let mut state = self.lock_state()?;
        if state.closed {
            return Err(PackConfigurationError::Closed);
        }
        let current_revision = state
            .document
            .as_ref()
            .map_or(AuthorityRevision::ABSENT, RootCompositionDocument::revision);
        if target_revision < current_revision {
            return Err(PackConfigurationError::RevisionRegression);
        }
        if target_revision == current_revision {
            return if document == state.document {
                Ok(())
            } else {
                Err(PackConfigurationError::RevisionConflict)
            };
        }
        state.projection = project(
            document
                .as_ref()
                .expect("a positive root revision has a composition")
                .composition(),
        );
        state.document = document;
        Ok(())
    }

    pub(crate) fn client(self: &Arc<Self>, family: PluginFamily) -> PackSelectionClient {
        PackSelectionClient {
            authority: Arc::clone(self),
            family,
            cache: None,
        }
    }

    pub(crate) fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.live_stamps.clear();
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, AuthorityState>, PackConfigurationError> {
        self.state
            .lock()
            .map_err(|_| PackConfigurationError::Unavailable)
    }

    fn issue(
        &self,
        family: PluginFamily,
        cache: &mut Option<ConfiguredPackSelection>,
    ) -> Result<ConfiguredPackSelection, PackSelectionIssueError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PackSelectionIssueError::Unavailable)?;
        if state.closed {
            return Err(PackSelectionIssueError::Unavailable);
        }
        let revision = state
            .document
            .as_ref()
            .map_or(AuthorityRevision::ABSENT, RootCompositionDocument::revision);
        let pack_ids = state.projection[family_index(family)].clone();
        if let Some(cached) = cache.as_ref()
            && cached.revision == revision
            && cached.pack_ids == pack_ids
        {
            return Ok(cached.clone());
        }

        let stamp = self.mint_stamp(&state.live_stamps)?;
        state.live_stamps.insert(stamp.clone());
        if let Some(previous) = cache.replace(ConfiguredPackSelection {
            stamp,
            revision,
            pack_ids,
        }) {
            state.live_stamps.remove(&previous.stamp);
        }
        Ok(cache
            .as_ref()
            .expect("a minted Pack selection is cached")
            .clone())
    }

    fn mint_stamp(&self, live_stamps: &HashSet<String>) -> Result<String, PackSelectionIssueError> {
        for _ in 0..PACK_SELECTION_MINT_ATTEMPTS {
            let mut random = [0_u8; PACK_SELECTION_RANDOM_BYTES];
            (self.random_fill)(&mut random).map_err(|()| PackSelectionIssueError::Unavailable)?;
            let stamp = encode_stamp(random);
            if !live_stamps.contains(&stamp) {
                return Ok(stamp);
            }
        }
        Err(PackSelectionIssueError::Unavailable)
    }

    fn release(&self, cache: &Option<ConfiguredPackSelection>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(selection) = cache.as_ref() {
            state.live_stamps.remove(&selection.stamp);
        }
    }
}

pub(crate) struct PackSelectionClient {
    authority: Arc<PackSelectionAuthority>,
    family: PluginFamily,
    cache: Option<ConfiguredPackSelection>,
}

impl PackSelectionClient {
    pub(crate) fn issue(&mut self) -> Result<ConfiguredPackSelection, PackSelectionIssueError> {
        self.authority.issue(self.family, &mut self.cache)
    }

    pub(crate) fn begin_activation(
        &self,
        selection_stamp: &str,
    ) -> Result<PackActivationTarget, PackActivationError> {
        let state = self
            .authority
            .state
            .lock()
            .map_err(|_| PackActivationError::Unavailable)?;
        if state.closed {
            return Err(PackActivationError::Unavailable);
        }
        let target = self
            .cache
            .as_ref()
            .filter(|selection| selection.stamp == selection_stamp)
            .cloned()
            .ok_or(PackActivationError::InvalidSelection)?;
        if !self.activation_is_current(&state, &target) {
            return Err(PackActivationError::InvalidSelection);
        }
        drop(state);
        Ok(PackActivationTarget(target))
    }

    pub(crate) fn commit_activation<'a>(
        &'a self,
        target: &PackActivationTarget,
    ) -> Result<PackActivationCommit<'a>, PackActivationError> {
        Ok(PackActivationCommit {
            _authority: self.lock_current_activation(&target.0)?,
        })
    }

    fn lock_current_activation<'a>(
        &'a self,
        target: &ConfiguredPackSelection,
    ) -> Result<MutexGuard<'a, AuthorityState>, PackActivationError> {
        let state = self
            .authority
            .state
            .lock()
            .map_err(|_| PackActivationError::Unavailable)?;
        if state.closed {
            return Err(PackActivationError::Unavailable);
        }
        if !self.activation_is_current(&state, target) {
            return Err(PackActivationError::InvalidSelection);
        }
        Ok(state)
    }

    fn activation_is_current(
        &self,
        state: &AuthorityState,
        target: &ConfiguredPackSelection,
    ) -> bool {
        let revision = state
            .document
            .as_ref()
            .map_or(AuthorityRevision::ABSENT, RootCompositionDocument::revision);
        self.cache.as_ref() == Some(target)
            && target.revision == revision
            && target.pack_ids == state.projection[family_index(self.family)]
    }
}

impl Drop for PackSelectionClient {
    fn drop(&mut self) {
        self.authority.release(&self.cache);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfiguredPackSelection {
    stamp: String,
    revision: AuthorityRevision,
    pack_ids: Vec<PackId>,
}

impl ConfiguredPackSelection {
    pub(crate) fn into_wire(self) -> (String, Vec<String>) {
        (
            self.stamp,
            self.pack_ids
                .into_iter()
                .map(|pack_id| pack_id.as_str().to_owned())
                .collect(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackActivationTarget(ConfiguredPackSelection);

impl PackActivationTarget {
    pub(crate) fn pack_ids(&self) -> &[PackId] {
        &self.0.pack_ids
    }

    pub(crate) fn selection_stamp(&self) -> &str {
        &self.0.stamp
    }
}

pub(crate) struct PackActivationCommit<'a> {
    _authority: MutexGuard<'a, AuthorityState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackActivationError {
    InvalidSelection,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackSelectionIssueError {
    Unavailable,
}

fn empty_projection() -> FamilyProjection {
    std::array::from_fn(|_| Vec::new())
}
fn family_index(family: PluginFamily) -> usize {
    PluginFamily::ALL
        .iter()
        .position(|candidate| *candidate == family)
        .expect("every family appears exactly once in the stable family order")
}

fn project(composition: &RootComposition) -> FamilyProjection {
    PluginFamily::ALL.map(|family| match family {
        PluginFamily::Providers => composition.providers().to_vec(),
        PluginFamily::Usage => composition.usage().to_vec(),
        PluginFamily::Ui => composition.ui().themes().to_vec(),
        singleton => composition
            .singleton(singleton)
            .expect("the fixed non-list family has one singleton slot")
            .cloned()
            .into_iter()
            .collect(),
    })
}

fn encode_stamp(random: [u8; PACK_SELECTION_RANDOM_BYTES]) -> String {
    let mut stamp = String::with_capacity(PACK_SELECTION_PREFIX.len() + random.len() * 2);
    stamp.push_str(PACK_SELECTION_PREFIX);
    for byte in random {
        stamp.push(LOWER_HEX[usize::from(byte >> 4)] as char);
        stamp.push(LOWER_HEX[usize::from(byte & 0x0f)] as char);
    }
    stamp
}

#[cfg(test)]
#[path = "pack_selection/tests.rs"]
mod tests;
