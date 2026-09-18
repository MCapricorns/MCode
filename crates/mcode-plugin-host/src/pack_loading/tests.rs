use std::fmt::Write as _;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;

use mcode_config::{
    ArtifactRef, AuthorityRevision, BundlePath, CanonicalVersion, HomeLayout, InventoryEntry,
    MAX_PACK_COMPONENT_BYTES, PackId, PackInstallation, PluginFamily, Sha256Digest,
    SourceBindingId, TrustHighWater, begin_staging, ensure_home_layout, replace_pack_installation,
};
use sha2::{Digest, Sha256};
use wasm_encoder::reencode::{Error, Reencode, utils};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::{ManglingAndAbi, Resolve};

use super::{PackLoadCheckpoint, PackLoadError, pack_world};
use crate::ComponentWorld;
use crate::generation::GenerationFence;
use crate::runtime::PluginRuntime;

const PACK_VERSION: &str = "1.2.3";
const SELECTED_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const TRUST_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";

struct CheckpointUnblock(Option<SyncSender<()>>);

impl CheckpointUnblock {
    const fn new(sender: SyncSender<()>) -> Self {
        Self(Some(sender))
    }

    fn resume(&mut self) {
        self.0
            .take()
            .expect("checkpoint unblock is consumed once")
            .send(())
            .expect("resume final generation revalidation");
    }
}

impl Drop for CheckpointUnblock {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

pub(crate) fn pack_component(world: ComponentWorld) -> Vec<u8> {
    let (name, source) = match world {
        ComponentWorld::Web => (
            "web",
            include_str!("../../../mcode-plugin-api/wit/feature-pack/web.wit"),
        ),
        ComponentWorld::Mcp => (
            "mcp",
            include_str!("../../../mcode-plugin-api/wit/feature-pack/mcp.wit"),
        ),
        ComponentWorld::Usage => (
            "usage",
            include_str!("../../../mcode-plugin-api/wit/feature-pack/usage.wit"),
        ),
        ComponentWorld::Provider => (
            "provider",
            include_str!("../../../mcode-plugin-api/wit/provider/provider.wit"),
        ),
    };
    let mut resolve = Resolve::default();
    let package = resolve
        .push_str(name, source)
        .expect("Pack fixture WIT must parse");
    let selected = resolve
        .select_world(&[package], Some(name))
        .expect("Pack fixture world must exist");
    let module = wit_component::dummy_module(&resolve, selected, ManglingAndAbi::Standard32);
    let mut bounded = wasm_encoder::Module::new();
    BoundedMemory
        .parse_core_module(&mut bounded, wasmparser::Parser::new(0), &module)
        .expect("Pack fixture module must reencode");
    let mut module = bounded.finish();
    embed_component_metadata(&mut module, &resolve, selected, StringEncoding::UTF8)
        .expect("Pack fixture metadata must embed");
    ComponentEncoder::default()
        .module(&module)
        .expect("Pack fixture module must decode")
        .validate(true)
        .encode()
        .expect("Pack fixture component must encode")
}

struct BoundedMemory;

impl Reencode for BoundedMemory {
    type Error = std::convert::Infallible;

    fn memory_type(
        &mut self,
        memory: wasmparser::MemoryType,
    ) -> Result<wasm_encoder::MemoryType, Error<Self::Error>> {
        let mut memory = utils::memory_type(self, memory);
        memory.maximum = Some(1_024);
        Ok(memory)
    }
}

pub(crate) fn layout() -> (tempfile::TempDir, HomeLayout) {
    let parent = tempfile::tempdir().expect("temporary parent");
    let home = HomeLayout::from_root(parent.path().join("home")).expect("valid home");
    ensure_home_layout(&home).expect("secure test home");
    (parent, home)
}

pub(crate) fn pack_id(value: &str) -> PackId {
    PackId::parse(value).expect("canonical Pack ID")
}

pub(crate) fn digest(bytes: &[u8]) -> Sha256Digest {
    let mut encoded = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(encoded, "{byte:02x}").expect("write fixture digest");
    }
    Sha256Digest::parse(encoded).expect("canonical fixture digest")
}

fn fixed_digest(value: &str) -> Sha256Digest {
    Sha256Digest::parse(value).expect("canonical fixed digest")
}

pub(crate) fn write_component(home: &HomeLayout, family: PluginFamily, id: &PackId, bytes: &[u8]) {
    let mut transaction = begin_staging(home).expect("begin secure fixture staging");
    let payload = home
        .transaction_staging_dir(transaction.id())
        .join("payload");
    let relative = BundlePath::parse(format!(
        "{}/packs/{}/versions/{PACK_VERSION}/component.wasm",
        family.directory_name(),
        id.as_str()
    ))
    .expect("canonical staged Pack path");
    transaction
        .write_file(&relative, bytes)
        .expect("write secure component fixture");
    drop(transaction);
    fs::create_dir_all(home.packs_dir(family)).expect("create Pack fixture parent");
    fs::rename(
        payload
            .join(family.directory_name())
            .join("packs")
            .join(id.as_str()),
        home.pack_dir(family, id.as_str())
            .expect("canonical Pack directory"),
    )
    .expect("publish Pack component fixture");
}

pub(crate) fn publish_installation(
    home: &HomeLayout,
    family: PluginFamily,
    id: &PackId,
    component_digest: Option<Sha256Digest>,
) -> PackInstallation {
    let mut inventory = vec![InventoryEntry::new(
        BundlePath::parse("themes/dark.json").expect("canonical declarative path"),
        fixed_digest(TRUST_DIGEST),
    )];
    if let Some(component_digest) = component_digest {
        inventory.insert(
            0,
            InventoryEntry::new(
                BundlePath::parse("component.wasm").expect("canonical component path"),
                component_digest,
            ),
        );
    }
    let installation = PackInstallation::new(
        family,
        id.clone(),
        SourceBindingId::parse("official-release").expect("canonical source"),
        ArtifactRef::new(
            CanonicalVersion::parse(PACK_VERSION).expect("canonical Pack version"),
            fixed_digest(SELECTED_DIGEST),
        ),
        TrustHighWater::new(7, fixed_digest(TRUST_DIGEST)).expect("valid trust high-water"),
        inventory,
    )
    .expect("valid Pack installation");
    replace_pack_installation(home, family, id, AuthorityRevision::ABSENT, &installation)
        .expect("publish Pack installation");
    installation
}

fn current_fence(family: PluginFamily) -> Arc<GenerationFence> {
    let fence = Arc::new(GenerationFence::new(
        Arc::new(AtomicU64::new(0)),
        crate::generation::GenerationDomain::Family(family),
        crate::generation::HostGeneration::new(1).expect("host generation"),
    ));
    fence.mark_current();
    fence
}

#[tokio::test(flavor = "current_thread")]
async fn exact_candidate_preserves_verified_authority_metadata() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Providers);
    let id = pack_id("provider-main");
    let bytes = pack_component(ComponentWorld::Provider);
    let component_digest = digest(&bytes);
    write_component(&home, PluginFamily::Providers, &id, &bytes);
    let installation = publish_installation(
        &home,
        PluginFamily::Providers,
        &id,
        Some(component_digest.clone()),
    );

    let candidate = super::CurrentPackSetService::new(&fence, &runtime, &home)
        .load_candidate(&id)
        .expect("load exact Provider Pack");

    assert_eq!(candidate.generation().get(), 1);
    assert_eq!(candidate.family(), PluginFamily::Providers);
    assert_eq!(candidate.pack_id(), &id);
    assert_eq!(candidate.installation_revision().get(), 1);
    assert_eq!(candidate.source(), installation.source());
    assert_eq!(candidate.selected(), installation.selected());
    assert_eq!(
        candidate.trust_high_water(),
        installation.trust_high_water()
    );
    assert_eq!(candidate.component_digest(), &component_digest);
    assert_eq!(candidate.world(), ComponentWorld::Provider);
    drop(candidate.into_component());
}

#[tokio::test(flavor = "current_thread")]
async fn exact_pack_id_does_not_fall_back_to_a_sibling() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Providers);
    let installed = pack_id("installed");
    let requested = pack_id("requested");
    let bytes = pack_component(ComponentWorld::Provider);
    write_component(&home, PluginFamily::Providers, &installed, &bytes);
    publish_installation(
        &home,
        PluginFamily::Providers,
        &installed,
        Some(digest(&bytes)),
    );

    let error = super::CurrentPackSetService::new(&fence, &runtime, &home)
        .load_candidate(&requested)
        .err()
        .expect("unrequested sibling must not load");

    assert_eq!(error, PackLoadError::InstallationMissing);
}

#[tokio::test(flavor = "current_thread")]
async fn bound_family_does_not_fall_back_to_another_family() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Web);
    let id = pack_id("cross-family");
    let bytes = pack_component(ComponentWorld::Provider);
    write_component(&home, PluginFamily::Providers, &id, &bytes);
    publish_installation(&home, PluginFamily::Providers, &id, Some(digest(&bytes)));

    let error = super::CurrentPackSetService::new(&fence, &runtime, &home)
        .load_candidate(&id)
        .err()
        .expect("cross-family Pack must not load");

    assert_eq!(error, PackLoadError::InstallationMissing);
}

#[tokio::test(flavor = "current_thread")]
async fn retired_generation_fails_before_pack_authority_read() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Web);
    fence.mark_retired();

    assert_eq!(
        super::CurrentPackSetService::new(&fence, &runtime, &home)
            .load_candidate(&pack_id("missing"))
            .err(),
        Some(PackLoadError::StaleGeneration)
    );
}

#[test]
fn retirement_before_final_revalidation_rejects_the_loaded_candidate() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Providers);
    let id = pack_id("retirement-race");
    let bytes = pack_component(ComponentWorld::Provider);
    write_component(&home, PluginFamily::Providers, &id, &bytes);
    publish_installation(&home, PluginFamily::Providers, &id, Some(digest(&bytes)));
    let (reached_send, reached_recv) = sync_channel(0);
    let (resume_send, resume_recv) = sync_channel(0);
    let service = super::CurrentPackSetService::new(&fence, &runtime, &home)
        .with_checkpoint(PackLoadCheckpoint::new(reached_send, resume_recv));

    thread::scope(|scope| {
        let worker = scope.spawn(move || service.load_candidate(&id));
        reached_recv
            .recv()
            .expect("loader must reach final-validation checkpoint");
        let mut unblock = CheckpointUnblock::new(resume_send);
        fence.mark_retired();
        unblock.resume();
        let loaded = worker.join();
        assert_eq!(
            loaded.expect("Pack load worker must not panic").err(),
            Some(PackLoadError::StaleGeneration)
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn closed_publication_fails_before_pack_authority_read() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Usage);
    fence.close_publication();

    assert_eq!(
        super::CurrentPackSetService::new(&fence, &runtime, &home)
            .load_candidate(&pack_id("missing"))
            .err(),
        Some(PackLoadError::Closed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_component_must_be_declared_present_and_digest_exact() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Usage);
    let bytes = pack_component(ComponentWorld::Usage);

    let undeclared = pack_id("undeclared");
    write_component(&home, PluginFamily::Usage, &undeclared, &bytes);
    publish_installation(&home, PluginFamily::Usage, &undeclared, None);
    assert_eq!(
        super::CurrentPackSetService::new(&fence, &runtime, &home)
            .load_candidate(&undeclared)
            .err(),
        Some(PackLoadError::ComponentUndeclared)
    );

    let missing = pack_id("missing-bytes");
    fs::create_dir_all(
        home.pack_dir(PluginFamily::Usage, missing.as_str())
            .expect("canonical missing Pack directory"),
    )
    .expect("create missing Pack directory");
    publish_installation(&home, PluginFamily::Usage, &missing, Some(digest(&bytes)));
    assert_eq!(
        super::CurrentPackSetService::new(&fence, &runtime, &home)
            .load_candidate(&missing)
            .err(),
        Some(PackLoadError::ComponentMissing)
    );

    let mismatched = pack_id("mismatched");
    write_component(&home, PluginFamily::Usage, &mismatched, &bytes);
    publish_installation(
        &home,
        PluginFamily::Usage,
        &mismatched,
        Some(fixed_digest(TRUST_DIGEST)),
    );
    assert_eq!(
        super::CurrentPackSetService::new(&fence, &runtime, &home)
            .load_candidate(&mismatched)
            .err(),
        Some(PackLoadError::DigestMismatch)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn crossed_component_world_is_rejected_by_bound_family() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Providers);
    let id = pack_id("crossed-world");
    let bytes = pack_component(ComponentWorld::Web);
    write_component(&home, PluginFamily::Providers, &id, &bytes);
    publish_installation(&home, PluginFamily::Providers, &id, Some(digest(&bytes)));

    let error = super::CurrentPackSetService::new(&fence, &runtime, &home)
        .load_candidate(&id)
        .err()
        .expect("crossed Pack world must fail");

    assert_eq!(error, PackLoadError::Compilation);
}

#[tokio::test(flavor = "current_thread")]
async fn rogue_siblings_do_not_affect_exact_requested_candidate() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Providers);
    let valid_bytes = pack_component(ComponentWorld::Provider);

    let malformed = pack_id("aaa-malformed");
    write_component(&home, PluginFamily::Providers, &malformed, &valid_bytes);
    publish_installation(
        &home,
        PluginFamily::Providers,
        &malformed,
        Some(digest(&valid_bytes)),
    );
    fs::write(
        home.pack_installation_json(PluginFamily::Providers, malformed.as_str())
            .expect("malformed sibling path"),
        b"{",
    )
    .expect("corrupt rogue sibling authority");

    let oversized = pack_id("bbb-oversized");
    let oversized_bytes = vec![0x5a; MAX_PACK_COMPONENT_BYTES + 1];
    write_component(&home, PluginFamily::Providers, &oversized, &oversized_bytes);
    publish_installation(
        &home,
        PluginFamily::Providers,
        &oversized,
        Some(digest(&oversized_bytes)),
    );

    let requested = pack_id("zzz-requested");
    write_component(&home, PluginFamily::Providers, &requested, &valid_bytes);
    publish_installation(
        &home,
        PluginFamily::Providers,
        &requested,
        Some(digest(&valid_bytes)),
    );

    let candidate = super::CurrentPackSetService::new(&fence, &runtime, &home)
        .load_candidate(&requested)
        .expect("load exact requested Pack without scanning siblings");

    assert_eq!(candidate.pack_id(), &requested);
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_requested_component_is_a_component_read_failure() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let fence = current_fence(PluginFamily::Providers);
    let id = pack_id("oversized-requested");
    let bytes = vec![0xa5; MAX_PACK_COMPONENT_BYTES + 1];
    write_component(&home, PluginFamily::Providers, &id, &bytes);
    publish_installation(&home, PluginFamily::Providers, &id, Some(digest(&bytes)));

    let error = super::CurrentPackSetService::new(&fence, &runtime, &home)
        .load_candidate(&id)
        .err()
        .expect("oversized requested component must fail");

    assert_eq!(error, PackLoadError::ComponentRead);
}

#[test]
fn every_family_maps_to_its_typed_pack_world() {
    let cases = [
        (PluginFamily::Providers, Ok(ComponentWorld::Provider)),
        (PluginFamily::Web, Ok(ComponentWorld::Web)),
        (PluginFamily::Mcp, Ok(ComponentWorld::Mcp)),
        (PluginFamily::Usage, Ok(ComponentWorld::Usage)),
        (PluginFamily::Ui, Err(PackLoadError::FamilyHasNoComponent)),
    ];

    for (family, expected) in cases {
        assert_eq!(pack_world(family), expected);
    }
}
