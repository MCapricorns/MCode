//! Ordering and budget regressions for handle-relative directory listings.
use std::ffi::OsString;
use tokio_util::sync::CancellationToken;

use super::super::{Limits, PathOrderKey, WalkLimiter, open_directory_nofollow};
use super::{ListedName, collect_listing, lossy_component, sort_listing};

fn listed(name: OsString) -> ListedName {
    ListedName {
        name,
        kind: None,
        skip: false,
        hidden_attr: false,
    }
}

fn names(entries: Vec<ListedName>) -> Vec<OsString> {
    entries.into_iter().map(|entry| entry.name).collect()
}

fn legacy_sort(mut entries: Vec<ListedName>) -> Vec<ListedName> {
    entries.sort_by(|left, right| {
        PathOrderKey::from_rendered_and_raw(
            lossy_component(&left.name).into_owned(),
            left.name.clone(),
        )
        .cmp(&PathOrderKey::from_rendered_and_raw(
            lossy_component(&right.name).into_owned(),
            right.name.clone(),
        ))
    });
    entries
}

#[test]
fn decorated_unicode_sort_matches_comparison_rendering_byte_for_byte() {
    let input = ["日本語", "zebra", "Éclair", "alpha", "ünicode", "Ωmega"]
        .map(OsString::from)
        .into_iter()
        .map(listed)
        .collect::<Vec<_>>();
    let expected = names(legacy_sort(input.clone()));
    let actual = names(sort_listing(input.clone()));
    let reversed = names(sort_listing(input.into_iter().rev().collect()));

    assert_eq!(actual, expected);
    assert_eq!(reversed, expected);
    assert_eq!(&actual[..3], &expected[..3], "top-N prefix changed");
}

#[test]
fn completed_listing_allocates_one_rendered_key_per_name() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["日本語", "zebra", "Éclair", "alpha", "ünicode", "Ωmega"] {
        std::fs::write(directory.path().join(name), b"").unwrap();
    }
    let limits = Limits {
        reverse_dir_enum: true,
        ..Limits::default()
    };
    let limiter = WalkLimiter::new(&limits);
    let listing = collect_listing(
        &open_directory_nofollow(directory.path()).unwrap(),
        &limiter,
        &CancellationToken::new(),
    )
    .unwrap();

    assert_eq!(listing.len(), 6);
    assert_eq!(limiter.listing_key_allocations(), 6);
    assert_eq!(
        names(listing),
        names(legacy_sort(
            ["日本語", "zebra", "Éclair", "alpha", "ünicode", "Ωmega"]
                .map(OsString::from)
                .into_iter()
                .map(listed)
                .collect(),
        ))
    );
}

#[cfg(unix)]
#[test]
fn unix_lossy_collision_uses_complete_raw_os_string_tie_break() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let first = OsString::from_vec(b"prefix-\x80-suffix-a".to_vec());
    let second = OsString::from_vec(b"prefix-\x81-suffix-a".to_vec());
    assert_eq!(lossy_component(&first), lossy_component(&second));

    let sorted = names(sort_listing(vec![listed(second), listed(first)]));
    assert_eq!(sorted[0].as_bytes(), b"prefix-\x80-suffix-a");
    assert_eq!(sorted[1].as_bytes(), b"prefix-\x81-suffix-a");
}

#[cfg(windows)]
#[test]
fn windows_valid_unicode_raw_tie_uses_the_complete_os_string() {
    use std::os::windows::ffi::OsStringExt;

    // Unpaired surrogates both render as U+FFFD; the long prefix proves the
    // listing sort compares the complete UTF-16 OsString, not a truncated key.
    let prefix = vec![u16::from(b'a'); 180];
    let mut first_units = prefix.clone();
    first_units.push(0xD800);
    first_units.extend([u16::from(b's'), u16::from(b'a')]);
    let mut second_units = prefix;
    second_units.push(0xD801);
    second_units.extend([u16::from(b's'), u16::from(b'a')]);
    let first = OsString::from_wide(&first_units);
    let second = OsString::from_wide(&second_units);
    assert_eq!(lossy_component(&first), lossy_component(&second));

    let sorted = names(sort_listing(vec![
        listed(second.clone()),
        listed(first.clone()),
    ]));
    assert_eq!(sorted[0], first);
    assert_eq!(sorted[1], second);
}

#[test]
fn exact_and_over_entry_limits_stop_before_an_unreserved_access() {
    for entry_count in [3usize, 4] {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..entry_count {
            std::fs::write(directory.path().join(format!("f{index}.txt")), b"").unwrap();
        }
        let limits = Limits {
            max_walk_entries: 3,
            ..Limits::default()
        };
        let limiter = WalkLimiter::new(&limits);
        let cancel = CancellationToken::new();
        let directory_file = open_directory_nofollow(directory.path()).unwrap();

        let error = match collect_listing(&directory_file, &limiter, &cancel) {
            Ok(_) => panic!("entry_count={entry_count}: listing unexpectedly completed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("entry limit"), "{error}");
        assert_eq!(limiter.walk_entries(), 3);
        assert_eq!(limiter.stopped_reason(), Some("walk entry limit reached"));

        let accesses_at_stop = limiter.entry_accesses();
        let stopped = collect_listing(&directory_file, &limiter, &cancel).unwrap();
        assert!(stopped.is_empty());
        assert_eq!(
            limiter.entry_accesses(),
            accesses_at_stop,
            "entry_count={entry_count}: stopped listing performed another platform access"
        );
    }
}
