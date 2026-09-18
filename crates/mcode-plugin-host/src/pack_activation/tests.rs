// Rust guideline compliant 2026-09-05.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{TryRecvError, sync_channel};
use std::time::Duration;

use mcode_config::{
    AuthorityRevision, PluginFamily, RootComposition, UiSelection, replace_root_composition,
};

use super::{PackActivationClient, PackActivationError};
use crate::ComponentWorld;
use crate::generation::{GenerationCommitError, GenerationFence, HostGeneration};
use crate::pack_loading::tests::{
    digest, layout, pack_component, pack_id, publish_installation, write_component,
};
use crate::pack_selection::PackSelectionAuthority;
use crate::runtime::PluginRuntime;

fn current_activity() -> (Arc<GenerationFence>, crate::generation::GenerationActivity) {
    let fence = Arc::new(GenerationFence::new(
        Arc::new(AtomicU64::new(0)),
        crate::generation::GenerationDomain::Family(PluginFamily::Providers),
        HostGeneration::new(1).expect("host generation"),
    ));
    fence.mark_current();
    let activity = fence.enter().expect("current generation activity");
    (fence, activity)
}

#[tokio::test(flavor = "current_thread")]
async fn exact_ordered_set_activation_is_idempotent() {
    let (_parent, home) = layout();
    let alpha = pack_id("pack-alpha");
    let beta = pack_id("pack-beta");
    let provider = pack_component(ComponentWorld::Provider);
    for pack_id in [&alpha, &beta] {
        write_component(&home, PluginFamily::Providers, pack_id, &provider);
        publish_installation(
            &home,
            PluginFamily::Providers,
            pack_id,
            Some(digest(&provider)),
        );
    }
    let composition = RootComposition::new(
        None,
        vec![alpha.clone(), beta.clone()],
        Vec::new(),
        UiSelection::empty(),
    )
    .expect("ordered Providers configuration");
    let document = replace_root_composition(&home, AuthorityRevision::ABSENT, &composition)
        .expect("publish ordered root configuration");
    let runtime = Arc::new(PluginRuntime::new());
    let authority = PackSelectionAuthority::new();
    authority
        .publish(Some(document))
        .expect("publish ordered selection");
    let mut client = PackActivationClient::new(
        runtime,
        home,
        PluginFamily::Providers,
        authority.client(PluginFamily::Providers),
    );
    let configured = client
        .configured_selection()
        .expect("ordered configured selection");
    let stamp = configured.into_wire().0;
    let (_fence, activity) = current_activity();

    assert_eq!(client.activate(&activity, &stamp).await, Ok(stamp.clone()));
    let active = client.active.as_ref().expect("activated ordered set");
    assert_eq!(active.target.pack_ids(), [alpha, beta]);
    assert_eq!(active.packs.len(), 2);
    let active_address = std::ptr::from_ref(active);
    let pack_addresses = active
        .packs
        .iter()
        .map(std::ptr::from_ref)
        .collect::<Vec<_>>();

    assert_eq!(client.activate(&activity, &stamp).await, Ok(stamp));
    let repeated = client.active.as_ref().expect("retained ordered set");
    assert_eq!(std::ptr::from_ref(repeated), active_address);
    assert_eq!(
        repeated
            .packs
            .iter()
            .map(std::ptr::from_ref)
            .collect::<Vec<_>>(),
        pack_addresses
    );
}

#[tokio::test(flavor = "current_thread")]
async fn empty_selection_deactivates_a_nonempty_set() {
    let (_parent, home) = layout();
    let alpha = pack_id("pack-alpha");
    let provider = pack_component(ComponentWorld::Provider);
    write_component(&home, PluginFamily::Providers, &alpha, &provider);
    publish_installation(
        &home,
        PluginFamily::Providers,
        &alpha,
        Some(digest(&provider)),
    );
    let first = RootComposition::new(None, vec![alpha], Vec::new(), UiSelection::empty())
        .expect("nonempty Providers configuration");
    let first = replace_root_composition(&home, AuthorityRevision::ABSENT, &first)
        .expect("publish nonempty root configuration");
    let runtime = Arc::new(PluginRuntime::new());
    let authority = PackSelectionAuthority::new();
    authority
        .publish(Some(first.clone()))
        .expect("publish nonempty selection");
    let mut client = PackActivationClient::new(
        runtime,
        home.clone(),
        PluginFamily::Providers,
        authority.client(PluginFamily::Providers),
    );
    let first_stamp = client
        .configured_selection()
        .expect("nonempty configured selection")
        .into_wire()
        .0;
    let (_fence, activity) = current_activity();
    client
        .activate(&activity, &first_stamp)
        .await
        .expect("activate nonempty set");
    assert_eq!(
        client
            .active
            .as_ref()
            .expect("nonempty active set")
            .packs
            .len(),
        1
    );

    let empty = replace_root_composition(&home, first.revision(), &RootComposition::empty())
        .expect("publish empty root configuration");
    authority
        .publish(Some(empty))
        .expect("publish empty selection");
    let empty_stamp = client
        .configured_selection()
        .expect("empty configured selection")
        .into_wire()
        .0;
    client
        .activate(&activity, &empty_stamp)
        .await
        .expect("deactivate complete set");

    let active = client.active.as_ref().expect("activated empty set");
    assert!(active.target.pack_ids().is_empty());
    assert!(active.packs.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn failed_replacement_retains_the_complete_previous_set() {
    let (_parent, home) = layout();
    let alpha = pack_id("pack-alpha");
    let beta = pack_id("pack-beta");
    let crossed = pack_id("pack-crossed");
    let provider = pack_component(ComponentWorld::Provider);
    for pack_id in [&alpha, &beta] {
        write_component(&home, PluginFamily::Providers, pack_id, &provider);
        publish_installation(
            &home,
            PluginFamily::Providers,
            pack_id,
            Some(digest(&provider)),
        );
    }
    let first = RootComposition::new(None, vec![alpha.clone()], Vec::new(), UiSelection::empty())
        .expect("first Providers configuration");
    let first = replace_root_composition(&home, AuthorityRevision::ABSENT, &first)
        .expect("publish first root configuration");
    let runtime = Arc::new(PluginRuntime::new());
    let authority = PackSelectionAuthority::new();
    authority
        .publish(Some(first.clone()))
        .expect("publish first selection");
    let mut client = PackActivationClient::new(
        Arc::clone(&runtime),
        home.clone(),
        PluginFamily::Providers,
        authority.client(PluginFamily::Providers),
    );
    let first_stamp = client
        .configured_selection()
        .expect("first configured selection")
        .into_wire()
        .0;
    let (_fence, activity) = current_activity();
    client
        .activate(&activity, &first_stamp)
        .await
        .expect("activate first Pack set");
    let first_active_address = std::ptr::from_ref(
        client
            .active
            .as_ref()
            .expect("first Pack set remains observable"),
    );

    let crossed_component = pack_component(ComponentWorld::Web);
    write_component(&home, PluginFamily::Providers, &crossed, &crossed_component);
    publish_installation(
        &home,
        PluginFamily::Providers,
        &crossed,
        Some(digest(&crossed_component)),
    );
    let second = RootComposition::new(None, vec![beta, crossed], Vec::new(), UiSelection::empty())
        .expect("replacement Providers configuration");
    let second = replace_root_composition(&home, first.revision(), &second)
        .expect("publish replacement root configuration");
    authority
        .publish(Some(second))
        .expect("publish replacement selection");
    let second_stamp = client
        .configured_selection()
        .expect("replacement configured selection")
        .into_wire()
        .0;

    assert_eq!(
        client.activate(&activity, &second_stamp).await,
        Err(PackActivationError::Failed)
    );
    let active = client.active.as_ref().expect("previous set remains active");
    assert_eq!(std::ptr::from_ref(active), first_active_address);
    assert_eq!(active.target.pack_ids(), std::slice::from_ref(&alpha));
    assert_eq!(active.packs.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_stamp_precedes_selected_pack_io() {
    let (_parent, home) = layout();
    let missing = pack_id("missing-pack");
    let composition = RootComposition::new(None, vec![missing], Vec::new(), UiSelection::empty())
        .expect("missing Providers configuration");
    let document = replace_root_composition(&home, AuthorityRevision::ABSENT, &composition)
        .expect("publish missing root configuration");
    let runtime = Arc::new(PluginRuntime::new());
    let authority = PackSelectionAuthority::new();
    authority
        .publish(Some(document))
        .expect("publish missing selection");
    let mut client = PackActivationClient::new(
        runtime,
        home,
        PluginFamily::Providers,
        authority.client(PluginFamily::Providers),
    );
    client
        .configured_selection()
        .expect("configured missing selection");
    let (_fence, activity) = current_activity();

    assert_eq!(
        client.activate(&activity, "psel1-invalid").await,
        Err(PackActivationError::InvalidSelection)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn retired_generation_cannot_publish_a_prepared_selection() {
    let (_parent, home) = layout();
    let runtime = Arc::new(PluginRuntime::new());
    let authority = PackSelectionAuthority::new();
    let mut client = PackActivationClient::new(
        runtime,
        home,
        PluginFamily::Providers,
        authority.client(PluginFamily::Providers),
    );
    let stamp = client
        .configured_selection()
        .expect("empty configured selection")
        .into_wire()
        .0;
    let (fence, activity) = current_activity();
    fence.mark_retired();

    assert_eq!(
        client.activate(&activity, &stamp).await,
        Err(PackActivationError::StaleGeneration)
    );
    assert!(client.active.is_none());
}

#[test]
fn generation_commit_linearizes_with_retirement() {
    let (fence, activity) = current_activity();
    let commit = activity
        .begin_commit()
        .expect("current generation commit guard");
    let (started_sender, started_receiver) = sync_channel(0);
    let (finished_sender, finished_receiver) = sync_channel(0);
    let retiring_fence = Arc::clone(&fence);
    let retirement = std::thread::spawn(move || {
        started_sender.send(()).expect("report retirement start");
        retiring_fence.mark_retired();
        finished_sender.send(()).expect("report retirement finish");
    });

    started_receiver.recv().expect("retirement thread started");
    assert_eq!(finished_receiver.try_recv(), Err(TryRecvError::Empty));
    drop(commit);
    finished_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("retirement completes after commit");
    retirement.join().expect("retirement thread completes");

    assert_eq!(
        activity.begin_commit().err(),
        Some(GenerationCommitError::Stale)
    );
}
