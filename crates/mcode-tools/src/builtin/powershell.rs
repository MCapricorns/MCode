//! Secure provisioning for the PowerShell 7 shell backend on Windows.
//!
//! MCode uses `pwsh.exe` from `PATH` when it can be spawned. If it is absent,
//! this module installs a pinned Microsoft portable ZIP below the MCode home
//! directory. The compiled JSON matrix fixes every URL, size, and SHA-256.
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT_ENCODING, USER_AGENT};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
#[cfg(test)]
use tokio::io::AsyncReadExt as _;
use tokio::io::AsyncWriteExt as _;

use crate::tool::ToolError;

const RELEASE_MATRIX_JSON: &str = include_str!("../../assets/powershell-windows.json");
const INSTALL_RECORD_NAME: &str = "mcode-install.json";
const POWERSHELL_EXECUTABLE: &str = "pwsh.exe";
// The record contains one bounded integrity entry per extracted ZIP entry.
const MAX_INSTALL_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 4_096;
// Implicit parent directories can outnumber ZIP entries; keep cache walks bounded.
const MAX_CACHE_ENTRIES: usize = MAX_ARCHIVE_ENTRIES * 4;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 768 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const INSTALL_LOCK_TIMEOUT: Duration = Duration::from_secs(11 * 60);
const INSTALL_LOCK_RETRY: Duration = Duration::from_millis(100);
const MAX_REDIRECTS: usize = 5;

// These files form the portable host's signed startup chain. Revalidating
// them catches dependency replacement even if filesystem timestamps were
// preserved while the remaining payload uses the recorded hash on change.
const REQUIRED_SIGNED_RUNTIME_FILES: &[&str] = &[
    POWERSHELL_EXECUTABLE,
    "pwsh.dll",
    "hostfxr.dll",
    "hostpolicy.dll",
    "coreclr.dll",
    "System.Private.CoreLib.dll",
    "System.Management.Automation.dll",
    "Microsoft.PowerShell.ConsoleHost.dll",
];
const REQUIRED_DATA_FILES: &[&str] = &["pwsh.deps.json", "pwsh.runtimeconfig.json"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseMatrix {
    schema_version: u32,
    version: String,
    assets: HashMap<String, ReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseAsset {
    filename: String,
    url: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone)]
struct SelectedArtifact {
    version: String,
    architecture: String,
    asset: ReleaseAsset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledFile {
    path: String,
    size_bytes: u64,
    modified_unix_nanos: u64,
    sha256: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRecord {
    schema_version: u32,
    version: String,
    architecture: String,
    asset_filename: String,
    asset_size_bytes: u64,
    asset_sha256: String,
    files: Vec<InstalledFile>,
}

impl InstallRecord {
    fn for_artifact(artifact: &SelectedArtifact, files: Vec<InstalledFile>) -> Self {
        Self {
            schema_version: 2,
            version: artifact.version.clone(),
            architecture: artifact.architecture.clone(),
            asset_filename: artifact.asset.filename.clone(),
            asset_size_bytes: artifact.asset.size_bytes,
            asset_sha256: artifact.asset.sha256.clone(),
            files,
        }
    }

    fn matches_artifact(&self, artifact: &SelectedArtifact) -> bool {
        self.schema_version == 2
            && self.version == artifact.version
            && self.architecture == artifact.architecture
            && self.asset_filename == artifact.asset.filename
            && self.asset_size_bytes == artifact.asset.size_bytes
            && self.asset_sha256 == artifact.asset.sha256
    }
}

#[derive(Debug, Clone)]
enum ArtifactSource {
    Https(String),
    #[cfg(test)]
    Local(PathBuf),
}

/// Ensure that the pinned, managed `pwsh.exe` is available.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] if the current architecture is unsupported,
/// the pinned release metadata is invalid, the artifact cannot be downloaded,
/// or any integrity, extraction, publication, or Authenticode check fails.
pub(crate) async fn ensure_pwsh() -> Result<PathBuf, ToolError> {
    let artifact = selected_artifact().map_err(|err| {
        ToolError::Execution(format!("invalid pinned PowerShell release matrix: {err}"))
    })?;
    let source = ArtifactSource::Https(artifact.asset.url.clone());
    ensure_artifact(&mcode_home(), &artifact, source, true)
        .await
        .map_err(|err| {
            ToolError::Execution(format!(
                "pwsh.exe was not available on PATH and pinned PowerShell {} could not be \
                 provisioned: {err}; offline hosts require an intact managed cache",
                artifact.version
            ))
        })
}

fn selected_artifact() -> std::io::Result<SelectedArtifact> {
    let matrix: ReleaseMatrix = serde_json::from_str(RELEASE_MATRIX_JSON).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("release matrix is not valid JSON: {err}"),
        )
    })?;
    if matrix.schema_version != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "unsupported PowerShell release matrix schema {}",
                matrix.schema_version
            ),
        ));
    }

    let architecture = target_architecture()?;
    let asset = matrix.assets.get(architecture).cloned().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("PowerShell has no pinned Windows asset for {architecture}"),
        )
    })?;
    let artifact = SelectedArtifact {
        version: matrix.version,
        architecture: architecture.to_owned(),
        asset,
    };
    validate_artifact_metadata(&artifact)?;
    Ok(artifact)
}

fn target_architecture() -> std::io::Result<&'static str> {
    #[cfg(target_arch = "x86_64")]
    return Ok("x86_64");
    #[cfg(target_arch = "aarch64")]
    return Ok("aarch64");
    #[cfg(target_arch = "x86")]
    return Ok("x86");
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "x86")))]
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "PowerShell portable Windows artifacts do not support {}",
            std::env::consts::ARCH
        ),
    ))
}

fn validate_artifact_metadata(artifact: &SelectedArtifact) -> std::io::Result<()> {
    validate_path_component(&artifact.version, "version")?;
    validate_path_component(&artifact.architecture, "architecture")?;
    validate_path_component(&artifact.asset.filename, "asset filename")?;
    if artifact.asset.size_bytes == 0 || artifact.asset.size_bytes > MAX_DOWNLOAD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "pinned asset size {} exceeds the {}-byte policy limit",
                artifact.asset.size_bytes, MAX_DOWNLOAD_BYTES
            ),
        ));
    }
    parse_sha256(&artifact.asset.sha256)?;

    let expected_url = format!(
        "https://github.com/PowerShell/PowerShell/releases/download/v{}/{}",
        artifact.version, artifact.asset.filename
    );
    if artifact.asset.url != expected_url {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("pinned asset URL is not the expected Microsoft release URL: {expected_url}"),
        ));
    }
    Ok(())
}

fn validate_path_component(value: &str, label: &str) -> std::io::Result<()> {
    let mut components = Path::new(value).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} is not one safe path component"),
        ));
    }
    Ok(())
}

fn mcode_home() -> PathBuf {
    mcode_home_from(
        std::env::var_os("MCODE_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
    )
}

fn mcode_home_from(
    mcode_home: Option<OsString>,
    home: Option<OsString>,
    user_profile: Option<OsString>,
) -> PathBuf {
    if let Some(path) = mcode_home.filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    home.or(user_profile)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".mcode")
}

fn install_dir(home: &Path, artifact: &SelectedArtifact) -> PathBuf {
    home.join("bin")
        .join("powershell")
        .join(&artifact.version)
        .join(&artifact.architecture)
}

async fn ensure_artifact(
    home: &Path,
    artifact: &SelectedArtifact,
    source: ArtifactSource,
    verify_signature: bool,
) -> std::io::Result<PathBuf> {
    let destination = install_dir(home, artifact);
    if cache_is_valid(&destination, artifact, verify_signature).await? {
        return Ok(destination.join(POWERSHELL_EXECUTABLE));
    }

    let parent = destination.parent().ok_or_else(|| {
        std::io::Error::other("managed PowerShell destination has no parent directory")
    })?;
    tokio::fs::create_dir_all(parent).await?;
    let lock_path = parent.join(format!(".{}.install.lock", artifact.architecture));
    let install_lock = acquire_install_lock(&lock_path).await?;

    // A concurrent process may have completed the install while this process
    // waited for the cross-process file lock.
    if cache_is_valid(&destination, artifact, verify_signature).await? {
        return Ok(destination.join(POWERSHELL_EXECUTABLE));
    }

    let staging = tempfile::Builder::new()
        .prefix(".pwsh-staging-")
        .tempdir_in(parent)?;
    let archive_path = staging.path().join("artifact.zip.part");
    download_artifact(&source, artifact, &archive_path).await?;

    let payload = staging.path().join("payload");
    let destination_for_install = destination.clone();
    let artifact_for_install = artifact.clone();
    run_install_task(staging, install_lock, move || {
        install_staged_artifact(
            &archive_path,
            &payload,
            &destination_for_install,
            &artifact_for_install,
            verify_signature,
        )
    })
    .await?;

    Ok(destination.join(POWERSHELL_EXECUTABLE))
}

async fn run_install_task<F>(
    staging: tempfile::TempDir,
    install_lock: File,
    install: F,
) -> std::io::Result<()>
where
    F: FnOnce() -> std::io::Result<()> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let result = install();

        // Dropping a JoinHandle cannot preempt spawn_blocking work. Retain
        // staging ownership and the cross-process lock inside the task so a
        // timed-out caller can discard its shell-spawn continuation safely.
        drop(staging);
        drop(install_lock);
        result
    })
    .await
    .map_err(|err| std::io::Error::other(format!("PowerShell install task failed: {err}")))?
}

fn install_staged_artifact(
    archive_path: &Path,
    payload: &Path,
    destination: &Path,
    artifact: &SelectedArtifact,
    verify_signature: bool,
) -> std::io::Result<()> {
    std::fs::create_dir(payload)?;
    let installed_files = extract_archive(archive_path, payload)?;
    write_install_record(payload, artifact, installed_files)?;
    if !validate_install(payload, artifact, verify_signature) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "staged PowerShell install failed post-extraction validation",
        ));
    }
    publish_install(payload, destination)?;

    if !validate_install(destination, artifact, verify_signature) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "published PowerShell install failed validation",
        ));
    }
    Ok(())
}

async fn cache_is_valid(
    destination: &Path,
    artifact: &SelectedArtifact,
    verify_signature: bool,
) -> std::io::Result<bool> {
    let destination = destination.to_owned();
    let artifact = artifact.clone();
    tokio::task::spawn_blocking(move || {
        Ok(validate_install(&destination, &artifact, verify_signature))
    })
    .await
    .map_err(|err| std::io::Error::other(format!("cache validation task failed: {err}")))?
}

async fn acquire_install_lock(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let started = Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(err) if err.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if started.elapsed() >= INSTALL_LOCK_TIMEOUT {
                    return Err(install_lock_timeout_error());
                }
                tokio::time::sleep(INSTALL_LOCK_RETRY).await;
            }
            Err(err) => return Err(err),
        }
    }
}

fn install_lock_timeout_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!(
            "timed out after {}s waiting for the PowerShell install lock in the managed pwsh cache",
            INSTALL_LOCK_TIMEOUT.as_secs(),
        ),
    )
}

fn cache_file_label(path: &Path) -> std::borrow::Cow<'_, str> {
    path.file_name()
        .map(|name| name.to_string_lossy())
        .filter(|name| !name.is_empty())
        .unwrap_or(std::borrow::Cow::Borrowed("managed pwsh cache"))
}

async fn download_artifact(
    source: &ArtifactSource,
    artifact: &SelectedArtifact,
    destination: &Path,
) -> std::io::Result<()> {
    let mut output = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .await?;
    let mut hasher = Sha256::new();
    let mut received = 0_u64;

    match source {
        ArtifactSource::Https(url) => {
            let client = reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(DOWNLOAD_TIMEOUT)
                .https_only(true)
                .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
                .build()
                .map_err(|err| std::io::Error::other(format!("build HTTPS client: {err}")))?;
            let request = async {
                let mut response = client
                    .get(url)
                    .header(USER_AGENT, "MCode PowerShell bootstrap")
                    .header(ACCEPT_ENCODING, "identity")
                    .send()
                    .await
                    .map_err(|err| {
                        std::io::Error::other(format!(
                            "download {}: {err}",
                            artifact.asset.filename
                        ))
                    })?;
                if response.url().scheme() != "https" {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "PowerShell download redirected away from HTTPS",
                    ));
                }
                response = response.error_for_status().map_err(|err| {
                    std::io::Error::other(format!("download {}: {err}", artifact.asset.filename))
                })?;
                if let Some(length) = response.content_length()
                    && length != artifact.asset.size_bytes
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "PowerShell download Content-Length is {length}, expected {}",
                            artifact.asset.size_bytes
                        ),
                    ));
                }
                while let Some(chunk) = response.chunk().await.map_err(|err| {
                    std::io::Error::other(format!("read PowerShell download: {err}"))
                })? {
                    consume_download_chunk(
                        &chunk,
                        &mut output,
                        &mut hasher,
                        &mut received,
                        artifact.asset.size_bytes,
                    )
                    .await?;
                }
                Ok(())
            };
            tokio::time::timeout(DOWNLOAD_TIMEOUT, request)
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "PowerShell download exceeded {}s",
                            DOWNLOAD_TIMEOUT.as_secs()
                        ),
                    )
                })??;
        }
        #[cfg(test)]
        ArtifactSource::Local(path) => {
            let metadata = tokio::fs::metadata(path).await?;
            if metadata.len() != artifact.asset.size_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "injected artifact size is {}, expected {}",
                        metadata.len(),
                        artifact.asset.size_bytes
                    ),
                ));
            }
            let mut input = tokio::fs::File::open(path).await?;
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let count = input.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                consume_download_chunk(
                    &buffer[..count],
                    &mut output,
                    &mut hasher,
                    &mut received,
                    artifact.asset.size_bytes,
                )
                .await?;
            }
        }
    }

    if received != artifact.asset.size_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!(
                "PowerShell download contained {received} bytes, expected {}",
                artifact.asset.size_bytes
            ),
        ));
    }
    let expected_hash = parse_sha256(&artifact.asset.sha256)?;
    let actual_hash: [u8; 32] = hasher.finalize().into();
    if actual_hash != expected_hash {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "PowerShell download SHA-256 mismatch: got {}, expected {}",
                encode_hex(&actual_hash),
                artifact.asset.sha256
            ),
        ));
    }
    output.flush().await?;
    output.sync_all().await?;
    Ok(())
}

async fn consume_download_chunk(
    chunk: &[u8],
    output: &mut tokio::fs::File,
    hasher: &mut Sha256,
    received: &mut u64,
    expected_size: u64,
) -> std::io::Result<()> {
    let chunk_len = u64::try_from(chunk.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "download chunk length does not fit u64",
        )
    })?;
    *received = received.checked_add(chunk_len).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "PowerShell download size overflowed u64",
        )
    })?;
    if *received > expected_size || *received > MAX_DOWNLOAD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("PowerShell download exceeded its {expected_size}-byte pinned size"),
        ));
    }
    hasher.update(chunk);
    output.write_all(chunk).await
}

fn extract_archive(archive_path: &Path, destination: &Path) -> std::io::Result<Vec<InstalledFile>> {
    let archive_file = File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(archive_file).map_err(zip_error)?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "PowerShell archive has {} entries, maximum is {MAX_ARCHIVE_ENTRIES}",
                archive.len()
            ),
        ));
    }

    let mut extracted_bytes = 0_u64;
    let mut paths = HashSet::with_capacity(archive.len());
    let mut installed_files = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_error)?;
        let relative = entry.enclosed_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ZIP entry {} escapes the extraction root", entry.name()),
            )
        })?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ZIP entry {} has an unsafe path", entry.name()),
            ));
        }
        if !paths.insert(relative.clone()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ZIP contains duplicate path {}", relative.display()),
            ));
        }

        if let Some(mode) = entry.unix_mode() {
            let file_type = mode & 0o170_000;
            if !matches!(file_type, 0 | 0o040_000 | 0o100_000) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "ZIP entry {} is not a regular file or directory",
                        entry.name()
                    ),
                ));
            }
        }

        let output_path = destination.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&output_path)?;
            continue;
        }

        let entry_size = entry.size();
        if entry_size > MAX_ARCHIVE_ENTRY_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ZIP entry {} is too large", entry.name()),
            ));
        }
        extracted_bytes = extracted_bytes.checked_add(entry_size).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "PowerShell archive size overflowed u64",
            )
        })?;
        if extracted_bytes > MAX_EXTRACTED_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "PowerShell archive exceeds the {MAX_EXTRACTED_BYTES}-byte extraction limit"
                ),
            ));
        }

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let manifest_path = normalized_relative_path(&relative)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output_path)?;
        let mut hasher = Sha256::new();
        let mut copied = 0_u64;
        {
            let mut reader = entry.by_ref().take(entry_size + 1);
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count])?;
                hasher.update(&buffer[..count]);
                copied = copied
                    .checked_add(u64::try_from(count).map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "ZIP read length does not fit u64",
                        )
                    })?)
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "ZIP entry size overflowed u64",
                        )
                    })?;
            }
        }
        if copied != entry_size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "ZIP entry {} expanded to {copied} bytes, expected {entry_size}",
                    entry.name()
                ),
            ));
        }
        output.flush()?;
        drop(output);

        let metadata = std::fs::symlink_metadata(&output_path)?;
        installed_files.push(InstalledFile {
            path: manifest_path,
            size_bytes: metadata.len(),
            modified_unix_nanos: modified_unix_nanos(&metadata)?,
            sha256: encode_hex(&hasher.finalize()),
        });
    }
    installed_files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(installed_files)
}

fn zip_error(err: zip::result::ZipError) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("invalid PowerShell ZIP: {err}"),
    )
}

fn write_install_record(
    destination: &Path,
    artifact: &SelectedArtifact,
    installed_files: Vec<InstalledFile>,
) -> std::io::Result<()> {
    let record_path = destination.join(INSTALL_RECORD_NAME);
    let mut record_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(record_path)?;
    let record = InstallRecord::for_artifact(artifact, installed_files);
    serde_json::to_writer_pretty(&mut record_file, &record)?;
    record_file.write_all(b"\n")?;
    record_file.sync_all()
}

fn validate_install(
    destination: &Path,
    artifact: &SelectedArtifact,
    verify_signature: bool,
) -> bool {
    let Ok(destination_metadata) = std::fs::symlink_metadata(destination) else {
        return false;
    };
    if !destination_metadata.file_type().is_dir() {
        return false;
    }

    let record_path = destination.join(INSTALL_RECORD_NAME);
    let Ok(record_metadata) = std::fs::symlink_metadata(&record_path) else {
        return false;
    };
    if !record_metadata.file_type().is_file() || record_metadata.len() > MAX_INSTALL_RECORD_BYTES {
        return false;
    }
    let Ok(record_bytes) = std::fs::read(record_path) else {
        return false;
    };
    let Ok(record) = serde_json::from_slice::<InstallRecord>(&record_bytes) else {
        return false;
    };
    if !record.matches_artifact(artifact)
        || validate_payload_manifest(destination, &record.files).is_err()
    {
        return false;
    }

    !verify_signature
        || REQUIRED_SIGNED_RUNTIME_FILES
            .iter()
            .all(|name| verify_authenticode(&destination.join(name)).is_ok())
}

#[derive(Debug)]
struct ObservedFile {
    path: String,
    absolute_path: PathBuf,
    size_bytes: u64,
    modified_unix_nanos: u64,
}

fn validate_payload_manifest(
    destination: &Path,
    expected_files: &[InstalledFile],
) -> std::io::Result<()> {
    if expected_files.is_empty() || expected_files.len() > MAX_ARCHIVE_ENTRIES {
        return Err(invalid_install_data(
            "install manifest has an invalid file count",
        ));
    }
    if expected_files
        .windows(2)
        .any(|pair| pair[0].path >= pair[1].path)
    {
        return Err(invalid_install_data(
            "install manifest paths are not strictly sorted",
        ));
    }
    for required in REQUIRED_SIGNED_RUNTIME_FILES
        .iter()
        .chain(REQUIRED_DATA_FILES)
    {
        if !expected_files.iter().any(|file| file.path == *required) {
            return Err(invalid_install_data(format!(
                "install manifest omits required runtime file {required}"
            )));
        }
    }

    let observed_files = collect_observed_files(destination)?;
    if observed_files.len() != expected_files.len() {
        return Err(invalid_install_data(format!(
            "install contains {} files, expected {}",
            observed_files.len(),
            expected_files.len()
        )));
    }

    for (expected, observed) in expected_files.iter().zip(&observed_files) {
        if normalized_relative_path(Path::new(&expected.path))? != expected.path
            || expected.path != observed.path
        {
            return Err(invalid_install_data(format!(
                "install file path does not match manifest: {}",
                expected.path
            )));
        }
        if expected.size_bytes != observed.size_bytes {
            return Err(invalid_install_data(format!(
                "install file {} has {} bytes, expected {}",
                expected.path, observed.size_bytes, expected.size_bytes
            )));
        }

        let expected_hash = parse_sha256(&expected.sha256)?;
        let required = is_required_runtime_file(&expected.path);
        if required && observed.size_bytes == 0 {
            return Err(invalid_install_data(format!(
                "required runtime file {} is empty",
                expected.path
            )));
        }
        if required || expected.modified_unix_nanos != observed.modified_unix_nanos {
            let actual_hash = hash_file(&observed.absolute_path, expected.size_bytes)?;
            if actual_hash != expected_hash {
                return Err(invalid_install_data(format!(
                    "install file {} failed its SHA-256 check",
                    expected.path
                )));
            }
        }
    }
    Ok(())
}

fn collect_observed_files(destination: &Path) -> std::io::Result<Vec<ObservedFile>> {
    let mut directories = vec![destination.to_owned()];
    let mut observed_files = Vec::new();
    let mut entry_count = 0_usize;

    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            entry_count = entry_count.checked_add(1).ok_or_else(|| {
                invalid_install_data("managed cache entry count overflowed usize")
            })?;
            if entry_count > MAX_CACHE_ENTRIES {
                return Err(invalid_install_data(
                    "managed cache contains too many filesystem entries",
                ));
            }

            let relative = path.strip_prefix(destination).map_err(|_| {
                invalid_install_data("managed cache entry escaped the install root")
            })?;
            let normalized = normalized_relative_path(relative)?;
            let metadata = std::fs::symlink_metadata(&path)?;
            if normalized == INSTALL_RECORD_NAME {
                continue;
            }
            if metadata.file_type().is_dir() {
                directories.push(path);
            } else if metadata.file_type().is_file() {
                observed_files.push(ObservedFile {
                    path: normalized,
                    absolute_path: path,
                    size_bytes: metadata.len(),
                    modified_unix_nanos: modified_unix_nanos(&metadata)?,
                });
                if observed_files.len() > MAX_ARCHIVE_ENTRIES {
                    return Err(invalid_install_data(
                        "managed cache contains too many files",
                    ));
                }
            } else {
                return Err(invalid_install_data(
                    "managed cache contains a link or special file",
                ));
            }
        }
    }

    observed_files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(observed_files)
}

fn normalized_relative_path(path: &Path) -> std::io::Result<String> {
    let mut normalized = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(invalid_install_data(
                "install manifest path is not strictly relative",
            ));
        };
        let component = component
            .to_str()
            .ok_or_else(|| invalid_install_data("install manifest path is not valid Unicode"))?;
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    if normalized.is_empty() {
        return Err(invalid_install_data("install manifest path is empty"));
    }
    Ok(normalized)
}

fn modified_unix_nanos(metadata: &std::fs::Metadata) -> std::io::Result<u64> {
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| {
            invalid_install_data("install file modification time predates the Unix epoch")
        })?;
    u64::try_from(modified.as_nanos()).map_err(|_| {
        invalid_install_data("install file modification time does not fit u64 nanoseconds")
    })
}

fn hash_file(path: &Path, expected_size: u64) -> std::io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total =
            total
                .checked_add(u64::try_from(count).map_err(|_| {
                    invalid_install_data("install file read length does not fit u64")
                })?)
                .ok_or_else(|| invalid_install_data("install file size overflowed u64"))?;
        if total > expected_size {
            return Err(invalid_install_data(
                "install file grew while its hash was checked",
            ));
        }
    }
    if total != expected_size {
        return Err(invalid_install_data(
            "install file changed size while its hash was checked",
        ));
    }
    Ok(hasher.finalize().into())
}

fn is_required_runtime_file(path: &str) -> bool {
    REQUIRED_SIGNED_RUNTIME_FILES.contains(&path) || REQUIRED_DATA_FILES.contains(&path)
}

fn invalid_install_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn publish_install(payload: &Path, destination: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(destination) {
        Ok(_) => {
            let parent = destination.parent().ok_or_else(|| {
                std::io::Error::other("managed PowerShell destination has no parent")
            })?;
            let quarantine = tempfile::Builder::new()
                .prefix(".pwsh-replaced-")
                .tempdir_in(parent)?;
            let previous = quarantine.path().join("previous");
            std::fs::rename(destination, &previous)?;
            if let Err(publish_error) = std::fs::rename(payload, destination) {
                let restore_error = std::fs::rename(&previous, destination).err();
                return Err(std::io::Error::new(
                    publish_error.kind(),
                    format!(
                        "failed to atomically publish PowerShell ({publish_error}); restore result: {}",
                        restore_error
                            .map(|err| err.to_string())
                            .unwrap_or_else(|| "restored previous cache".to_owned())
                    ),
                ));
            }
            drop(quarantine);
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(payload, destination)
        }
        Err(err) => Err(err),
    }
}

fn parse_sha256(value: &str) -> std::io::Result<[u8; 32]> {
    if value.len() != 64 || !value.is_ascii() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "SHA-256 must contain exactly 64 lowercase hexadecimal characters",
        ));
    }
    let mut output = [0_u8; 32];
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for (index, pair) in pairs.iter().enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(output)
}

fn hex_nibble(value: u8) -> std::io::Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "SHA-256 must use lowercase hexadecimal characters",
        )),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn verify_authenticode(path: &Path) -> std::io::Result<()> {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE,
        WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE, WTD_UICONTEXT_EXECUTE,
        WinVerifyTrust,
    };

    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: u32::try_from(size_of::<WINTRUST_FILE_INFO>())
            .expect("WINTRUST_FILE_INFO size fits u32"),
        pcwszFilePath: wide_path.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut trust_data = WINTRUST_DATA {
        cbStruct: u32::try_from(size_of::<WINTRUST_DATA>()).expect("WINTRUST_DATA size fits u32"),
        pPolicyCallbackData: std::ptr::null_mut(),
        pSIPClientData: std::ptr::null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &raw mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        hWVTStateData: std::ptr::null_mut(),
        pwszURLReference: std::ptr::null_mut(),
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE,
        dwUIContext: WTD_UICONTEXT_EXECUTE,
        pSignatureSettings: std::ptr::null_mut(),
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    // SAFETY: `action`, `trust_data`, `file_info`, and the terminated UTF-16
    // path remain live and correctly sized for the call. No UI window is used.
    let verify_status = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &raw mut action,
            std::ptr::from_mut(&mut trust_data).cast(),
        )
    };
    trust_data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: this closes only the state created in `trust_data` by the prior
    // verification call; all referenced storage remains live.
    let close_status = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &raw mut action,
            std::ptr::from_mut(&mut trust_data).cast(),
        )
    };

    if verify_status != ERROR_SUCCESS as i32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "WinVerifyTrust rejected {} with status 0x{:08x}",
                cache_file_label(path),
                verify_status as u32
            ),
        ));
    }
    if close_status != ERROR_SUCCESS as i32 {
        return Err(std::io::Error::other(format!(
            "WinVerifyTrust state close failed for {} with status 0x{:08x}",
            cache_file_label(path),
            close_status as u32
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use zip::write::SimpleFileOptions;

    const FIXTURE_PWSH_DLL: &[u8] = b"fixture pwsh library";

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in entries {
            archive.start_file(*name, options).unwrap();
            archive.write_all(contents).unwrap();
        }
        archive.finish().unwrap();
    }

    fn write_fixture_zip(path: &Path, executable: &[u8]) {
        write_zip(
            path,
            &[
                (POWERSHELL_EXECUTABLE, executable),
                ("pwsh.dll", FIXTURE_PWSH_DLL),
                ("hostfxr.dll", b"fixture hostfxr"),
                ("hostpolicy.dll", b"fixture hostpolicy"),
                ("coreclr.dll", b"fixture coreclr"),
                ("System.Private.CoreLib.dll", b"fixture corelib"),
                ("System.Management.Automation.dll", b"fixture automation"),
                ("Microsoft.PowerShell.ConsoleHost.dll", b"fixture console"),
                ("pwsh.deps.json", b"{}"),
                ("pwsh.runtimeconfig.json", b"{}"),
            ],
        );
    }

    fn fixture_artifact(path: &Path) -> SelectedArtifact {
        let bytes = std::fs::read(path).unwrap();
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        SelectedArtifact {
            version: "test-1".to_owned(),
            architecture: "x86_64".to_owned(),
            asset: ReleaseAsset {
                filename: "PowerShell-test-win-x64.zip".to_owned(),
                url: "https://example.invalid/PowerShell-test-win-x64.zip".to_owned(),
                size_bytes: u64::try_from(bytes.len()).unwrap(),
                sha256: encode_hex(&hash),
            },
        }
    }

    #[test]
    fn pinned_matrix_has_only_fixed_official_assets() {
        let matrix: ReleaseMatrix = serde_json::from_str(RELEASE_MATRIX_JSON).unwrap();
        assert_eq!(matrix.schema_version, 1);
        assert_eq!(matrix.version, "7.6.5");
        assert_eq!(matrix.assets.len(), 3);
        for architecture in ["x86_64", "aarch64", "x86"] {
            let artifact = SelectedArtifact {
                version: matrix.version.clone(),
                architecture: architecture.to_owned(),
                asset: matrix.assets[architecture].clone(),
            };
            validate_artifact_metadata(&artifact).unwrap();
        }
        assert!(!RELEASE_MATRIX_JSON.contains("latest"));
    }

    #[test]
    fn home_resolution_uses_json_era_mcode_home_layout() {
        assert_eq!(
            mcode_home_from(Some(OsString::from("C:/managed")), None, None),
            PathBuf::from("C:/managed")
        );
        assert_eq!(
            mcode_home_from(None, Some(OsString::from("C:/home")), None),
            PathBuf::from("C:/home/.mcode")
        );
        assert_eq!(
            mcode_home_from(None, None, Some(OsString::from("C:/profile"))),
            PathBuf::from("C:/profile/.mcode")
        );
    }

    #[test]
    fn install_lock_timeout_error_does_not_embed_absolute_host_path() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join(".x86_64.install.lock");
        let msg = install_lock_timeout_error().to_string();
        assert!(
            !msg.contains(&dir.path().to_string_lossy().into_owned()),
            "{msg}"
        );
        assert!(!msg.contains(&lock.to_string_lossy().into_owned()), "{msg}");
        assert!(msg.contains("managed pwsh cache"), "{msg}");
    }

    #[test]
    fn authenticode_rejection_does_not_embed_absolute_host_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pwsh.dll");
        std::fs::write(&file, b"not an authenticode-signed image").unwrap();
        let err = verify_authenticode(&file).unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains(&dir.path().to_string_lossy().into_owned()),
            "{msg}"
        );
        assert!(!msg.contains(&file.to_string_lossy().into_owned()), "{msg}");
        assert!(msg.contains("pwsh.dll"), "{msg}");
        assert!(msg.contains("WinVerifyTrust"), "{msg}");
    }

    #[tokio::test]
    async fn cancelled_install_wait_keeps_guards_and_skips_continuation() {
        let sandbox = tempfile::tempdir().unwrap();
        let lock_path = sandbox.path().join("install.lock");
        let install_lock = acquire_install_lock(&lock_path).await.unwrap();
        let staging = tempfile::Builder::new()
            .prefix("staging-")
            .tempdir_in(sandbox.path())
            .unwrap();
        let staging_path = staging.path().to_owned();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let continued = Arc::new(AtomicBool::new(false));
        let continued_after_wait = Arc::clone(&continued);

        let caller = tokio::spawn(async move {
            run_install_task(staging, install_lock, move || {
                let _ = started_tx.send(());
                release_rx.recv().map_err(|err| {
                    std::io::Error::other(format!("install test gate closed: {err}"))
                })?;
                Ok(())
            })
            .await
            .unwrap();
            continued_after_wait.store(true, Ordering::SeqCst);
        });

        started_rx.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(staging_path.exists());

        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let lock_error = fs2::FileExt::try_lock_exclusive(&contender).unwrap_err();
        assert_eq!(
            lock_error.raw_os_error(),
            fs2::lock_contended_error().raw_os_error()
        );

        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match fs2::FileExt::try_lock_exclusive(&contender) {
                    Ok(()) => break,
                    Err(err)
                        if err.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(err) => panic!("retry install lock: {err}"),
                }
            }
        })
        .await
        .expect("detached install task should release its lock");
        fs2::FileExt::unlock(&contender).unwrap();

        assert!(!continued.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn local_artifact_installs_without_network() {
        let sandbox = tempfile::tempdir().unwrap();
        let archive = sandbox.path().join("fixture.zip");
        write_fixture_zip(&archive, b"signed fixture");
        let artifact = fixture_artifact(&archive);

        let executable = ensure_artifact(
            sandbox.path(),
            &artifact,
            ArtifactSource::Local(archive),
            false,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&executable).unwrap(), b"signed fixture");
        let record: InstallRecord = serde_json::from_slice(
            &std::fs::read(executable.parent().unwrap().join(INSTALL_RECORD_NAME)).unwrap(),
        )
        .unwrap();
        assert!(record.matches_artifact(&artifact));
        assert_eq!(record.files.len(), 10);
    }

    #[tokio::test]
    async fn corrupt_cache_is_replaced_from_injected_artifact() {
        let sandbox = tempfile::tempdir().unwrap();
        let archive = sandbox.path().join("fixture.zip");
        write_fixture_zip(&archive, b"fixture");
        let artifact = fixture_artifact(&archive);
        let source = ArtifactSource::Local(archive);

        let executable = ensure_artifact(sandbox.path(), &artifact, source.clone(), false)
            .await
            .unwrap();
        std::fs::write(&executable, []).unwrap();

        let repaired = ensure_artifact(sandbox.path(), &artifact, source, false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(repaired).unwrap(), b"fixture");
    }

    #[tokio::test]
    async fn missing_or_corrupt_runtime_dependency_rebuilds_the_cache() {
        let sandbox = tempfile::tempdir().unwrap();
        let archive = sandbox.path().join("fixture.zip");
        write_fixture_zip(&archive, b"fixture");
        let artifact = fixture_artifact(&archive);
        let source = ArtifactSource::Local(archive);

        let executable = ensure_artifact(sandbox.path(), &artifact, source.clone(), false)
            .await
            .unwrap();
        let dependency = executable.parent().unwrap().join("pwsh.dll");
        std::fs::remove_file(&dependency).unwrap();

        ensure_artifact(sandbox.path(), &artifact, source.clone(), false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dependency).unwrap(), FIXTURE_PWSH_DLL);

        std::fs::write(&dependency, vec![b'x'; FIXTURE_PWSH_DLL.len()]).unwrap();
        ensure_artifact(sandbox.path(), &artifact, source, false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(dependency).unwrap(), FIXTURE_PWSH_DLL);
    }

    #[tokio::test]
    async fn concurrent_installs_share_one_atomic_destination() {
        let sandbox = tempfile::tempdir().unwrap();
        let archive = sandbox.path().join("fixture.zip");
        write_fixture_zip(&archive, b"fixture");
        let artifact = fixture_artifact(&archive);
        let source = ArtifactSource::Local(archive);

        let first = ensure_artifact(sandbox.path(), &artifact, source.clone(), false);
        let second = ensure_artifact(sandbox.path(), &artifact, source, false);
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first.unwrap(), second.unwrap());
    }

    #[tokio::test]
    async fn unsigned_injected_executable_is_not_published_when_trust_is_required() {
        let sandbox = tempfile::tempdir().unwrap();
        let archive = sandbox.path().join("unsigned.zip");
        write_fixture_zip(&archive, b"not a signed PE image");
        let artifact = fixture_artifact(&archive);

        let error = ensure_artifact(
            sandbox.path(),
            &artifact,
            ArtifactSource::Local(archive),
            true,
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("failed post-extraction validation"),
            "{error}"
        );
        assert!(!install_dir(sandbox.path(), &artifact).exists());
    }

    #[tokio::test]
    async fn unavailable_injected_artifact_fails_without_partial_install() {
        let sandbox = tempfile::tempdir().unwrap();
        let template = sandbox.path().join("template.zip");
        write_fixture_zip(&template, b"fixture");
        let artifact = fixture_artifact(&template);
        std::fs::remove_file(&template).unwrap();

        let error = ensure_artifact(
            sandbox.path(),
            &artifact,
            ArtifactSource::Local(template),
            false,
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!install_dir(sandbox.path(), &artifact).exists());
    }

    #[tokio::test]
    async fn hash_mismatch_never_publishes_an_install() {
        let sandbox = tempfile::tempdir().unwrap();
        let archive = sandbox.path().join("fixture.zip");
        write_fixture_zip(&archive, b"fixture");
        let mut artifact = fixture_artifact(&archive);
        artifact.asset.sha256 = "00".repeat(32);

        let error = ensure_artifact(
            sandbox.path(),
            &artifact,
            ArtifactSource::Local(archive),
            false,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("SHA-256 mismatch"), "{error}");
        assert!(!install_dir(sandbox.path(), &artifact).exists());
    }

    #[tokio::test]
    async fn zip_traversal_is_rejected_without_writing_outside_staging() {
        let sandbox = tempfile::tempdir().unwrap();
        let archive = sandbox.path().join("traversal.zip");
        write_zip(
            &archive,
            &[
                ("../escape.txt", b"escape"),
                (POWERSHELL_EXECUTABLE, b"fixture"),
            ],
        );
        let artifact = fixture_artifact(&archive);

        let error = ensure_artifact(
            sandbox.path(),
            &artifact,
            ArtifactSource::Local(archive),
            false,
        )
        .await
        .unwrap_err();

        assert!(
            error.to_string().contains("escapes the extraction root"),
            "{error}"
        );
        assert!(!sandbox.path().join("escape.txt").exists());
        assert!(!install_dir(sandbox.path(), &artifact).exists());
    }
}
