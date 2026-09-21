use std::fs;

use crate::{
    AccessControlEvidence, ConfigErrorKind, HomeLayout, begin_staging, ensure_home_layout,
    probe_access_control,
};

#[test]
fn zero_file_id_is_rejected() {
    assert_eq!(
        super::identity_from_parts(1, [0; 16])
            .expect_err("zero identity")
            .kind(),
        ConfigErrorKind::AccessControl
    );
    assert!(super::identity_from_parts(1, [1; 16]).is_ok());
}

#[test]
fn payload_directory_post_create_failure_poisons_writer() {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    let mut writer = begin_staging(&layout).expect("begin");
    super::fail_next_payload_directory_prepare_for_test();
    assert_eq!(
        writer
            .write_file(&crate::BundlePath::parse("dir/file").expect("path"), b"x")
            .expect_err("post-create failure")
            .kind(),
        ConfigErrorKind::Io
    );
    assert_eq!(
        writer
            .write_file(&crate::BundlePath::parse("retry").expect("path"), b"x")
            .expect_err("poisoned")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
}

#[test]
fn journal_temp_post_create_failure_cleans_residue() {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    super::fail_next_journal_temp_prepare_for_test();
    assert!(begin_staging(&layout).is_err());
    let residues = fs::read_dir(layout.host_staging_dir())
        .expect("staging root")
        .flat_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    assert_eq!(residues.len(), 1, "begin residue is preserved");
    assert!(!residues[0].path().join(".journal.json.tmp").exists());
}

#[test]
fn existing_global_lock_is_rejected_without_dacl_repair() {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    fs::write(layout.host_staging_lock(), []).expect("foreign lock fixture");
    super::windows_file::make_permissive_for_test(&layout.host_staging_lock());

    assert_eq!(
        begin_staging(&layout).err().expect("invalid lock").kind(),
        ConfigErrorKind::AccessControl
    );
    assert!(matches!(
        probe_access_control(&layout.host_staging_lock()),
        AccessControlEvidence::WindowsProtectedDacl {
            extra_aces: 1..,
            ..
        }
    ));
    assert!(!layout.host_staging_dir().exists());
}
