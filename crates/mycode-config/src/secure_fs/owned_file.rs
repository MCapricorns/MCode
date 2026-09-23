//! Coordinates validated owned-file reads and locked atomic updates.
//!
//! This module deliberately contains no document schema. It validates every
//! relative path through [`HomeLayout`], then delegates handle-relative native
//! operations to the active platform implementation.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use zeroize::Zeroizing;

use crate::{ConfigError, ConfigErrorKind, HomeLayout};

#[cfg(unix)]
use super::unix::unix_file as platform;
#[cfg(windows)]
use super::windows::windows_file as platform;

#[cfg(not(any(unix, windows)))]
mod fallback {
    use super::{ConfigError, ConfigErrorKind, OsString, Path, Zeroizing};

    pub(super) fn ensure_directory(
        _root: &Path,
        _components: &[OsString],
    ) -> Result<(), ConfigError> {
        Err(unavailable())
    }

    pub(super) fn read_file(
        _root: &Path,
        _components: &[OsString],
        _maximum_bytes: usize,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
        Err(unavailable())
    }

    pub(super) struct Transaction;

    impl Transaction {
        pub(super) fn begin(_root: &Path, _components: &[OsString]) -> Result<Self, ConfigError> {
            Err(unavailable())
        }

        pub(super) fn require_private_lock(&self) -> Result<(), ConfigError> {
            Err(unavailable())
        }

        pub(super) fn read(
            &mut self,
            _maximum_bytes: usize,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
            Err(unavailable())
        }

        pub(super) fn replace(&mut self, _bytes: &[u8]) -> Result<(), ConfigError> {
            Err(unavailable())
        }
    }

    fn unavailable() -> ConfigError {
        ConfigError::new(ConfigErrorKind::AccessControl)
    }
}

#[cfg(not(any(unix, windows)))]
use fallback as platform;

/// Creates only the owned directories named by `relative`.
///
/// Every component is created no-follow and private, so callers can
/// materialize authority directories below the owned root on demand.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::PathEscape`] for unsafe components and native
/// security, access, identity, or durability failures otherwise.
pub fn ensure_owned_directory(
    home: &HomeLayout,
    relative: impl AsRef<Path>,
) -> Result<(), ConfigError> {
    let path = OwnedPath::new(home, relative.as_ref())?;
    platform::ensure_directory(&path.root, &path.components)
}

/// Reads a private regular file without creating any filesystem object.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::Oversized`] when content exceeds
/// `maximum_bytes` and [`ConfigError`] for owned-path security, access,
/// identity, or I/O failures.
pub fn read_owned_file(
    home: &HomeLayout,
    relative: impl AsRef<Path>,
    maximum_bytes: usize,
) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
    let path = OwnedPath::new(home, relative.as_ref())?;
    require_file_name(&path)?;
    platform::read_file(&path.root, &path.components, maximum_bytes)
}

/// Replaces a private regular file while holding its persistent lock.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security, lock, access, identity,
/// serialization-bound, or durability failures.
pub fn replace_owned_file(
    home: &HomeLayout,
    relative: impl AsRef<Path>,
    bytes: &[u8],
) -> Result<(), ConfigError> {
    let path = OwnedPath::new(home, relative.as_ref())?;
    require_file_name(&path)?;
    let mut transaction = platform::Transaction::begin(&path.root, &path.components)?;
    transaction.replace(bytes)
}

/// Runs one read-modify-replace callback under a persistent advisory lock.
///
/// # Errors
///
/// Returns the callback error unchanged plus [`ConfigError`] for lock,
/// access, identity, or durability failures.
pub fn locked_update_owned_file(
    home: &HomeLayout,
    relative: impl AsRef<Path>,
    maximum_bytes: usize,
    update: impl FnOnce(Option<&[u8]>) -> Result<Vec<u8>, ConfigError>,
) -> Result<(), ConfigError> {
    let path = OwnedPath::new(home, relative.as_ref())?;
    require_file_name(&path)?;
    let mut transaction = platform::Transaction::begin(&path.root, &path.components)?;
    transaction.require_private_lock()?;
    let current = transaction.read(maximum_bytes)?;
    let replacement = update(current.as_ref().map(|bytes| bytes.as_slice()))?;
    if replacement.len() > maximum_bytes {
        return Err(ConfigError::new(ConfigErrorKind::Oversized));
    }
    transaction.replace(&replacement)
}

struct OwnedPath {
    root: PathBuf,
    components: Vec<OsString>,
}

impl OwnedPath {
    fn new(home: &HomeLayout, relative: &Path) -> Result<Self, ConfigError> {
        let joined = home.owned_join(relative)?;
        let relative = joined
            .strip_prefix(home.root())
            .map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?;
        let components = relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => Ok(name.to_os_string()),
                _ => Err(ConfigError::new(ConfigErrorKind::PathEscape)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            root: home.root().to_path_buf(),
            components,
        })
    }
}

fn require_file_name(path: &OwnedPath) -> Result<(), ConfigError> {
    if path.components.is_empty() {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Arc, Barrier};

    use super::{
        OwnedPath, ensure_owned_directory, locked_update_owned_file, platform, read_owned_file,
        replace_owned_file,
    };
    use crate::{ConfigError, ConfigErrorKind, HomeLayout};

    #[cfg(windows)]
    use crate::secure_fs::windows::windows_acl::assert_exact_private_for_tests;

    fn layout() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        (parent, layout)
    }

    fn owned(layout: &HomeLayout, relative: &str) -> PathBuf {
        layout.owned_join(relative).expect("owned path")
    }

    #[test]
    fn missing_read_creates_nothing() {
        let (parent, layout) = layout();
        let value =
            read_owned_file(&layout, "sessions/ses-1/todos.json", 64).expect("missing read");
        assert!(value.is_none());
        assert_eq!(
            fs::read_dir(parent.path()).expect("parent listing").count(),
            0
        );
    }

    #[test]
    fn wrong_case_owned_root_alias_is_rejected() {
        let parent = tempfile::tempdir().expect("parent");
        fs::create_dir(parent.path().join("Home")).expect("root alias");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");

        let error = read_owned_file(&layout, "settings.json", 64).expect_err("root alias");

        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
        let names = fs::read_dir(parent.path())
            .expect("parent listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [std::ffi::OsString::from("Home")]);
    }

    #[test]
    fn mutation_creates_only_required_ancestors_and_persistent_lock() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "sessions/ses-1/todos.json", b"value").expect("replace");

        assert_eq!(
            fs::read(owned(&layout, "sessions/ses-1/todos.json")).expect("target"),
            b"value"
        );
        let names = fs::read_dir(owned(&layout, "sessions/ses-1"))
            .expect("session listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|name| name == "todos.json"));
        assert!(names.iter().any(|name| name == "todos.json.lock"));
        assert_eq!(
            fs::read_dir(layout.root()).expect("root listing").count(),
            1
        );
        assert_eq!(
            fs::read_dir(owned(&layout, "sessions"))
                .expect("sessions listing")
                .count(),
            1
        );
    }

    #[test]
    fn root_file_mutation_creates_only_the_target_and_lock() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"value").expect("replace");

        assert!(layout.root().is_dir());
        assert!(owned(&layout, "settings.json").is_file());
        assert!(layout.root().join("settings.json.lock").is_file());
        assert_eq!(
            fs::read_dir(layout.root()).expect("root listing").count(),
            2
        );
    }

    #[test]
    fn directories_target_and_lock_have_exact_private_access() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "sessions/ses-1/todos.json", b"value").expect("replace");

        for directory in [
            layout.root().to_path_buf(),
            owned(&layout, "sessions"),
            owned(&layout, "sessions/ses-1"),
        ] {
            assert_private_evidence(&directory, true);
        }
        assert_private_evidence(&owned(&layout, "sessions/ses-1/todos.json"), false);
        assert_private_evidence(&owned(&layout, "sessions/ses-1/todos.json.lock"), false);
    }

    #[test]
    fn explicit_directory_creation_stops_at_requested_component() {
        let (_parent, layout) = layout();
        ensure_owned_directory(&layout, "sessions/ses-1").expect("directory");
        assert!(owned(&layout, "sessions/ses-1").is_dir());
        assert_eq!(
            fs::read_dir(owned(&layout, "sessions/ses-1"))
                .expect("session listing")
                .count(),
            0
        );
    }

    #[test]
    fn bounded_reads_reject_oversized_content() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"12345").expect("replace");
        let error = read_owned_file(&layout, "settings.json", 4).expect_err("oversized");
        assert_eq!(error.kind(), ConfigErrorKind::Oversized);
        let bytes = read_owned_file(&layout, "settings.json", 5).expect("bounded read");
        assert_eq!(
            bytes.as_deref().map(Vec::as_slice),
            Some(b"12345".as_slice())
        );
    }

    #[test]
    fn permissive_target_and_lock_are_replaced_or_tightened() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"old").expect("initial replace");
        let target = owned(&layout, "settings.json");
        let lock = layout.root().join("settings.json.lock");
        platform::make_permissive_for_test(&target);
        platform::make_permissive_for_test(&lock);
        let error = read_owned_file(&layout, "settings.json", 64)
            .expect_err("permissive target read must fail closed");
        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);

        replace_owned_file(&layout, "settings.json", b"new").expect("private replacement");

        assert_eq!(fs::read(&target).expect("target"), b"new");
        assert_private_evidence(&target, false);
        assert_private_evidence(&lock, false);
    }

    #[test]
    fn locked_update_rejects_permissive_target_without_change() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"old").expect("initial replace");
        platform::make_permissive_for_test(&owned(&layout, "settings.json"));

        let error = locked_update_owned_file(&layout, "settings.json", 64, |_| Ok(b"new".to_vec()))
            .expect_err("permissive locked update must fail closed");

        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
        assert_eq!(
            fs::read(owned(&layout, "settings.json")).expect("target"),
            b"old"
        );
    }

    #[test]
    fn wrong_types_and_wrong_case_aliases_are_rejected() {
        let (_parent, layout) = layout();
        ensure_owned_directory(&layout, "sessions/ses-1").expect("session directory");
        let target = owned(&layout, "sessions/ses-1/todos.json");
        fs::create_dir(&target).expect("wrong target type");
        let error = read_owned_file(&layout, "sessions/ses-1/todos.json", 64)
            .expect_err("directory target");
        assert_eq!(error.kind(), ConfigErrorKind::Io);
        fs::remove_dir(&target).expect("remove wrong type");

        fs::write(owned(&layout, "sessions/ses-1/Todos.JSON"), b"alias").expect("wrong-case alias");
        let error = read_owned_file(&layout, "sessions/ses-1/todos.json", 64)
            .expect_err("wrong-case alias");
        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
        let names = fs::read_dir(owned(&layout, "sessions/ses-1"))
            .expect("session listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| name == "Todos.JSON"));
        assert!(names.iter().all(|name| name != "todos.json"));

        let (_alias_parent, alias_layout) = self::layout();
        ensure_owned_directory(&alias_layout, "sessions").expect("sessions directory");
        fs::create_dir(owned(&alias_layout, "sessions/SES-1")).expect("intermediate alias");
        let error = read_owned_file(&alias_layout, "sessions/ses-1/todos.json", 64)
            .expect_err("wrong-case intermediate alias");
        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
    }

    #[cfg(unix)]
    #[test]
    fn unix_external_prefix_link_is_followed() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().expect("parent");
        let real = parent.path().join("real");
        fs::create_dir(&real).expect("real prefix");
        let linked = parent.path().join("linked");
        symlink(&real, &linked).expect("prefix link");
        let layout = HomeLayout::from_root(linked.join("home")).expect("layout");

        replace_owned_file(&layout, "settings.json", b"value").expect("replace through prefix");

        assert_eq!(
            fs::read(real.join("home/settings.json")).expect("target"),
            b"value"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_intermediate_and_final_links_are_rejected() {
        use std::os::unix::fs::symlink;

        let (parent, layout) = layout();
        ensure_owned_directory(&layout, "sessions").expect("sessions");
        let outside = parent.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        let session = owned(&layout, "sessions/ses-1");
        symlink(&outside, &session).expect("intermediate link");
        let error = read_owned_file(&layout, "sessions/ses-1/todos.json", 64)
            .expect_err("intermediate link");
        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
        fs::remove_file(&session).expect("remove link");

        ensure_owned_directory(&layout, "sessions/ses-1").expect("session");
        let outside_file = outside.join("todos.json");
        fs::write(&outside_file, b"outside").expect("outside file");
        symlink(&outside_file, owned(&layout, "sessions/ses-1/todos.json")).expect("final link");
        let error =
            read_owned_file(&layout, "sessions/ses-1/todos.json", 64).expect_err("final link");
        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
    }

    #[cfg(unix)]
    #[test]
    fn unix_foreign_owned_target_is_rejected_without_change() {
        use std::os::unix::fs::{MetadataExt, chown};

        if rustix::process::geteuid().as_raw() != 0 {
            eprintln!("skip: safe foreign-owner fixture requires euid 0");
            return;
        }
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"old").expect("initial replace");
        let target = owned(&layout, "settings.json");
        chown(&target, Some(65_534), None).expect("foreign owner fixture");

        let error = replace_owned_file(&layout, "settings.json", b"new")
            .expect_err("foreign owner must fail");

        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
        assert_eq!(fs::read(&target).expect("preserved target"), b"old");
        assert_eq!(
            fs::metadata(&target).expect("target metadata").uid(),
            65_534
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_external_prefix_junction_is_followed() {
        let parent = tempfile::tempdir().expect("parent");
        let real = parent.path().join("real");
        fs::create_dir(&real).expect("real prefix");
        let linked = parent.path().join("linked");
        junction::create(&real, &linked).expect("prefix junction");
        let layout = HomeLayout::from_root(linked.join("home")).expect("layout");

        replace_owned_file(&layout, "settings.json", b"value").expect("replace through prefix");

        assert_eq!(
            fs::read(real.join("home/settings.json")).expect("target"),
            b"value"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_intermediate_and_final_reparse_points_are_rejected() {
        let (parent, layout) = layout();
        ensure_owned_directory(&layout, "sessions").expect("sessions");
        let outside = parent.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        let session = owned(&layout, "sessions/ses-1");
        junction::create(&outside, &session).expect("intermediate junction");
        let error = read_owned_file(&layout, "sessions/ses-1/todos.json", 64)
            .expect_err("intermediate reparse");
        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
        junction::delete(&session).expect("remove junction reparse data");
        fs::remove_dir(&session).expect("remove junction fixture directory");

        ensure_owned_directory(&layout, "sessions/ses-1").expect("session");
        junction::create(&outside, owned(&layout, "sessions/ses-1/todos.json"))
            .expect("final junction");
        let error =
            read_owned_file(&layout, "sessions/ses-1/todos.json", 64).expect_err("final reparse");
        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
    }

    #[test]
    fn concurrent_locked_updates_do_not_lose_changes() {
        let (_parent, layout) = layout();
        let layout = Arc::new(layout);
        let barrier = Arc::new(Barrier::new(9));
        let threads = (0..8)
            .map(|_| {
                let layout = Arc::clone(&layout);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..16 {
                        locked_update_owned_file(&layout, "counter", 32, |current| {
                            let value = current
                                .map(|bytes| {
                                    std::str::from_utf8(bytes)
                                        .expect("UTF-8 counter")
                                        .parse::<u64>()
                                        .expect("counter")
                                })
                                .unwrap_or(0);
                            Ok((value + 1).to_string().into_bytes())
                        })
                        .expect("locked update");
                    }
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for thread in threads {
            thread.join().expect("update thread");
        }
        let bytes = read_owned_file(&layout, "counter", 32).expect("final read");
        assert_eq!(bytes.as_deref().map(Vec::as_slice), Some(b"128".as_slice()));
    }

    #[test]
    fn advisory_lock_excludes_a_cooperating_process() {
        let (_parent, layout) = layout();
        let path =
            OwnedPath::new(&layout, std::path::Path::new("settings.json")).expect("owned path");
        let transaction = platform::Transaction::begin(&path.root, &path.components)
            .expect("parent transaction lock");
        let status = Command::new(std::env::current_exe().expect("test executable"))
            .arg("secure_fs::owned_file::tests::cross_process_lock_helper")
            .arg("--exact")
            .env(
                "MYCODE_CONFIG_LOCK_TEST_PATH",
                layout.root().join("settings.json.lock"),
            )
            .status()
            .expect("child test process");
        drop(transaction);
        assert!(status.success(), "child lock probe failed: {status}");
    }

    #[test]
    fn cross_process_lock_helper() {
        let Some(path) = std::env::var_os("MYCODE_CONFIG_LOCK_TEST_PATH") else {
            return;
        };
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open persistent lock");
        match file.try_lock() {
            Err(fs::TryLockError::WouldBlock) => {}
            Ok(()) => panic!("cross-process lock was not held"),
            Err(fs::TryLockError::Error(error)) => {
                panic!("cross-process lock probe failed: {error}")
            }
        }
    }

    #[test]
    fn locked_update_borrows_current_and_publishes_replacement() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"old").expect("initial replace");

        locked_update_owned_file(&layout, "settings.json", 64, |current| {
            let current: Option<&[u8]> = current;
            assert_eq!(current, Some(b"old".as_slice()));
            Ok(b"new-value".to_vec())
        })
        .expect("locked update");

        assert_eq!(
            read_owned_file(&layout, "settings.json", 64)
                .expect("read replacement")
                .as_deref()
                .map(Vec::as_slice),
            Some(b"new-value".as_slice())
        );
    }

    #[test]
    fn callback_failure_preserves_target_without_temporary_file() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"old").expect("initial replace");

        let error = locked_update_owned_file(&layout, "settings.json", 64, |_| {
            Err(ConfigError::new(ConfigErrorKind::AuthorityValidation))
        })
        .expect_err("callback failure");

        assert_eq!(error.kind(), ConfigErrorKind::AuthorityValidation);
        assert_preserved_without_temporary_file(&layout);
    }

    #[test]
    fn oversized_locked_replacement_preserves_target_without_temporary_file() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"old").expect("initial replace");

        let error = locked_update_owned_file(&layout, "settings.json", 4, |_| Ok(vec![b'x'; 5]))
            .expect_err("oversized replacement");

        assert_eq!(error.kind(), ConfigErrorKind::Oversized);
        assert_preserved_without_temporary_file(&layout);
    }

    fn assert_preserved_without_temporary_file(layout: &HomeLayout) {
        assert_eq!(
            fs::read(owned(layout, "settings.json")).expect("target"),
            b"old"
        );
        assert!(
            fs::read_dir(layout.root())
                .expect("root listing")
                .map(|entry| entry.expect("entry").file_name())
                .all(|name| !name.to_string_lossy().ends_with(".tmp"))
        );
    }

    #[test]
    fn injected_pre_rename_failure_preserves_target_and_cleans_temp() {
        let (_parent, layout) = layout();
        replace_owned_file(&layout, "settings.json", b"old").expect("initial replace");
        platform::fail_before_rename_for_test();

        let error = replace_owned_file(&layout, "settings.json", b"new")
            .expect_err("injected pre-rename failure");

        assert_eq!(error.kind(), ConfigErrorKind::AtomicReplace);
        assert_eq!(
            fs::read(owned(&layout, "settings.json")).expect("target"),
            b"old"
        );
        let names = fs::read_dir(layout.root())
            .expect("root listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert!(
            names
                .iter()
                .all(|name| !name.to_string_lossy().ends_with(".tmp")),
            "temporary file leaked: {names:?}"
        );
    }

    #[test]
    fn injected_parent_durability_failure_is_propagated() {
        let (_parent, layout) = layout();
        platform::fail_parent_barrier_for_test();
        let error = replace_owned_file(&layout, "settings.json", b"new")
            .expect_err("parent durability failure");
        assert_eq!(error.kind(), ConfigErrorKind::Io);
    }

    fn assert_private_evidence(path: &std::path::Path, directory: bool) {
        let metadata = fs::metadata(path).expect("metadata");
        assert_eq!(
            metadata.is_dir(),
            directory,
            "{} has the wrong final type",
            path.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                metadata.permissions().mode() & 0o777,
                if directory { 0o700 } else { 0o600 },
                "{} is not private",
                path.display()
            );
        }
        #[cfg(windows)]
        assert_exact_private_for_tests(path);
    }
}
