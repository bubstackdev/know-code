//! Daily public release metadata checks are isolated from query execution. Installation is always explicit.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use kc_store::limits::{self, Kind};
use semver::Version;
use serde::{Deserialize, Serialize};

/// The GitHub repository releases come from, `owner/name`: the one place the update check and `self-update` read it.
/// Cargo.toml's `repository` must match (checked by a test below).
macro_rules! release_repo { () => { "bubstackdev/knowcode" } }
const REPO: &str = release_repo!();
const API: &str = concat!("https://api.github.com/repos/", release_repo!(), "/releases/latest");
const DOWNLOAD: &str = concat!("https://github.com/", release_repo!(), "/releases/download");
const DAY: u64 = 24 * 60 * 60;
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Default, Serialize, Deserialize)]
struct State {
    format: u32,
    #[serde(default)] last_attempt: u64,
    #[serde(default)] last_success: u64,
    #[serde(default)] latest: Option<String>,
    #[serde(default)] notified_version: Option<String>,
    #[serde(default)] notified_at: u64,
}

struct Cache { dir: PathBuf, _lock: File }
impl Cache {
    fn open(wait: bool) -> Result<Option<Self>> {
        let dir = std::path::absolute(kc_index::cache_dir()?.join("updates"))?;
        let state = dir.join("state.json");
        let lock = dir.join("check.lock");
        for path in [&state, &lock] {
            if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) || kc_index::cache::durable_file(path)? {
                bail!("update cache overlaps a symlink or durable notes; retained {}", path.display());
            }
        }
        fs::create_dir_all(&dir)?;
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).mode(0o600).open(lock)?;
        let until = Instant::now() + Duration::from_secs(1);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { dir, _lock: file })),
                Err(TryLockError::WouldBlock) if wait && Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(e)) => return Err(e.into()),
            }
        }
    }
    fn read(&self) -> Result<State> {
        let path = self.dir.join("state.json");
        match fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State { format: 1, ..Default::default() }),
            Err(e) => return Err(e.into()),
            Ok(m) if m.len() > 64 * 1024 => bail!("unrecognized update cache; retained {}", path.display()),
            _ => {},
        }
        let state: State = serde_json::from_slice(&fs::read(&path)?).context("unrecognized update cache; retained")?;
        if state.format != 1 { bail!("unrecognized update cache format; retained"); }
        Ok(state)
    }
    fn write(&self, state: &State) -> Result<()> {
        let path = self.dir.join("state.json");
        let tmp = self.dir.join(format!("state.{}.tmp", std::process::id()));
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new().create_new(true).write(true).mode(0o600).open(&tmp)?;
        let result = (|| -> Result<()> {
            file.write_all(&serde_json::to_vec(state)?)?;
            fs::rename(&tmp, path)?;
            Ok(())
        })();
        if result.is_err() { let _ = fs::remove_file(tmp); }
        result
    }
}

fn owner() -> &'static str { REPO.split_once('/').map_or(REPO, |(owner, _)| owner) }
fn repo_name() -> &'static str { REPO.split_once('/').map_or(REPO, |(_, name)| name) }

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn due(at: u64, clock: u64) -> bool { at == 0 || clock.saturating_sub(at) >= DAY || at > clock.saturating_add(DAY) }
fn version(tag: &str) -> Result<Version> { Ok(Version::parse(tag.strip_prefix('v').unwrap_or(tag))?) }
fn newer(tag: &str) -> bool { version(tag).is_ok_and(|v| v.pre.is_empty() && v.cmp_precedence(&Version::parse(CURRENT).expect("package SemVer")).is_gt()) }

/// Best effort, no waiting for network or another process. A helper outlives a short CLI command only long enough
/// to fetch bounded public metadata; it does not index repositories or run as a resident service.
pub fn start() {
    let _ = schedule();
}
fn schedule() -> Result<()> {
    if !kc_index::config::update_check()?.0 { return Ok(()); }
    let Some(cache) = Cache::open(false)? else { return Ok(()) };
    let mut state = cache.read()?;
    let clock = now();
    let mut notified = false;
    if let Some(tag) = state.latest.as_ref().filter(|tag| newer(tag))
        && (state.notified_version.as_ref() != Some(tag) || due(state.notified_at, clock))
    {
        eprintln!("knowcode: {tag} is available; run `knowcode self-update`. Disable automatic checks with `knowcode config set update.check false`.");
        state.notified_version = Some(tag.clone());
        state.notified_at = clock;
        notified = true;
    }
    if !due(state.last_attempt, clock) {
        if notified { cache.write(&state)?; }
        return Ok(());
    }
    state.last_attempt = clock;
    cache.write(&state)?;
    let mut child = Command::new(std::env::current_exe()?);
    child.arg("_check-update").current_dir(&cache.dir)
        .env("KNOWCODE_CACHE_DIR", std::path::absolute(kc_index::cache_dir()?)?)
        .env("KNOWCODE_CONFIG_DIR", std::path::absolute(kc_index::config_dir()?)?)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // The state was committed under the cross-process lock before spawning: concurrent clients schedule once/day.
    let mut child = child.spawn().context("starting release check")?;
    std::thread::spawn(move || { let _ = child.wait(); });
    Ok(())
}

struct Scratch { path: PathBuf, keep: bool }
impl Scratch {
    fn new(parent: &Path) -> Result<Self> {
        fs::create_dir_all(parent)?;
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = parent.join(format!(".knowcode-update-{}-{unique}", std::process::id()));
        fs::create_dir(&path)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(Self { path, keep: false })
    }
}
impl Drop for Scratch { fn drop(&mut self) { if !self.keep { let _ = fs::remove_dir_all(&self.path); } } }

fn download(url: &str, destination: &Path, install: bool) -> Result<()> {
    let mut cmd = Command::new("curl");
    // --disable must be first: user curl configuration must not add credentials, bodies, or unrelated requests.
    cmd.args(["--disable", "--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--proto-redir", "=https",
        "--connect-timeout", "2", "--max-time", if install { "120" } else { "5" }, "--max-filesize", "2097152", "--user-agent"])
        .arg(format!("knowcode/{CURRENT}"))
        .args(["--header", "Accept: application/vnd.github+json", "--output"]).arg(destination).arg(url)
        .current_dir(destination.parent().context("download has no directory")?);
    let out = limits::output(cmd, if install { Kind::UpdateInstall } else { Kind::UpdateCheck }, "public release download", None)
        .context("running curl for the public release (curl is required)")?;
    if !out.status.success() { bail!("public release download failed: {}", String::from_utf8_lossy(&out.stderr).trim()); }
    if fs::metadata(destination)?.len() > 2 * 1024 * 1024 { bail!("release response exceeds the size limit"); }
    Ok(())
}

struct Release { tag: String, version: Version, installer: String, digest: String }
fn parse_release(bytes: &[u8]) -> Result<Release> {
    let data: serde_json::Value = serde_json::from_slice(bytes).context("invalid public release metadata")?;
    if data["draft"] != false || data["prerelease"] != false { bail!("latest release is not a published stable release"); }
    let tag = data["tag_name"].as_str().context("release has no tag")?;
    let v = version(tag).context("release tag is not SemVer")?;
    if !v.pre.is_empty() { bail!("latest release is a prerelease"); }
    let expected = format!("{DOWNLOAD}/{tag}/knowcode-installer.sh");
    let asset = data["assets"].as_array().context("release has no assets")?.iter()
        .find(|a| a["name"] == "knowcode-installer.sh").context("latest release has no shell installer yet")?;
    let url = asset["browser_download_url"].as_str().context("installer has no download URL")?;
    if url != expected && url != expected.replace('+', "%2B") { bail!("installer URL is outside the expected KnowCode release"); }
    let digest = asset["digest"].as_str().and_then(|s| s.strip_prefix("sha256:")).context("installer has no GitHub SHA-256 digest")?;
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) { bail!("invalid installer digest"); }
    Ok(Release { tag: tag.into(), version: v, installer: url.into(), digest: digest.to_ascii_lowercase() })
}
fn fetch(scratch: &Scratch) -> Result<Release> {
    let json = scratch.path.join("release.json");
    download(API, &json, false)?;
    parse_release(&fs::read(json)?)
}

/// Hidden one-shot helper. Failures are quiet to the caller and cannot mark a repository answer incomplete.
pub fn probe() -> Result<()> {
    if !kc_index::config::update_check()?.0 { return Ok(()); }
    let dir = std::path::absolute(kc_index::cache_dir()?.join("updates"))?;
    let scratch = Scratch::new(&dir)?;
    let release = fetch(&scratch)?;
    if !kc_index::config::update_check()?.0 { return Ok(()); }
    let Some(cache) = Cache::open(true)? else { return Ok(()) };
    let mut state = cache.read()?;
    state.latest = Some(release.tag);
    state.last_success = now();
    cache.write(&state)
}

fn receipt_path() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        Some(dir) => PathBuf::from(dir), None => dirs::home_dir().context("no home for installer receipt")?.join(".config"),
    };
    Ok(dir.join("knowcode/knowcode-receipt.json"))
}
fn installation() -> Result<(PathBuf, PathBuf, String)> {
    let exe = std::env::current_exe()?;
    let receipt = receipt_path()?;
    if fs::symlink_metadata(&receipt).is_ok_and(|m| m.is_symlink()) { bail!("installer receipt is symlinked; reinstall with the shell installer"); }
    let text = fs::read_to_string(&receipt).context("no matching shell-install receipt; update through your package manager or re-run the shell installer")?;
    let data: serde_json::Value = serde_json::from_str(&text)?;
    if data.pointer("/provider/source").and_then(|s| s.as_str()) != Some("cargo-dist")
        || data.pointer("/source/owner").and_then(|s| s.as_str()) != Some(owner())
        || data.pointer("/source/name").and_then(|s| s.as_str()) != Some(repo_name())
        || data.pointer("/source/app_name").and_then(|s| s.as_str()) != Some("knowcode")
        || data["install_layout"] != "flat"
    { bail!("unrecognized shell-install receipt; update through the original installer"); }
    let prefix = PathBuf::from(data["install_prefix"].as_str().context("receipt has no installation prefix")?);
    let binary = prefix.join("knowcode");
    if !prefix.is_absolute() || fs::symlink_metadata(&binary).is_ok_and(|m| m.is_symlink())
        || fs::canonicalize(&binary).ok() != fs::canonicalize(&exe).ok()
    { bail!("receipt belongs to another binary or a symlink; update through the original installer"); }
    Ok((prefix, receipt, text))
}
fn verify(path: &Path, expected: &str) -> Result<()> {
    let mut sha = Command::new("sha256sum");
    sha.arg(path);
    let out = match limits::output(sha, Kind::UpdateCheck, "installer checksum", None) {
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {
            let mut sha = Command::new("shasum"); sha.args(["-a", "256"]).arg(path);
            limits::output(sha, Kind::UpdateCheck, "installer checksum", None)?
        },
        other => other?,
    };
    if !out.status.success() || String::from_utf8_lossy(&out.stdout).split_whitespace().next() != Some(expected) { bail!("installer checksum mismatch; nothing installed"); }
    Ok(())
}

pub fn self_update(check_only: bool) -> Result<()> {
    let installation = if check_only { None } else { Some(installation()?) };
    let scratch = Scratch::new(&kc_index::cache_dir()?.join("updates"))?;
    let release = fetch(&scratch)?;
    if !newer(&release.tag) {
        println!("KnowCode {CURRENT} is current (latest stable release: {}).", release.version);
        return Ok(());
    }
    if check_only { println!("KnowCode {} is available (installed: {CURRENT}); run `knowcode self-update` or upgrade through your package manager.", release.version); return Ok(()); }
    let (prefix, receipt, receipt_before) = installation.expect("checked above");
    let Some(_cache) = Cache::open(false)? else { bail!("another update check is running; retry shortly"); };
    let installer = scratch.path.join("installer.sh");
    eprintln!("knowcode: downloading {}", release.tag);
    download(&release.installer, &installer, true)?;
    verify(&installer, &release.digest)?;
    let mut backup = Scratch::new(&prefix)?;
    let old = backup.path.join("knowcode");
    fs::copy(prefix.join("knowcode"), &old)?;
    let install = (|| -> Result<()> {
        eprintln!("knowcode: installing {}", release.tag);
        let mut cmd = Command::new("sh");
        cmd.arg(&installer).current_dir(&scratch.path).env("KNOWCODE_INSTALL_DIR", &prefix)
            .env("KNOWCODE_NO_MODIFY_PATH", "1").env("KNOWCODE_DISABLE_UPDATE", "1");
        for key in ["KNOWCODE_DOWNLOAD_URL", "INSTALLER_DOWNLOAD_URL", "KNOWCODE_INSTALLER_GHE_BASE_URL", "KNOWCODE_INSTALLER_GITHUB_BASE_URL", "KNOWCODE_GITHUB_TOKEN", "KNOWCODE_UNMANAGED_INSTALL"] { cmd.env_remove(key); }
        let out = limits::output(cmd, Kind::UpdateInstall, "KnowCode installer", None)?;
        if !out.status.success() { bail!("installer failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)); }
        let mut cmd = Command::new(prefix.join("knowcode")); cmd.arg("--version");
        let out = limits::output(cmd, Kind::UpdateCheck, "installed version check", None)?;
        if !out.status.success() || String::from_utf8_lossy(&out.stdout).trim() != format!("knowcode {}", release.version) { bail!("installed binary did not report the expected version"); }
        Ok(())
    })();
    if let Err(error) = install {
        if let Err(rollback) = fs::rename(&old, prefix.join("knowcode")) {
            backup.keep = true;
            return Err(anyhow!("{error:#}; rollback failed: {rollback}; previous binary retained at {}", old.display()));
        }
        fs::write(&receipt, receipt_before).context("previous binary restored, but installer receipt could not be restored")?;
        return Err(error.context("previous binary and installer receipt restored"));
    }
    println!("Installed KnowCode {}. Start a new client session to use it.", release.version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_repository_is_named_once_and_matches_the_package() {
        assert_eq!(env!("CARGO_PKG_REPOSITORY"), format!("https://github.com/{REPO}"));
        assert_eq!((owner(), repo_name()), ("bubstackdev", "knowcode"));
    }

    #[test]
    fn only_newer_stable_precedence_not_build_metadata_triggers_a_notice() {
        assert!(!newer("v0.1.0"));
        assert!(!newer("0.1.0+other-build"));
        assert!(!newer("v0.2.0-rc.1"));
        assert!(!newer("not-a-version"));
        assert!(newer("v0.1.1"));
        assert!(newer("0.2.0+build"));
    }

    #[test]
    fn checks_wait_a_day_and_recover_from_a_large_clock_rollback() {
        assert!(due(0, 10));
        assert!(!due(100, 100 + DAY - 1));
        assert!(due(100, 100 + DAY));
        assert!(!due(101, 100));
        assert!(due(100 + DAY + 1, 100));
    }

    #[test]
    fn metadata_requires_stable_published_release_and_its_own_checksummed_installer() {
        let valid = serde_json::json!({"tag_name":"v0.2.0", "draft":false, "prerelease":false,
            "assets":[{"name":"knowcode-installer.sh", "browser_download_url":format!("{DOWNLOAD}/v0.2.0/knowcode-installer.sh"),
                "digest":format!("sha256:{}", "ab".repeat(32))}]});
        let parse = |data: &serde_json::Value| parse_release(&serde_json::to_vec(data).unwrap());
        assert!(parse(&valid).is_ok());
        for (key, value) in [("draft", serde_json::json!(true)), ("prerelease", serde_json::json!(true)),
            ("tag_name", serde_json::json!("v0.2.0-rc.1"))] {
            let mut bad = valid.clone();
            bad[key] = value;
            assert!(parse(&bad).is_err());
        }
        for url in ["https://example.com/installer.sh".to_string(), format!("{DOWNLOAD}/v0.3.0/knowcode-installer.sh")] {
            let mut bad = valid.clone();
            bad["assets"][0]["browser_download_url"] = url.as_str().into();
            assert!(parse(&bad).is_err());
        }
        let mut bad = valid;
        bad["assets"][0].as_object_mut().unwrap().remove("digest");
        assert!(parse(&bad).is_err());
    }
}
