//! Zed extension for the `skills` CLI (https://github.com/roxblnfk/zed-skills).
//!
//! Registers the `Skills JSON` / `Skill Markdown` languages and launches
//! `skills lsp` as the language server for them.
//!
//! Binary resolution order (deliberate — see README):
//! 1. explicit user override in Zed settings (`lsp.skills.binary.path`),
//! 2. the binary this extension previously downloaded,
//! 3. a fresh download from the GitHub release,
//! 4. `skills` found on PATH — LAST resort only, because an unrelated tool
//!    with the same name (the PHP `llm/skills` plugin) may shadow ours and
//!    it has no `lsp` subcommand.

use std::fs;

use zed_extension_api::settings::LspSettings;
use zed_extension_api::{self as zed, LanguageServerId, Result};

const GITHUB_REPO: &str = "roxblnfk/zed-skills";
const BINARY_NAME: &str = "skills";

struct SkillsBinary {
    path: String,
    args: Option<Vec<String>>,
    env: Vec<(String, String)>,
}

/// Where a given release lands on disk, relative to the extension work dir.
struct ReleaseLayout {
    asset_name: String,
    file_type: zed::DownloadedFileType,
    /// Versioned directory the archive is extracted into.
    version_dir: String,
    /// Path to the binary inside `version_dir`.
    binary_path: String,
}

impl ReleaseLayout {
    /// Maps (os, arch) to the release assets published on the GitHub release.
    /// Returns `None` for platforms without a prebuilt asset
    /// (x86_64 macOS, aarch64 Windows, 32-bit x86).
    fn new(os: zed::Os, arch: zed::Architecture, version: &str) -> Option<Self> {
        let triple = match (os, arch) {
            (zed::Os::Windows, zed::Architecture::X8664) => "x86_64-pc-windows-msvc",
            (zed::Os::Mac, zed::Architecture::Aarch64) => "aarch64-apple-darwin",
            (zed::Os::Linux, zed::Architecture::X8664) => "x86_64-unknown-linux-gnu",
            (zed::Os::Linux, zed::Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
            _ => return None,
        };
        let stem = format!("{BINARY_NAME}-{triple}");
        let (suffix, file_type) = match os {
            zed::Os::Windows => ("zip", zed::DownloadedFileType::Zip),
            zed::Os::Mac | zed::Os::Linux => ("tar.gz", zed::DownloadedFileType::GzipTar),
        };
        let exe = match os {
            zed::Os::Windows => format!("{BINARY_NAME}.exe"),
            zed::Os::Mac | zed::Os::Linux => BINARY_NAME.to_string(),
        };
        let version_dir = format!("{BINARY_NAME}-{version}");
        Some(Self {
            asset_name: format!("{stem}.{suffix}"),
            file_type,
            // Archives contain a `<stem>/` top-level directory.
            binary_path: format!("{version_dir}/{stem}/{exe}"),
            version_dir,
        })
    }
}

struct SkillsExtension {
    cached_binary_path: Option<String>,
}

impl SkillsExtension {
    fn language_server_binary(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<SkillsBinary> {
        let binary_settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|lsp_settings| lsp_settings.binary);
        let args = binary_settings
            .as_ref()
            .and_then(|settings| settings.arguments.clone());
        let env: Vec<(String, String)> = binary_settings
            .as_ref()
            .and_then(|settings| settings.env.clone())
            .map(|env| env.into_iter().collect())
            .unwrap_or_default();

        // 1. Explicit user override always wins.
        if let Some(path) = binary_settings.and_then(|settings| settings.path) {
            return Ok(SkillsBinary { path, args, env });
        }

        // 2. Binary resolved earlier in this session.
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(SkillsBinary {
                    path: path.clone(),
                    args,
                    env,
                });
            }
        }

        // 3. Our own downloaded release binary. PATH is deliberately NOT
        //    consulted before this point: another tool named `skills` (the
        //    PHP llm/skills plugin) may be installed and it has no `lsp`
        //    subcommand.
        match self.download_release_binary(language_server_id) {
            Ok(path) => {
                self.cached_binary_path = Some(path.clone());
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::None,
                );
                Ok(SkillsBinary { path, args, env })
            }
            Err(download_error) => {
                // A download from a previous session may still be on disk
                // (e.g. we are offline and the release lookup failed).
                if let Some(path) = self.find_existing_download() {
                    self.cached_binary_path = Some(path.clone());
                    zed::set_language_server_installation_status(
                        language_server_id,
                        &zed::LanguageServerInstallationStatus::None,
                    );
                    return Ok(SkillsBinary { path, args, env });
                }

                // 4. Last resort: PATH. Only reached when there is no release
                //    asset for this platform or the download failed.
                if let Some(path) = worktree.which(BINARY_NAME) {
                    return Ok(SkillsBinary { path, args, env });
                }

                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::Failed(download_error.clone()),
                );
                Err(format!(
                    "{download_error}. Install the `skills` CLI \
                     (https://github.com/roxblnfk/zed-skills) into PATH, or point Zed at a \
                     binary via settings: {{\"lsp\": {{\"skills\": {{\"binary\": {{\"path\": \
                     \"/path/to/skills\"}}}}}}}}"
                ))
            }
        }
    }

    /// Downloads (if needed) the latest stable release binary into a versioned
    /// directory under the extension work dir, pruning older versions.
    fn download_release_binary(&self, language_server_id: &LanguageServerId) -> Result<String> {
        let (os, arch) = zed::current_platform();

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );
        let release = zed::latest_github_release(
            GITHUB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let layout = ReleaseLayout::new(os, arch, &release.version).ok_or_else(|| {
            format!(
                "no prebuilt `skills` binary for this platform in release {}",
                release.version
            )
        })?;

        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == layout.asset_name)
            .ok_or_else(|| {
                format!(
                    "release {} has no asset named {}",
                    release.version, layout.asset_name
                )
            })?;

        if !fs::metadata(&layout.binary_path).is_ok_and(|stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
            zed::download_file(&asset.download_url, &layout.version_dir, layout.file_type)
                .map_err(|error| format!("failed to download {}: {error}", layout.asset_name))?;

            zed::make_file_executable(&layout.binary_path)?;

            // Prune older downloaded versions.
            if let Ok(entries) = fs::read_dir(".") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if name.starts_with(&format!("{BINARY_NAME}-")) && name != layout.version_dir {
                        fs::remove_dir_all(entry.path()).ok();
                    }
                }
            }
        }

        Ok(layout.binary_path)
    }

    /// Looks for a binary left behind by a previous session's download
    /// (highest version directory wins).
    fn find_existing_download(&self) -> Option<String> {
        let (os, arch) = zed::current_platform();
        let mut versions: Vec<String> = fs::read_dir(".")
            .ok()?
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter_map(|name| {
                name.strip_prefix(&format!("{BINARY_NAME}-"))
                    .map(str::to_string)
            })
            .collect();
        versions.sort();
        for version in versions.into_iter().rev() {
            if let Some(layout) = ReleaseLayout::new(os, arch, &version) {
                if fs::metadata(&layout.binary_path).is_ok_and(|stat| stat.is_file()) {
                    return Some(layout.binary_path);
                }
            }
        }
        None
    }
}

impl zed::Extension for SkillsExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary = self.language_server_binary(language_server_id, worktree)?;
        Ok(zed::Command {
            command: binary.path,
            args: binary.args.unwrap_or_else(|| vec!["lsp".into()]),
            env: binary.env,
        })
    }
}

zed::register_extension!(SkillsExtension);
