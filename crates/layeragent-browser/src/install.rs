//! Finding or downloading a Chromium for the agent to drive.
//!
//! Order: explicit `browser.executable`, a previously downloaded managed copy, a system
//! Chrome/Chromium/Edge/Brave, then (if allowed) a one-time download of Chrome for Testing
//! into the LayerAgent home. The download is HTTPS-only from Google's official host; the
//! archive's SHA-256 is recorded next to it for later comparison, since the host publishes
//! no checksums to verify against.
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const MANIFEST: &str = "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";
const ALLOWED_HOSTS: [&str; 2] = ["storage.googleapis.com", "edgedl.me.gvt1.com"];
const MAX_ARCHIVE: u64 = 600 * 1024 * 1024;
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;

/// Platform key used by Chrome for Testing, and the executable's path inside the archive.
fn platform() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("linux64", "chrome-linux64/chrome"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some((
            "mac-arm64",
            "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ))
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some((
            "mac-x64",
            "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(("win64", "chrome-win64/chrome.exe"))
    } else {
        None
    }
}

fn system_candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    #[cfg(target_os = "linux")]
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "brave-browser",
    ] {
        for dir in [
            "/usr/bin",
            "/usr/local/bin",
            "/snap/bin",
            "/opt/google/chrome",
        ] {
            found.push(Path::new(dir).join(name));
        }
    }
    #[cfg(target_os = "macos")]
    for app in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    ] {
        found.push(PathBuf::from(app));
    }
    #[cfg(target_os = "windows")]
    for var in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
        if let Ok(base) = std::env::var(var) {
            for rel in [
                "Google\\Chrome\\Application\\chrome.exe",
                "Microsoft\\Edge\\Application\\msedge.exe",
                "BraveSoftware\\Brave-Browser\\Application\\brave.exe",
            ] {
                found.push(Path::new(&base).join(rel));
            }
        }
    }
    found
}

pub fn find_system() -> Option<PathBuf> {
    system_candidates().into_iter().find(|p| p.is_file())
}

fn managed_root(home: &Path) -> PathBuf {
    home.join("browser")
}

/// Newest managed install, if any.
pub fn find_managed(home: &Path) -> Option<PathBuf> {
    let (_, relative) = platform()?;
    let mut versions: Vec<_> = std::fs::read_dir(managed_root(home))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(relative).is_file())
        .collect();
    versions.sort();
    versions.pop().map(|p| p.join(relative))
}

/// Locates a usable browser without downloading anything.
pub fn locate(home: &Path, explicit: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(path) = explicit {
        ensure!(
            path.is_file(),
            "browser.executable {} does not exist",
            path.display()
        );
        return Ok(Some(path.to_path_buf()));
    }
    if let Ok(path) = std::env::var("LAYERAGENT_BROWSER") {
        ensure!(
            Path::new(&path).is_file(),
            "LAYERAGENT_BROWSER {path} does not exist"
        );
        return Ok(Some(path.into()));
    }
    Ok(find_managed(home).or_else(find_system))
}

/// Locates a browser, downloading the managed one when permitted.
pub async fn ensure_browser(
    home: &Path,
    explicit: Option<&Path>,
    auto_install: bool,
) -> Result<PathBuf> {
    if let Some(path) = locate(home, explicit)? {
        return Ok(path);
    }
    ensure!(
        auto_install,
        "no Chrome/Chromium found; run `layeragent browser install` or set browser.executable"
    );
    install(home).await
}

fn check_download_url(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)?;
    ensure!(
        parsed.scheme() == "https" && parsed.username().is_empty() && parsed.password().is_none(),
        "browser download must use HTTPS without embedded credentials"
    );
    ensure!(
        parsed
            .host_str()
            .is_some_and(|h| ALLOWED_HOSTS.contains(&h)),
        "unexpected browser download host {:?}",
        parsed.host_str()
    );
    Ok(())
}

/// Downloads and unpacks the stable Chrome for Testing build for this platform.
pub async fn install(home: &Path) -> Result<PathBuf> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = LOCK.lock().await;
    let (key, relative) =
        platform().context("no managed browser build exists for this platform")?;
    if let Some(existing) = find_managed(home) {
        return Ok(existing);
    }
    let client = reqwest::Client::builder()
        // Validate exactly the manifest/download URLs we requested. Never follow
        // a redirect that could bypass the official-origin check.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(900))
        .build()?;
    let manifest: serde_json::Value = client
        .get(MANIFEST)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let stable = &manifest["channels"]["Stable"];
    let version = stable["version"]
        .as_str()
        .context("manifest has no stable version")?;
    ensure!(
        version.chars().all(|c| c.is_ascii_digit() || c == '.'),
        "unexpected version string"
    );
    let url = stable["downloads"]["chrome"]
        .as_array()
        .and_then(|list| list.iter().find(|d| d["platform"] == key))
        .and_then(|d| d["url"].as_str())
        .context("manifest has no download for this platform")?
        .to_string();
    check_download_url(&url)?;
    let root = managed_root(home);
    std::fs::create_dir_all(&root)?;
    let staging = root.join(format!(".download-{}", layeragent_protocol::new_id()));
    std::fs::create_dir(&staging)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))?;
    }
    let result = download_and_unpack(&client, &url, &staging, version).await;
    let target = root.join(version);
    match result {
        Ok(()) => {
            if target.exists() {
                std::fs::remove_dir_all(&target)?;
            }
            std::fs::rename(&staging, &target)?;
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    }
    let executable = target.join(relative);
    ensure!(
        executable.is_file(),
        "downloaded archive has no browser executable"
    );
    Ok(executable)
}

async fn download_and_unpack(
    client: &reqwest::Client,
    url: &str,
    dir: &Path,
    version: &str,
) -> Result<()> {
    let response = client.get(url).send().await?.error_for_status()?;
    ensure!(
        response.status().is_success(),
        "browser download redirected; refusing an unvalidated destination"
    );
    if let Some(len) = response.content_length() {
        ensure!(len <= MAX_ARCHIVE, "browser archive is unexpectedly large");
    }
    let archive = dir.join("browser.zip");
    let mut file = std::fs::File::create(&archive)?;
    let (mut hasher, mut total) = (Sha256::new(), 0u64);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        total += chunk.len() as u64;
        ensure!(
            total <= MAX_ARCHIVE,
            "browser archive exceeded the size limit"
        );
        hasher.update(&chunk);
        file.write_all(&chunk)?;
    }
    file.flush()?;
    drop(file);
    let digest = hex::encode(hasher.finalize());
    std::fs::write(
        dir.join("SOURCE"),
        format!("{url}\nversion {version}\nsha256 {digest}\n"),
    )?;
    let (archive_path, out) = (archive.clone(), dir.to_path_buf());
    tokio::task::spawn_blocking(move || unpack(&archive_path, &out)).await??;
    std::fs::remove_file(&archive)?;
    Ok(())
}

fn unpack(archive: &Path, out: &Path) -> Result<()> {
    unpack_with_limits(archive, out, MAX_ENTRIES, MAX_UNPACKED)
}

/// The staging directory is private to the installer. Reject both archive links
/// and any pre-existing link in the destination; never overwrite installer metadata.
fn extraction_directory(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("unsupported archive path component");
        };
        path.push(component);
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "archive directory collides with a file or symlink"
        );
    }
    Ok(())
}

fn unpack_with_limits(
    archive: &Path,
    out: &Path,
    max_entries: usize,
    max_bytes: u64,
) -> Result<()> {
    let metadata = std::fs::symlink_metadata(out)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "install staging root must be a real directory"
    );
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    ensure!(
        zip.len() <= max_entries,
        "browser archive contains too many entries"
    );
    let mut remaining = max_bytes;
    let mut destinations = HashSet::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let name = entry.name()?;
        ensure!(
            !name.contains(['\\', ':', '\0'])
                && !name.starts_with('/')
                && !name.split('/').any(|part| matches!(part, "." | ".."))
                && !name.chars().any(char::is_control),
            "archive contains a platform-dependent or non-normal path"
        );
        // Windows strips terminal dots/spaces and recognizes device names even
        // with extensions. Refuse these spellings consistently on every host.
        for part in name.split('/').filter(|part| !part.is_empty()) {
            let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            let device = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || ["COM", "LPT"].iter().any(|prefix| {
                    base.strip_prefix(prefix)
                        .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'))
                });
            ensure!(
                !part.ends_with(['.', ' ']) && !device,
                "archive contains an ambiguous Windows filename"
            );
        }
        let Some(relative) = entry.enclosed_name() else {
            bail!("archive entry escapes the install directory");
        };
        ensure!(
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "archive contains a non-normal relative path"
        );
        ensure!(
            destinations.insert(relative.clone()),
            "duplicate archive destination"
        );
        ensure!(!entry.is_symlink(), "archive symbolic links are forbidden");
        let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        ensure!(
            kind == 0 || kind == if entry.is_dir() { 0o040000 } else { 0o100000 },
            "archive special files are forbidden"
        );
        let path = out.join(&relative);
        if entry.is_dir() {
            extraction_directory(out, &relative)?;
            continue;
        }
        if let Some(parent) = relative.parent() {
            extraction_directory(out, parent)?;
        }
        let size = entry.size();
        ensure!(
            size <= remaining,
            "browser archive exceeds the expanded size limit"
        );
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&path)
            .context("archive destination already exists or is unsafe")?;
        let copied = std::io::copy(&mut (&mut entry).take(size.saturating_add(1)), &mut file)?;
        ensure!(
            copied == size,
            "browser archive entry has an inconsistent expanded size"
        );
        remaining -= copied;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode & 0o755))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_urls_are_pinned_to_https_and_known_hosts() {
        assert!(
            check_download_url("https://storage.googleapis.com/chrome-for-testing-public/1/x.zip")
                .is_ok()
        );
        assert!(check_download_url("http://storage.googleapis.com/x.zip").is_err());
        assert!(check_download_url("https://evil.example/x.zip").is_err());
        assert!(check_download_url("https://user:secret@storage.googleapis.com/x.zip").is_err());
        assert!(check_download_url("https://storage.googleapis.com.evil.example/x.zip").is_err());
    }

    #[test]
    fn explicit_and_managed_paths_win_over_discovery() -> Result<()> {
        let home = tempfile::tempdir()?;
        let fake = home.path().join("chrome");
        std::fs::write(&fake, "")?;
        assert_eq!(locate(home.path(), Some(&fake))?, Some(fake.clone()));
        assert!(locate(home.path(), Some(&home.path().join("missing"))).is_err());
        if let Some((_, relative)) = platform() {
            let managed = home.path().join("browser/150.0.0.0").join(relative);
            std::fs::create_dir_all(managed.parent().expect("has parent"))?;
            std::fs::write(&managed, "")?;
            assert_eq!(find_managed(home.path()), Some(managed));
        }
        Ok(())
    }

    #[test]
    fn unpacking_rejects_path_traversal() -> Result<()> {
        use std::io::Write as _;
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("a.zip");
        {
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive)?);
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("../escape.txt", options)?;
            writer.write_all(b"x")?;
            writer.finish()?;
        }
        let out = dir.path().join("out");
        std::fs::create_dir(&out)?;
        assert!(unpack(&archive, &out).is_err());
        assert!(!dir.path().join("escape.txt").exists());
        Ok(())
    }
    fn archive_with_file(archive: &Path, name: &str, body: &[u8]) -> Result<()> {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(archive)?);
        writer.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o755),
        )?;
        writer.write_all(body)?;
        writer.finish()?;
        Ok(())
    }

    #[test]
    fn legitimate_deflated_browser_tree_extracts_with_executable_permissions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("browser.zip");
        archive_with_file(&archive, "chrome-linux64/chrome", b"browser fixture")?;
        let out = dir.path().join("out");
        std::fs::create_dir(&out)?;
        unpack(&archive, &out)?;
        let executable = out.join("chrome-linux64/chrome");
        assert_eq!(std::fs::read(&executable)?, b"browser fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&executable)?.permissions().mode() & 0o7777,
                0o755
            );
        }
        Ok(())
    }

    #[test]
    fn archive_links_and_cross_platform_escape_names_are_rejected() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("browser.zip");
        let out = dir.path().join("out");
        std::fs::create_dir(&out)?;
        {
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive)?);
            writer.add_symlink(
                "chrome-linux64/link",
                "../../outside",
                zip::write::SimpleFileOptions::default(),
            )?;
            writer.finish()?;
        }
        assert!(unpack(&archive, &out).is_err());
        assert!(!out.join("chrome-linux64/link").exists());
        for name in [
            "../escape",
            "/absolute",
            "sub/../escape",
            "C:\\escape",
            "sub\\..\\escape",
            "normal:stream",
            "NUL.txt",
            "folder/COM1",
            "trailing.",
            "trailing ",
        ] {
            archive_with_file(&archive, name, b"forbidden")?;
            assert!(unpack(&archive, &out).is_err(), "accepted {name}");
        }
        assert!(!dir.path().join("escape").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlink_ancestors_and_file_overwrites_are_rejected() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("browser.zip");
        let out = dir.path().join("out");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&out)?;
        std::fs::create_dir(&outside)?;
        std::os::unix::fs::symlink(&outside, out.join("redirect"))?;
        archive_with_file(&archive, "redirect/escape", b"forbidden")?;
        assert!(unpack(&archive, &out).is_err());
        assert!(!outside.join("escape").exists());
        std::fs::write(out.join("SOURCE"), "trusted provenance")?;
        archive_with_file(&archive, "SOURCE", b"forged provenance")?;
        assert!(unpack(&archive, &out).is_err());
        assert_eq!(
            std::fs::read_to_string(out.join("SOURCE"))?,
            "trusted provenance"
        );
        let root_link = dir.path().join("root-link");
        std::os::unix::fs::symlink(&outside, &root_link)?;
        assert!(unpack(&archive, &root_link).is_err());
        Ok(())
    }

    #[test]
    fn archive_expansion_and_entry_counts_are_bounded() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("browser.zip");
        archive_with_file(&archive, "large", &[b'x'; 4096])?;
        let out = dir.path().join("out");
        std::fs::create_dir(&out)?;
        assert!(unpack_with_limits(&archive, &out, 10, 1024).is_err());
        assert!(!out.join("large").exists());
        assert!(unpack_with_limits(&archive, &out, 0, 8192).is_err());
        assert!(!out.join("large").exists());
        Ok(())
    }
}
