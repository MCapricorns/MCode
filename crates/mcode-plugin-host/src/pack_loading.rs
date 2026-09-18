//! Loads one exact Pack candidate for one exact current Host generation.
//!
//! The loader never discovers Pack IDs, scans directories, accepts caller
//! paths, or treats the selected bundle digest as the component digest.

// Rust guideline compliant 2026-09-05.

use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use crate::generation::{GenerationActivity, GenerationFence, HostGeneration};
use crate::runtime::{CompiledPackComponent, PluginRuntime};
use crate::{ComponentLimits, ComponentWorld, MAX_COMPONENT_BYTES};
use mcode_config::{
    ArtifactRef, AuthorityRevision, HomeLayout, MAX_PACK_COMPONENT_BYTES, PackId, PluginFamily,
    Sha256Digest, SourceBindingId, TrustHighWater, read_pack_component, read_pack_installation,
};
use sha2::{Digest as Sha2Digest, Sha256};

const _: () = assert!(MAX_PACK_COMPONENT_BYTES == MAX_COMPONENT_BYTES);

#[cfg(test)]
pub(super) struct PackLoadCheckpoint {
    reached: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
impl PackLoadCheckpoint {
    pub(super) fn new(
        reached: std::sync::mpsc::SyncSender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        Self { reached, resume }
    }

    pub(super) fn pause(&self) {
        self.reached.send(()).expect("report checkpoint arrival");
        self.resume
            .recv()
            .expect("resume final generation revalidation");
    }
}

/// Reports one stable, non-sensitive Pack loading failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackLoadError {
    /// The bound Host generation is no longer current.
    StaleGeneration,
    /// The publication authority has begun its final shutdown.
    Closed,
    /// The bound family carries declarative assets only.
    FamilyHasNoComponent,
    /// The exact Pack installation authority could not be read.
    InstallationRead,
    /// The exact Pack installation authority is absent.
    InstallationMissing,
    /// The canonical component is not declared in the inventory.
    ComponentUndeclared,
    /// The exact canonical component could not be read securely.
    ComponentRead,
    /// The exact canonical component is absent.
    ComponentMissing,
    /// Component bytes do not match the canonical inventory digest.
    DigestMismatch,
    /// The component does not compile for the bound family world.
    Compilation,
}

impl Display for PackLoadError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StaleGeneration => "bound Host generation is stale",
            Self::Closed => "Host publication authority is closed",
            Self::FamilyHasNoComponent => "bound family has no executable component",
            Self::InstallationRead => "Pack installation authority is unreadable",
            Self::InstallationMissing => "Pack installation authority is missing",
            Self::ComponentUndeclared => "Pack component is not declared by its inventory",
            Self::ComponentRead => "Pack component is unreadable",
            Self::ComponentMissing => "Pack component is missing",
            Self::DigestMismatch => "Pack component digest does not match its inventory",
            Self::Compilation => "Pack component compilation failed",
        })
    }
}

impl std::error::Error for PackLoadError {}

/// Retains one verified Pack candidate without creating a Store or instance.
pub(crate) struct CompiledPackCandidate {
    generation: HostGeneration,
    verified: VerifiedPackCandidate,
}

pub(crate) struct VerifiedPackCandidate {
    family: PluginFamily,
    pack_id: PackId,
    installation_revision: AuthorityRevision,
    source: SourceBindingId,
    selected: ArtifactRef,
    trust_high_water: TrustHighWater,
    component_digest: Sha256Digest,
    world: ComponentWorld,
    component: CompiledPackComponent,
}

impl CompiledPackCandidate {
    /// Returns the exact Host generation stamped at final validation.
    pub(crate) const fn generation(&self) -> HostGeneration {
        self.generation
    }

    /// Returns the generation-derived Pack family.
    pub(crate) const fn family(&self) -> PluginFamily {
        self.verified.family
    }

    /// Returns the exact explicitly requested Pack ID.
    pub(crate) const fn pack_id(&self) -> &PackId {
        &self.verified.pack_id
    }

    /// Returns the installation revision used for this candidate.
    pub(crate) const fn installation_revision(&self) -> AuthorityRevision {
        self.verified.installation_revision
    }

    /// Returns the installation source binding.
    pub(crate) const fn source(&self) -> &SourceBindingId {
        &self.verified.source
    }

    /// Returns the mechanically selected bundle artifact.
    pub(crate) const fn selected(&self) -> &ArtifactRef {
        &self.verified.selected
    }

    /// Returns the installation trust high-water.
    pub(crate) const fn trust_high_water(&self) -> &TrustHighWater {
        &self.verified.trust_high_water
    }

    /// Returns the canonical component inventory digest.
    pub(crate) const fn component_digest(&self) -> &Sha256Digest {
        &self.verified.component_digest
    }

    /// Returns the generation-derived typed Pack world.
    pub(crate) const fn world(&self) -> ComponentWorld {
        self.verified.world
    }

    /// Consumes this candidate into its opaque compiled component.
    pub(crate) fn into_component(self) -> CompiledPackComponent {
        self.verified.component
    }
}

impl VerifiedPackCandidate {
    pub(crate) const fn component(&self) -> &CompiledPackComponent {
        &self.component
    }
}

/// Loads exact Pack IDs for one opaque current Host generation.
pub(crate) struct CurrentPackSetService<'a> {
    fence: &'a Arc<GenerationFence>,
    expected: HostGeneration,
    runtime: &'a PluginRuntime,
    home: &'a HomeLayout,
    #[cfg(test)]
    checkpoint: Option<PackLoadCheckpoint>,
}

impl<'a> CurrentPackSetService<'a> {
    pub(crate) fn new(
        fence: &'a Arc<GenerationFence>,
        runtime: &'a PluginRuntime,
        home: &'a HomeLayout,
    ) -> Self {
        Self {
            fence,
            expected: fence.generation(),
            runtime,
            home,
            #[cfg(test)]
            checkpoint: None,
        }
    }

    /// Loads one exact Pack candidate without discovery or instantiation.
    ///
    /// The bound generation must be current both before the exact authority
    /// read and again after it; any retirement in between rejects the
    /// candidate.
    ///
    /// # Errors
    ///
    /// Returns [`PackLoadError`] for a stale or closed generation,
    /// unreadable exact authority or bytes, digest mismatch, or exact-world
    /// compilation failure.
    pub(crate) fn load_candidate(
        &self,
        pack_id: &PackId,
    ) -> Result<CompiledPackCandidate, PackLoadError> {
        let _initial = self.bind_current()?;
        let family = match self.fence.domain() {
            crate::generation::GenerationDomain::Family(family) => family,
            crate::generation::GenerationDomain::Session => {
                return Err(PackLoadError::FamilyHasNoComponent);
            }
        };
        let verified = load_verified_pack(self.runtime, self.home, family, pack_id)?;
        #[cfg(test)]
        if let Some(checkpoint) = &self.checkpoint {
            checkpoint.pause();
        }
        let _final_activity = self.bind_current()?;
        Ok(CompiledPackCandidate {
            generation: self.expected,
            verified,
        })
    }

    fn bind_current(&self) -> Result<GenerationActivity, PackLoadError> {
        if self.fence.publication_closed() {
            return Err(PackLoadError::Closed);
        }
        self.fence.enter().ok_or(PackLoadError::StaleGeneration)
    }

    #[cfg(test)]
    pub(super) fn with_checkpoint(mut self, checkpoint: PackLoadCheckpoint) -> Self {
        self.checkpoint = Some(checkpoint);
        self
    }
}

pub(crate) fn load_verified_pack(
    runtime: &PluginRuntime,
    home: &HomeLayout,
    family: PluginFamily,
    pack_id: &PackId,
) -> Result<VerifiedPackCandidate, PackLoadError> {
    let world = pack_world(family)?;
    let document = read_pack_installation(home, family, pack_id)
        .map_err(|_| PackLoadError::InstallationRead)?
        .ok_or(PackLoadError::InstallationMissing)?;
    let installation = document.installation();
    let component_digest = installation
        .component_digest()
        .ok_or(PackLoadError::ComponentUndeclared)?
        .clone();
    let bytes = read_pack_component(home, family, pack_id, installation.selected().version())
        .map_err(|_| PackLoadError::ComponentRead)?
        .ok_or(PackLoadError::ComponentMissing)?;
    if !digest_matches(&bytes, &component_digest) {
        return Err(PackLoadError::DigestMismatch);
    }
    let component = runtime
        .compile_pack(&bytes, world, ComponentLimits::default())
        .map_err(|_| PackLoadError::Compilation)?;
    Ok(VerifiedPackCandidate {
        family,
        pack_id: pack_id.clone(),
        installation_revision: document.revision(),
        source: installation.source().clone(),
        selected: installation.selected().clone(),
        trust_high_water: installation.trust_high_water().clone(),
        component_digest,
        world,
        component,
    })
}

/// Compares bytes with one canonical lowercase SHA-256 authority digest.
pub(crate) fn digest_matches(bytes: &[u8], expected: &Sha256Digest) -> bool {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    Sha256Digest::parse(&encoded).is_ok_and(|parsed| parsed == *expected)
}

const fn pack_world(family: PluginFamily) -> Result<ComponentWorld, PackLoadError> {
    match family {
        PluginFamily::Providers => Ok(ComponentWorld::Provider),
        PluginFamily::Web => Ok(ComponentWorld::Web),
        PluginFamily::Mcp => Ok(ComponentWorld::Mcp),
        PluginFamily::Usage => Ok(ComponentWorld::Usage),
        PluginFamily::Ui => Err(PackLoadError::FamilyHasNoComponent),
    }
}

#[cfg(test)]
pub(crate) mod tests;
