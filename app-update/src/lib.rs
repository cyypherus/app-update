use semver::Version;
use sha2::{Digest, Sha256};
use std::future::Future;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::io::Cursor;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::{env, fmt, fs, path::Path};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use tempfile::TempDir;
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct UpdateConfig {
    current_version: Version,
    platform: Platform,
}

impl UpdateConfig {
    pub fn new(current_version: Version) -> Result<Self, UpdateConfigError> {
        Ok(Self::for_platform(current_version, Platform::current()?))
    }

    pub fn for_platform(current_version: Version, platform: Platform) -> Self {
        Self {
            current_version,
            platform,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacosArm,
    MacosIntel,
    WindowsX8664Gnu,
}

impl Platform {
    pub fn current() -> Result<Self, UpdateConfigError> {
        match (env::consts::OS, env::consts::ARCH) {
            ("macos", "aarch64") => Ok(Self::MacosArm),
            ("macos", _) => Ok(Self::MacosIntel),
            ("windows", _) => Ok(Self::WindowsX8664Gnu),
            (os, _) => Err(UpdateConfigError::UnsupportedOs(os)),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MacosArm => "macos-arm",
            Self::MacosIntel => "macos-intel",
            Self::WindowsX8664Gnu => "windows-x86_64-gnu",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub trait UpdateApi {
    type Error: std::error::Error + Send + Sync + 'static;

    fn latest_update(
        &self,
        platform: Platform,
    ) -> impl Future<Output = Result<Option<AvailableUpdate>, Self::Error>> + Send;

    fn download_update<'a>(
        &'a self,
        update: &'a AvailableUpdate,
        platform: Platform,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send + 'a;
}

#[derive(Clone, Debug)]
pub struct AppUpdater<A> {
    config: UpdateConfig,
    api: A,
}

impl<A> AppUpdater<A>
where
    A: UpdateApi,
{
    pub fn new(config: UpdateConfig, api: A) -> Self {
        Self { config, api }
    }

    pub async fn check(&self) -> Result<UpdateCheck, UpdateError<A::Error>> {
        let latest = self
            .api
            .latest_update(self.config.platform)
            .await
            .map_err(UpdateError::Api)?
            .ok_or(UpdateError::NoVersion {
                platform: self.config.platform,
            })?;

        if latest.version <= self.config.current_version {
            Ok(UpdateCheck::UpToDate {
                version: latest.version,
            })
        } else {
            Ok(UpdateCheck::Available(latest))
        }
    }

    pub async fn update(&self) -> Result<UpdateOutcome, UpdateError<A::Error>> {
        self.update_with_status(|_| std::future::ready(())).await
    }

    pub async fn update_with_status<F, Fut>(
        &self,
        report: F,
    ) -> Result<UpdateOutcome, UpdateError<A::Error>>
    where
        F: Fn(UpdateStatus) -> Fut,
        Fut: Future<Output = ()>,
    {
        report(UpdateStatus::Checking).await;

        match self.check().await? {
            UpdateCheck::UpToDate { version } => {
                report(UpdateStatus::UpToDate {
                    version: version.clone(),
                })
                .await;
                Ok(UpdateOutcome::UpToDate { version })
            }
            UpdateCheck::Available(update) => {
                let version = update.version.clone();
                report(UpdateStatus::Downloading {
                    version: version.clone(),
                })
                .await;
                let archive = self.download(&update).await?;
                report(UpdateStatus::Installing {
                    version: version.clone(),
                })
                .await;
                self.install(&archive).await?;
                report(UpdateStatus::Updated {
                    version: version.clone(),
                })
                .await;
                Ok(UpdateOutcome::Updated { version })
            }
        }
    }

    pub async fn download_and_install(
        &self,
        update: AvailableUpdate,
    ) -> Result<(), UpdateError<A::Error>> {
        let archive = self.download(&update).await?;
        self.install(&archive).await
    }

    async fn download(&self, update: &AvailableUpdate) -> Result<Vec<u8>, UpdateError<A::Error>> {
        let archive = self
            .api
            .download_update(update, self.config.platform)
            .await
            .map_err(UpdateError::Api)?;
        let actual: [u8; 32] = Sha256::digest(&archive).into();

        if actual != update.sha256 {
            return Err(UpdateError::ChecksumMismatch {
                version: update.version.clone(),
                platform: self.config.platform,
            });
        }

        Ok(archive)
    }

    async fn install(&self, archive: &[u8]) -> Result<(), UpdateError<A::Error>> {
        #[cfg(target_os = "windows")]
        {
            return self.install_windows(archive).await;
        }

        #[cfg(target_os = "macos")]
        {
            return self.install_macos(archive).await;
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let _ = archive;
            Err(UpdateError::UnsupportedOs(env::consts::OS))
        }
    }

    #[cfg(target_os = "windows")]
    async fn install_windows(&self, archive: &[u8]) -> Result<(), UpdateError<A::Error>> {
        let temp_dir = TempDir::new()?;
        extract_zip(archive, temp_dir.path())?;

        let new_exe = find_single_payload(temp_dir.path(), "exe").map_err(|error| match error {
            PayloadSelectionError::Io(error) => UpdateError::Io(error),
            PayloadSelectionError::Missing => UpdateError::MissingExecutable,
            PayloadSelectionError::Multiple => UpdateError::MultipleExecutables,
        })?;

        self_replace::self_replace(&new_exe)?;
        fs::remove_file(new_exe)?;

        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn install_macos(&self, archive: &[u8]) -> Result<(), UpdateError<A::Error>> {
        let current_exe = env::current_exe()?;
        let app_bundle = find_app_bundle(&current_exe)?;
        let temp_dir = TempDir::new()?;
        extract_zip(archive, temp_dir.path())?;

        let new_app_bundle =
            find_single_payload(temp_dir.path(), "app").map_err(|error| match error {
                PayloadSelectionError::Io(error) => UpdateError::Io(error),
                PayloadSelectionError::Missing => UpdateError::MissingMacosBundle,
                PayloadSelectionError::Multiple => UpdateError::MultipleMacosBundles,
            })?;

        let output = tokio::process::Command::new("rsync")
            .args(["-a", "--delete"])
            .arg(new_app_bundle.join("."))
            .arg(&app_bundle)
            .output()
            .await?;

        if !output.status.success() {
            return Err(UpdateError::InstallCommandFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(())
    }
}

pub async fn restart_application() -> Result<(), RestartError> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let current_exe = env::current_exe()?;

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new(&current_exe).spawn()?;
    }

    #[cfg(target_os = "macos")]
    {
        let app_bundle = restart_app_bundle(&current_exe)?;
        std::process::Command::new("open")
            .arg("-n")
            .arg(&app_bundle)
            .spawn()?;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Err(RestartError::UnsupportedOs(env::consts::OS))
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    std::process::exit(0);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateCheck {
    UpToDate { version: Version },
    Available(AvailableUpdate),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: Version,
    sha256: [u8; 32],
}

impl AvailableUpdate {
    pub fn new(version: Version, sha256: impl AsRef<str>) -> Result<Self, ChecksumError> {
        Ok(Self {
            version,
            sha256: parse_sha256(sha256.as_ref())?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    UpToDate { version: Version },
    Updated { version: Version },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    Checking,
    UpToDate { version: Version },
    Downloading { version: Version },
    Installing { version: Version },
    Updated { version: Version },
}

#[derive(Error, Debug)]
pub enum UpdateConfigError {
    #[error("unsupported OS: {0}")]
    UnsupportedOs(&'static str),
}

#[derive(Error, Debug)]
pub enum ChecksumError {
    #[error("invalid SHA-256 `{0}`")]
    InvalidSha256(String),
}

#[derive(Error, Debug)]
pub enum RestartError {
    #[error("unsupported OS: {0}")]
    UnsupportedOs(&'static str),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not find .app bundle")]
    MissingAppBundle,
}

#[derive(Error, Debug)]
pub enum UpdateError<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    #[error("update API failed: {0}")]
    Api(#[source] E),
    #[error("unsupported OS: {0}")]
    UnsupportedOs(&'static str),
    #[error("no version found for {platform}")]
    NoVersion { platform: Platform },
    #[error("checksum mismatch for {version} on {platform}")]
    ChecksumMismatch {
        version: Version,
        platform: Platform,
    },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("archive contains unsafe path: {0}")]
    UnsafeArchivePath(String),
    #[error("update archive does not contain an executable")]
    MissingExecutable,
    #[error("update archive contains multiple executables")]
    MultipleExecutables,
    #[error("could not find .app bundle")]
    MissingAppBundle,
    #[error("update archive does not contain an .app bundle")]
    MissingMacosBundle,
    #[error("update archive contains multiple .app bundles")]
    MultipleMacosBundles,
    #[error("install command failed: {0}")]
    InstallCommandFailed(String),
}

fn parse_sha256(sha256: &str) -> Result<[u8; 32], ChecksumError> {
    if sha256.len() != 64 {
        return Err(ChecksumError::InvalidSha256(sha256.to_string()));
    }

    let mut bytes = [0; 32];
    for i in 0..32 {
        let part = &sha256[i * 2..i * 2 + 2];
        bytes[i] = u8::from_str_radix(part, 16)
            .map_err(|_| ChecksumError::InvalidSha256(sha256.to_string()))?;
    }

    Ok(bytes)
}

#[derive(Debug)]
enum PayloadSelectionError {
    Io(std::io::Error),
    Missing,
    Multiple,
}

fn find_single_payload(
    extracted_archive: &Path,
    payload_extension: &str,
) -> Result<std::path::PathBuf, PayloadSelectionError> {
    let mut payload = None;

    for entry in fs::read_dir(extracted_archive).map_err(PayloadSelectionError::Io)? {
        let path = entry.map_err(PayloadSelectionError::Io)?.path();
        let matches = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(payload_extension))
            && match payload_extension {
                "app" => path.is_dir(),
                "exe" => path.is_file(),
                _ => false,
            };

        if matches {
            if payload.is_some() {
                return Err(PayloadSelectionError::Multiple);
            }
            payload = Some(path);
        }
    }

    payload.ok_or(PayloadSelectionError::Missing)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn extract_zip<E>(archive: &[u8], extract_to: &Path) -> Result<(), UpdateError<E>>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let reader = Cursor::new(archive);
    let mut archive = zip::ZipArchive::new(reader)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let enclosed_name = file
            .enclosed_name()
            .ok_or_else(|| UpdateError::UnsafeArchivePath(file.name().to_string()))?;
        let outpath = extract_to.join(enclosed_name);

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut outfile = fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    fs::set_permissions(&outpath, fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn app_bundle_path(exe_path: &Path) -> Option<PathBuf> {
    let mut current = exe_path;
    let mut levels = 0;

    while let Some(parent) = current.parent() {
        if levels >= 3 {
            break;
        }

        if parent.extension().and_then(|value| value.to_str()) == Some("app") {
            return Some(parent.to_path_buf());
        }

        current = parent;
        levels += 1;
    }

    None
}

#[cfg(target_os = "macos")]
fn find_app_bundle<E>(exe_path: &Path) -> Result<PathBuf, UpdateError<E>>
where
    E: std::error::Error + Send + Sync + 'static,
{
    app_bundle_path(exe_path).ok_or(UpdateError::MissingAppBundle)
}

#[cfg(target_os = "macos")]
fn restart_app_bundle(exe_path: &Path) -> Result<PathBuf, RestartError> {
    app_bundle_path(exe_path).ok_or(RestartError::MissingAppBundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct FakeApi {
        latest: Option<AvailableUpdate>,
        archive: Vec<u8>,
    }

    impl UpdateApi for FakeApi {
        type Error = std::io::Error;

        fn latest_update(
            &self,
            _platform: Platform,
        ) -> impl Future<Output = Result<Option<AvailableUpdate>, Self::Error>> + Send {
            std::future::ready(Ok(self.latest.clone()))
        }

        fn download_update<'a>(
            &'a self,
            _update: &'a AvailableUpdate,
            _platform: Platform,
        ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send + 'a {
            std::future::ready(Ok(self.archive.clone()))
        }
    }

    #[test]
    fn selects_macos_payload_without_installed_bundle_name() {
        let extracted_archive = tempfile::tempdir().unwrap();
        let downloaded_bundle = extracted_archive.path().join("Release.app");
        fs::create_dir(&downloaded_bundle).unwrap();

        let selected = find_single_payload(extracted_archive.path(), "app").unwrap();

        assert_eq!(selected, downloaded_bundle);
    }

    #[test]
    fn selects_windows_payload_without_installed_executable_name() {
        let extracted_archive = tempfile::tempdir().unwrap();
        let downloaded_executable = extracted_archive.path().join("release.exe");
        fs::write(&downloaded_executable, []).unwrap();

        let selected = find_single_payload(extracted_archive.path(), "exe").unwrap();

        assert_eq!(selected, downloaded_executable);
    }

    #[test]
    fn checksum_must_be_valid_sha256_hex() {
        let result = AvailableUpdate::new(Version::new(1, 0, 0), "bad");

        assert!(matches!(result, Err(ChecksumError::InvalidSha256(_))));
    }

    #[tokio::test]
    async fn check_reports_available_when_latest_is_newer() {
        let sha256 = format!("{:x}", Sha256::digest(b"archive"));
        let api = FakeApi {
            latest: Some(AvailableUpdate::new(Version::new(1, 1, 0), sha256).unwrap()),
            archive: Vec::new(),
        };
        let updater = AppUpdater::new(
            UpdateConfig::for_platform(Version::new(1, 0, 0), Platform::MacosArm),
            api,
        );

        let result = updater.check().await.unwrap();

        assert!(matches!(result, UpdateCheck::Available(_)));
    }

    #[tokio::test]
    async fn update_rejects_archive_with_wrong_checksum() {
        let sha256 = format!("{:x}", Sha256::digest(b"expected"));
        let api = FakeApi {
            latest: Some(AvailableUpdate::new(Version::new(1, 1, 0), sha256).unwrap()),
            archive: b"actual".to_vec(),
        };
        let updater = AppUpdater::new(
            UpdateConfig::for_platform(Version::new(1, 0, 0), Platform::MacosArm),
            api,
        );

        let result = updater.update().await;

        assert!(matches!(result, Err(UpdateError::ChecksumMismatch { .. })));
    }
}
