//! Reads one bounded opaque Pack component from its canonical artifact path.
//!
//! This surface does not inspect installations, inventories, signatures,
//! trust state, or activation state. Callers select the typed family, Pack ID,
//! and canonical version; this module owns the sole artifact path mapping.

use std::path::PathBuf;

use zeroize::Zeroizing;

use crate::secure_fs::owned_file::read_owned_file;
use crate::{CanonicalVersion, ConfigError, HomeLayout, PackId, PluginFamily};

/// Canonical inventory path of an executable Pack component.
pub(crate) const PACK_COMPONENT_BUNDLE_PATH: &str = "component.wasm";

/// Maximum byte length of one Pack component artifact.
pub const MAX_PACK_COMPONENT_BYTES: usize = 4 * 1024 * 1024;

/// Reads one Pack component artifact without creating filesystem objects.
///
/// The sole mapping is
/// `plugins/<family>/packs/<pack-id>/versions/<canonical-semver>/component.wasm`
/// below `home`. Returned bytes are exact and opaque. Declarative Packs may
/// omit this artifact.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security, file I/O, or when the
/// artifact exceeds [`MAX_PACK_COMPONENT_BYTES`].
pub fn read_pack_component(
    home: &HomeLayout,
    family: PluginFamily,
    pack_id: &PackId,
    version: &CanonicalVersion,
) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
    let relative = component_relative_path(family, pack_id, version);
    let target = home.root().join(&relative);
    read_owned_file(home, relative, MAX_PACK_COMPONENT_BYTES)
        .map_err(|error| error.with_path(&target))
}

fn component_relative_path(
    family: PluginFamily,
    pack_id: &PackId,
    version: &CanonicalVersion,
) -> PathBuf {
    PathBuf::from("plugins")
        .join(family.directory_name())
        .join("packs")
        .join(pack_id.as_str())
        .join("versions")
        .join(version.as_str())
        .join(PACK_COMPONENT_BUNDLE_PATH)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{MAX_PACK_COMPONENT_BYTES, read_pack_component};
    use crate::secure_fs::owned_file::replace_owned_file;
    use crate::{CanonicalVersion, ConfigErrorKind, HomeLayout, PackId, PluginFamily};

    fn layout() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("temporary parent");
        let home = HomeLayout::from_root(parent.path().join("home")).expect("valid layout");
        (parent, home)
    }

    fn pack_id(value: &str) -> PackId {
        PackId::parse(value).expect("Pack ID")
    }

    fn version(value: &str) -> CanonicalVersion {
        CanonicalVersion::parse(value).expect("canonical version")
    }

    #[test]
    fn exact_canonical_path_returns_exact_opaque_bytes() {
        let (_parent, home) = layout();
        let relative = PathBuf::from(
            "plugins/web/packs/web-search/versions/1.2.3-alpha.1+build.7/component.wasm",
        );
        let artifact = b"\0asm\x01\0\0\0opaque-pack-component";
        replace_owned_file(&home, &relative, artifact).expect("write component fixture");

        let bytes = read_pack_component(
            &home,
            PluginFamily::Web,
            &pack_id("web-search"),
            &version("1.2.3-alpha.1+build.7"),
        )
        .expect("read component")
        .expect("present component");

        assert_eq!(bytes.as_slice(), artifact);
    }

    #[test]
    fn missing_component_creates_nothing() {
        let (parent, home) = layout();

        let bytes = read_pack_component(
            &home,
            PluginFamily::Providers,
            &pack_id("openai"),
            &version("1.0.0"),
        )
        .expect("missing read");

        assert!(bytes.is_none());
        assert_eq!(
            fs::read_dir(parent.path()).expect("parent listing").count(),
            0
        );
    }

    #[test]
    fn legacy_bundle_path_is_not_an_executable_alias() {
        let (_parent, home) = layout();
        replace_owned_file(
            &home,
            "plugins/web/packs/web-search/versions/1.0.0/bin/main.wasm",
            b"legacy",
        )
        .expect("write legacy fixture");

        let bytes = read_pack_component(
            &home,
            PluginFamily::Web,
            &pack_id("web-search"),
            &version("1.0.0"),
        )
        .expect("canonical read");

        assert!(bytes.is_none());
    }

    #[test]
    fn four_mib_component_is_accepted() {
        let (_parent, home) = layout();
        let relative = "plugins/ui/packs/runtime/versions/2.0.0/component.wasm";
        let artifact = vec![0x5a; MAX_PACK_COMPONENT_BYTES];
        replace_owned_file(&home, relative, &artifact).expect("write boundary fixture");

        let bytes = read_pack_component(
            &home,
            PluginFamily::Ui,
            &pack_id("runtime"),
            &version("2.0.0"),
        )
        .expect("read boundary component")
        .expect("present boundary component");

        assert_eq!(bytes.as_slice(), artifact);
    }

    #[test]
    fn component_larger_than_four_mib_is_rejected() {
        let (_parent, home) = layout();
        let relative = PathBuf::from("plugins/usage/packs/question/versions/3.0.0/component.wasm");
        replace_owned_file(&home, &relative, &vec![0xa5; MAX_PACK_COMPONENT_BYTES + 1])
            .expect("write oversized fixture");

        let error = read_pack_component(
            &home,
            PluginFamily::Usage,
            &pack_id("question"),
            &version("3.0.0"),
        )
        .expect_err("oversized component");

        assert_eq!(error.kind(), ConfigErrorKind::Oversized);
        assert_eq!(error.path(), Some(home.root().join(relative).as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_is_rejected_without_reading_target() {
        use std::os::unix::fs::symlink;

        let (parent, home) = layout();
        let relative = PathBuf::from("plugins/web/packs/files/versions/1.0.0/component.wasm");
        replace_owned_file(&home, &relative, b"fixture").expect("write component fixture");
        fs::remove_file(home.root().join(&relative)).expect("remove component");
        let outside = parent.path().join("outside.wasm");
        fs::write(&outside, b"outside").expect("outside fixture");
        symlink(&outside, home.root().join(&relative)).expect("component symlink");

        let error = read_pack_component(
            &home,
            PluginFamily::Web,
            &pack_id("files"),
            &version("1.0.0"),
        )
        .expect_err("symlink rejection");

        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
        assert_eq!(fs::read(outside).expect("outside bytes"), b"outside");
    }
}
