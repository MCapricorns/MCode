use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mcode_config::{
    AuthorityRevision, HomeLayout, PackId, PluginFamily, RootComposition, RootCompositionDocument,
    UiSelection, replace_root_composition,
};

use super::{
    PackActivationError, PackConfigurationError, PackSelectionAuthority, PackSelectionIssueError,
    family_index, project,
};

fn pack(value: impl AsRef<str>) -> PackId {
    PackId::parse(value).expect("valid Pack ID")
}

fn document(composition: &RootComposition) -> RootCompositionDocument {
    let parent = tempfile::tempdir().expect("temporary parent");
    let home = HomeLayout::from_root(parent.path().join("home")).expect("valid layout");
    replace_root_composition(&home, AuthorityRevision::ABSENT, composition)
        .expect("initial root composition")
}

fn documents(
    first: &RootComposition,
    second: &RootComposition,
) -> (RootCompositionDocument, RootCompositionDocument) {
    let parent = tempfile::tempdir().expect("temporary parent");
    let home = HomeLayout::from_root(parent.path().join("home")).expect("valid layout");
    let first = replace_root_composition(&home, AuthorityRevision::ABSENT, first)
        .expect("initial root composition");
    let second = replace_root_composition(&home, first.revision(), second)
        .expect("advanced root composition");
    (first, second)
}

fn deterministic_authority() -> (Arc<PackSelectionAuthority>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(1));
    let fill_calls = Arc::clone(&calls);
    let authority = PackSelectionAuthority::with_random(move |bytes| {
        let value = fill_calls.fetch_add(1, Ordering::Relaxed) as u8;
        bytes.fill(value);
        Ok(())
    });
    (authority, calls)
}

#[test]
fn empty_composition_projects_every_family_to_empty() {
    let projection = project(&RootComposition::empty());

    for family in PluginFamily::ALL {
        assert!(projection[family_index(family)].is_empty());
    }
}

#[test]
fn projection_preserves_lists_selects_singletons_and_carries_ui_themes() {
    let providers = vec![pack("provider-z"), pack("provider-a")];
    let usage = vec![pack("usage-second"), pack("usage-first")];
    let themes = vec![pack("theme-a"), pack("theme-b")];
    let mut composition = RootComposition::new(
        None,
        providers.clone(),
        usage.clone(),
        UiSelection::new(themes.clone()).expect("valid UI selection"),
    )
    .expect("valid root composition");
    for family in PluginFamily::SINGLETONS {
        composition
            .set_singleton(
                family,
                Some(pack(format!("{}-pack", family.directory_name()))),
            )
            .expect("valid singleton family");
    }

    let projection = project(&composition);

    assert_eq!(projection[family_index(PluginFamily::Providers)], providers);
    assert_eq!(projection[family_index(PluginFamily::Usage)], usage);
    assert_eq!(projection[family_index(PluginFamily::Ui)], themes);
    for family in PluginFamily::SINGLETONS {
        assert_eq!(
            projection[family_index(family)],
            vec![pack(format!("{}-pack", family.directory_name()))]
        );
    }
}

#[test]
fn projection_retains_all_256_pack_ids_in_exact_order() {
    let providers = (0..256)
        .map(|index| pack(format!("provider-{index:03}")))
        .collect::<Vec<_>>();
    let composition =
        RootComposition::new(None, providers.clone(), Vec::new(), UiSelection::empty())
            .expect("bounded provider selection");

    assert_eq!(
        project(&composition)[family_index(PluginFamily::Providers)],
        providers
    );
}

#[test]
fn publication_enforces_independent_revision_regression_and_conflict() {
    let authority = PackSelectionAuthority::new();
    let mut changed = RootComposition::empty();
    changed
        .set_singleton(PluginFamily::Web, Some(pack("web-pack")))
        .expect("Session singleton");
    let (first, second) = documents(&RootComposition::empty(), &changed);
    let conflicting_first = document(&changed);

    assert_eq!(authority.publish(None), Ok(()));
    assert_eq!(authority.publish(Some(first.clone())), Ok(()));
    assert_eq!(authority.publish(Some(first)), Ok(()));
    assert_eq!(
        authority.publish(None),
        Err(PackConfigurationError::RevisionRegression)
    );
    assert_eq!(
        authority.publish(Some(conflicting_first)),
        Err(PackConfigurationError::RevisionConflict)
    );
    assert_eq!(authority.publish(Some(second)), Ok(()));
}

#[test]
fn one_generation_caches_one_stamp_until_configuration_advances() {
    let (authority, calls) = deterministic_authority();
    let mut client = authority.client(PluginFamily::Web);

    let absent = client.issue().expect("absent configuration selection");
    let repeated = client.issue().expect("idempotent selection");
    assert_eq!(absent, repeated);
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    let mut configured = RootComposition::empty();
    configured
        .set_singleton(PluginFamily::Web, Some(pack("web-primary")))
        .expect("Session singleton");
    authority
        .publish(Some(document(&configured)))
        .expect("configuration advance");
    let advanced = client.issue().expect("advanced configuration selection");
    assert_ne!(advanced.stamp, absent.stamp);
    assert_eq!(advanced.pack_ids, vec![pack("web-primary")]);
    assert_eq!(calls.load(Ordering::Relaxed), 3);
}

#[test]
fn new_client_gets_a_new_well_formed_private_stamp() {
    let (authority, _) = deterministic_authority();
    let mut first_client = authority.client(PluginFamily::Providers);
    let first = first_client.issue().expect("first generation selection");
    let mut second_client = authority.client(PluginFamily::Providers);
    let second = second_client.issue().expect("second generation selection");

    assert_ne!(first.stamp, second.stamp);
    for stamp in [&first.stamp, &second.stamp] {
        assert_eq!(stamp.len(), 38);
        assert!(stamp.starts_with("psel1-"));
        assert!(
            stamp[6..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }
}

#[test]
fn live_stamp_collision_retries_without_aliasing_clients() {
    let values = Arc::new(Mutex::new(VecDeque::from([
        [1_u8; 16], [1_u8; 16], [2_u8; 16],
    ])));
    let fill_values = Arc::clone(&values);
    let authority = PackSelectionAuthority::with_random(move |bytes| {
        *bytes = fill_values
            .lock()
            .expect("available random fixture")
            .pop_front()
            .expect("bounded random request");
        Ok(())
    });
    let mut first_client = authority.client(PluginFamily::Providers);
    let first = first_client.issue().expect("first live stamp");
    let mut second_client = authority.client(PluginFamily::Usage);
    let second = second_client.issue().expect("collision retry");

    assert_ne!(first.stamp, second.stamp);
    assert!(values.lock().expect("available random fixture").is_empty());
}

#[test]
fn random_failure_and_collision_exhaustion_fail_without_fallback() {
    let failed = PackSelectionAuthority::with_random(|_| Err(()));
    let mut failed_client = failed.client(PluginFamily::Web);
    assert_eq!(
        failed_client.issue(),
        Err(PackSelectionIssueError::Unavailable)
    );

    let colliding = PackSelectionAuthority::with_random(|bytes| {
        bytes.fill(7);
        Ok(())
    });
    let mut live_client = colliding.client(PluginFamily::Web);
    let _live = live_client.issue().expect("first collision value");
    let mut colliding_client = colliding.client(PluginFamily::Usage);
    assert_eq!(
        colliding_client.issue(),
        Err(PackSelectionIssueError::Unavailable)
    );
    let state = colliding.state.lock().expect("available authority state");
    assert_eq!(state.live_stamps.len(), 1);
}

#[test]
fn activation_requires_the_exact_current_client_selection() {
    let (authority, _) = deterministic_authority();
    let mut first = authority.client(PluginFamily::Providers);
    let issued = first.issue().expect("first client selection");
    let target = first
        .begin_activation(&issued.stamp)
        .expect("current activation target");
    assert_eq!(target.pack_ids(), issued.pack_ids);
    assert_eq!(target.selection_stamp(), issued.stamp);
    assert_eq!(
        first.begin_activation("psel1-unknown"),
        Err(PackActivationError::InvalidSelection)
    );

    let mut second = authority.client(PluginFamily::Providers);
    second.issue().expect("second client selection");
    assert!(matches!(
        second.commit_activation(&target),
        Err(PackActivationError::InvalidSelection)
    ));
}

#[test]
fn configuration_advance_invalidates_prepared_activation_before_commit() {
    let (authority, _) = deterministic_authority();
    let mut client = authority.client(PluginFamily::Web);
    let issued = client.issue().expect("initial selection");
    let target = client
        .begin_activation(&issued.stamp)
        .expect("initial activation target");
    let mut configured = RootComposition::empty();
    configured
        .set_singleton(PluginFamily::Web, Some(pack("web-primary")))
        .expect("Session singleton");
    authority
        .publish(Some(document(&configured)))
        .expect("configuration advance");

    assert!(matches!(
        client.commit_activation(&target),
        Err(PackActivationError::InvalidSelection)
    ));
}

#[test]
fn activation_commit_linearizes_with_configuration_publication() {
    let (authority, _) = deterministic_authority();
    let mut client = authority.client(PluginFamily::Providers);
    let issued = client.issue().expect("configured selection");
    let target = client
        .begin_activation(&issued.stamp)
        .expect("activation target");
    let advanced = document(&RootComposition::empty());

    let commit = client
        .commit_activation(&target)
        .expect("current activation commit");
    let (started_sender, started_receiver) = sync_channel(0);
    let (finished_sender, finished_receiver) = sync_channel(0);
    let publishing_authority = Arc::clone(&authority);
    let publication = std::thread::spawn(move || {
        started_sender.send(()).expect("report publication start");
        let result = publishing_authority.publish(Some(advanced));
        finished_sender
            .send(result)
            .expect("report publication finish");
    });

    started_receiver.recv().expect("publication thread started");
    assert_eq!(finished_receiver.try_recv(), Err(TryRecvError::Empty));
    drop(commit);
    assert_eq!(
        finished_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("publication completes after activation commit"),
        Ok(())
    );
    publication.join().expect("publication thread completes");
    assert_eq!(
        client.commit_activation(&target).err(),
        Some(PackActivationError::InvalidSelection)
    );
}

#[test]
fn close_and_poison_have_stable_fail_closed_errors() {
    let closed = PackSelectionAuthority::new();
    let mut client = closed.client(PluginFamily::Web);
    closed.close();
    assert_eq!(closed.publish(None), Err(PackConfigurationError::Closed));
    assert_eq!(client.issue(), Err(PackSelectionIssueError::Unavailable));
    assert_eq!(
        client.begin_activation("psel1-closed"),
        Err(PackActivationError::Unavailable)
    );

    let poisoned = PackSelectionAuthority::new();
    let unwind = catch_unwind(AssertUnwindSafe(|| {
        let _state = poisoned.state.lock().expect("lock authority for poisoning");
        panic!("poison Pack selection authority");
    }));
    assert!(unwind.is_err());
    assert_eq!(
        poisoned.publish(None),
        Err(PackConfigurationError::Unavailable)
    );
    let poisoned_client = poisoned.client(PluginFamily::Web);
    assert_eq!(
        poisoned_client.begin_activation("psel1-poisoned"),
        Err(PackActivationError::Unavailable)
    );
}
