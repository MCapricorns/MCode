//! Immutable launch snapshot for structured exec.
//!
//! One preparation captures cwd, the sorted allowlisted environment (including
//! reconstructed PATH), argv, and the pinned executable. Every platform spawn
//! consumes that snapshot; none of them read the process environment.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use super::ResolveError;
use super::env::{env_path, native_os_len, snapshot_child_environment, sort_env};
use super::image::ImageKind;
use super::resolve::{FileIdentity, PinnedImage, pin_program_with_path};
use crate::tool::ToolError;

/// Domain separator for the versioned invocation digest.
const INVOCATION_DIGEST_DOMAIN: &[u8] = b"mcode-tools exec-invocation v2";
/// Digest schema version mixed after the domain string.
const INVOCATION_DIGEST_VERSION: u64 = 2;

/// Immutable cwd, environment, argv, and pinned executable used for one spawn.
#[derive(Debug)]
pub(crate) struct PreparedInvocation {
    pinned: PinnedImage,
    argv0: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
    invocation_digest: [u8; 32],
}

impl PreparedInvocation {
    /// Snapshots cwd and allowlisted environment, pins `program`, and digests.
    ///
    /// Request and environment aggregate budgets are enforced before the
    /// snapshot, argv clone, and digest buffers are retained.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArgs`] when the request, environment, or
    /// image is rejected, and [`ToolError::Execution`] when cancelled.
    pub(super) fn prepare(
        session_cwd: &Path,
        program: &str,
        args: &[String],
        cancel: &CancellationToken,
    ) -> Result<Self, ToolError> {
        let env = snapshot_child_environment()?;
        Self::from_snapshot(session_cwd, program, args, &env, cancel)
            .map_err(ResolveError::into_tool_error)
    }

    /// Pins `program` with its canonical path as `argv[0]`.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::NotFound`] when PATH or path lookup misses the
    /// executable, and [`ResolveError::Other`] for every fail-closed rejection.
    pub(super) fn from_snapshot(
        session_cwd: &Path,
        program: &str,
        args: &[String],
        env: &[(OsString, OsString)],
        cancel: &CancellationToken,
    ) -> Result<Self, ResolveError> {
        Self::from_snapshot_inner(session_cwd, program, None, args, env, cancel)
    }

    /// Pins `program` while preserving `argv0` for alias-sensitive programs.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::NotFound`] when PATH or path lookup misses the
    /// executable, and [`ResolveError::Other`] for every fail-closed rejection.
    pub(super) fn from_snapshot_with_argv0(
        session_cwd: &Path,
        program: &str,
        argv0: &str,
        args: &[String],
        env: &[(OsString, OsString)],
        cancel: &CancellationToken,
    ) -> Result<Self, ResolveError> {
        Self::from_snapshot_inner(session_cwd, program, Some(argv0), args, env, cancel)
    }

    fn from_snapshot_inner(
        session_cwd: &Path,
        program: &str,
        argv0: Option<&str>,
        args: &[String],
        env: &[(OsString, OsString)],
        cancel: &CancellationToken,
    ) -> Result<Self, ResolveError> {
        let mut env = env.to_vec();
        sort_env(&mut env);
        let pinned = pin_program_with_path(session_cwd, program, args, env_path(&env), cancel)?;
        let argv0 = argv0.map_or_else(
            || {
                pinned
                    .canonical_path
                    .to_str()
                    .expect("pin_program validated canonical path Unicode")
                    .to_owned()
            },
            str::to_owned,
        );
        let invocation_digest = invocation_digest(
            &pinned.canonical_path,
            pinned.identity,
            &pinned.digest,
            &argv0,
            args,
            session_cwd,
            &env,
        );
        Ok(Self {
            pinned,
            argv0,
            args: args.to_vec(),
            cwd: session_cwd.to_path_buf(),
            env,
            invocation_digest,
        })
    }

    /// SHA-256 invocation digest over path, identity, image, argv, cwd, and env.
    #[must_use]
    pub(super) fn invocation_digest(&self) -> &[u8; 32] {
        &self.invocation_digest
    }

    /// SHA-256 digest of the pinned image bytes.
    #[must_use]
    pub(super) fn image_digest(&self) -> &[u8; 32] {
        &self.pinned.digest
    }

    /// Native identity of the pinned image.
    #[must_use]
    pub(super) fn image_identity(&self) -> FileIdentity {
        self.pinned.identity
    }

    /// Classified kernel-loadable image kind.
    #[must_use]
    pub(super) fn image_kind(&self) -> ImageKind {
        self.pinned.kind
    }

    /// Canonical native path of the pinned executable.
    #[must_use]
    pub(super) fn canonical_path(&self) -> &Path {
        &self.pinned.canonical_path
    }

    /// Effective `argv[0]` captured at preparation.
    #[cfg(test)]
    #[must_use]
    pub(super) fn argv0(&self) -> &str {
        &self.argv0
    }

    /// Argument vector captured at preparation.
    #[cfg(test)]
    #[must_use]
    pub(super) fn args(&self) -> &[String] {
        &self.args
    }

    /// Working directory captured at preparation.
    #[cfg(test)]
    #[must_use]
    pub(super) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Sorted allowlisted environment captured at preparation.
    #[must_use]
    pub(super) fn env(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    /// Splits the snapshot into its owned spawn inputs.
    pub(super) fn into_spawn_parts(
        self,
    ) -> (
        PinnedImage,
        String,
        Vec<String>,
        PathBuf,
        Vec<(OsString, OsString)>,
    ) {
        (self.pinned, self.argv0, self.args, self.cwd, self.env)
    }
}

/// Length-framed SHA-256 over the effective launch identity.
pub(super) fn invocation_digest(
    canonical_path: &Path,
    identity: FileIdentity,
    image_digest: &[u8; 32],
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    frame_bytes(&mut hasher, INVOCATION_DIGEST_DOMAIN);
    hasher.update(INVOCATION_DIGEST_VERSION.to_be_bytes());
    frame_os(&mut hasher, canonical_path.as_os_str());
    frame_identity(&mut hasher, identity);
    frame_bytes(&mut hasher, image_digest);
    frame_bytes(&mut hasher, argv0.as_bytes());
    hasher.update(u64::try_from(args.len()).unwrap_or(u64::MAX).to_be_bytes());
    for argument in args {
        frame_bytes(&mut hasher, argument.as_bytes());
    }
    frame_os(&mut hasher, cwd.as_os_str());
    hasher.update(u64::try_from(env.len()).unwrap_or(u64::MAX).to_be_bytes());
    for (key, value) in env {
        frame_os(&mut hasher, key);
        frame_os(&mut hasher, value);
    }
    hasher.finalize().into()
}

fn frame_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn frame_os(hasher: &mut Sha256, value: &OsStr) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        frame_bytes(hasher, value.as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let units: Vec<u16> = value.encode_wide().collect();
        let byte_len = u64::try_from(units.len().saturating_mul(2)).unwrap_or(u64::MAX);
        hasher.update(byte_len.to_be_bytes());
        for unit in units {
            hasher.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        frame_bytes(hasher, value.as_encoded_bytes());
    }
}

fn frame_identity(hasher: &mut Sha256, identity: FileIdentity) {
    #[cfg(unix)]
    {
        hasher.update(identity.device.to_be_bytes());
        hasher.update(identity.inode.to_be_bytes());
    }
    #[cfg(windows)]
    {
        hasher.update(identity.volume.to_be_bytes());
        hasher.update(identity.file_id);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = identity;
        hasher.update(0u64.to_be_bytes());
    }
}

/// Redacted environment lengths for UI details. Values are never copied here.
pub(super) fn environment_summary(env: &[(OsString, OsString)], limit: usize) -> serde_json::Value {
    let key_byte_lengths: Vec<usize> = env
        .iter()
        .take(limit)
        .map(|(key, _)| native_os_len(key))
        .collect();
    let value_byte_lengths: Vec<usize> = env
        .iter()
        .take(limit)
        .map(|(_, value)| native_os_len(value))
        .collect();
    serde_json::json!({
        "count": env.len(),
        "key_byte_lengths": key_byte_lengths,
        "value_byte_lengths": value_byte_lengths,
        "omitted": env.len().saturating_sub(limit),
    })
}

#[cfg(test)]
mod tests {
    use super::super::resolve::encode_hex;
    use super::*;

    fn fixture_env(path: &OsStr) -> Vec<(OsString, OsString)> {
        vec![(OsString::from("PATH"), path.to_os_string())]
    }

    fn required_host_image() -> PathBuf {
        #[cfg(windows)]
        {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let path = PathBuf::from(root).join("System32").join("whoami.exe");
            assert!(
                path.is_file(),
                "required host image {} is absent",
                path.display()
            );
            path
        }
        #[cfg(target_os = "macos")]
        {
            let path = PathBuf::from("/usr/bin/true");
            assert!(
                path.is_file(),
                "required host image /usr/bin/true is absent"
            );
            path
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
        {
            let path = PathBuf::from("/bin/true");
            assert!(path.is_file(), "required host image /bin/true is absent");
            path
        }
    }

    fn pin_host_image(cwd: &Path) -> PinnedImage {
        let program = required_host_image();
        pin_program_with_path(
            cwd,
            program.to_str().expect("host image path is Unicode"),
            &[],
            None,
            &CancellationToken::new(),
        )
        .expect("pin host image")
    }

    #[test]
    fn argv0_cwd_path_and_allowlisted_env_change_the_invocation_digest() {
        let cwd_a = tempfile::tempdir().unwrap();
        let cwd_b = tempfile::tempdir().unwrap();
        let pinned = pin_host_image(cwd_a.path());
        let argv0 = pinned
            .canonical_path
            .to_str()
            .expect("pinned path is Unicode");
        let path_a = OsString::from(cwd_a.path().as_os_str());
        let path_b = OsString::from(cwd_b.path().as_os_str());
        let env_a = fixture_env(&path_a);
        let mut env_path = env_a.clone();
        env_path[0].1 = path_b.clone();
        let mut env_extra = env_a.clone();
        env_extra.push((OsString::from("TZ"), OsString::from("UTC")));
        sort_env(&mut env_extra);

        let base = invocation_digest(
            &pinned.canonical_path,
            pinned.identity,
            &pinned.digest,
            argv0,
            &[],
            cwd_a.path(),
            &env_a,
        );
        let changed_argv0 = invocation_digest(
            &pinned.canonical_path,
            pinned.identity,
            &pinned.digest,
            "shell-alias",
            &[],
            cwd_a.path(),
            &env_a,
        );
        let changed_cwd = invocation_digest(
            &pinned.canonical_path,
            pinned.identity,
            &pinned.digest,
            argv0,
            &[],
            cwd_b.path(),
            &env_a,
        );
        let changed_path = invocation_digest(
            &pinned.canonical_path,
            pinned.identity,
            &pinned.digest,
            argv0,
            &[],
            cwd_a.path(),
            &env_path,
        );
        let changed_env = invocation_digest(
            &pinned.canonical_path,
            pinned.identity,
            &pinned.digest,
            argv0,
            &[],
            cwd_a.path(),
            &env_extra,
        );
        assert_ne!(encode_hex(&base), encode_hex(&changed_argv0));
        assert_ne!(encode_hex(&base), encode_hex(&changed_cwd));
        assert_ne!(encode_hex(&base), encode_hex(&changed_path));
        assert_ne!(encode_hex(&base), encode_hex(&changed_env));
        assert_eq!(
            encode_hex(&base),
            encode_hex(&invocation_digest(
                &pinned.canonical_path,
                pinned.identity,
                &pinned.digest,
                argv0,
                &[],
                cwd_a.path(),
                &env_a,
            ))
        );
    }

    #[test]
    fn prepared_environment_is_independent_of_later_process_env() {
        let cwd = tempfile::tempdir().unwrap();
        let program = required_host_image();
        let marker = OsString::from("mcode-exec-prepared-marker");
        let env = vec![
            (
                OsString::from("PATH"),
                std::env::var_os("PATH").unwrap_or_default(),
            ),
            (OsString::from("TZ"), marker.clone()),
        ];
        let prepared = PreparedInvocation::from_snapshot(
            cwd.path(),
            program.to_str().expect("host image path is Unicode"),
            &[],
            &env,
            &CancellationToken::new(),
        )
        .expect("prepare snapshot");
        assert_eq!(
            prepared.argv0(),
            prepared
                .canonical_path()
                .to_str()
                .expect("pinned path is Unicode")
        );
        assert_eq!(prepared.args(), &[] as &[String]);
        assert_eq!(prepared.cwd(), cwd.path());
        assert!(
            prepared
                .env()
                .iter()
                .any(|(key, value)| key.as_os_str() == "TZ" && value == &marker),
            "prepared environment lost the snapshot TZ"
        );
        let snapshot = prepared.env().to_vec();
        let digest = *prepared.invocation_digest();
        let argv0 = prepared.argv0().to_owned();
        let path = prepared.canonical_path().to_path_buf();
        let identity = prepared.image_identity();
        let image = *prepared.image_digest();
        drop(prepared);
        assert_eq!(
            digest,
            invocation_digest(&path, identity, &image, &argv0, &[], cwd.path(), &snapshot)
        );
    }

    #[test]
    fn environment_summary_redacts_values() {
        let env = vec![
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("TZ"), OsString::from("secret-timezone")),
        ];
        let summary = environment_summary(&env, 64);
        let encoded = serde_json::to_string(&summary).unwrap();
        assert!(!encoded.contains("secret-timezone"), "{encoded}");
        assert!(!encoded.contains("/bin"), "{encoded}");
        assert_eq!(summary["count"], 2);
        assert_eq!(summary["omitted"], 0);
    }
}
