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
macro_rules! release_repo { () => { "bubstackdev/know-code" } }
const REPO: &str = release_repo!();
/// The repository's name before it moved (2026-10-27): receipts of installs made then name it, and GitHub redirects it.
const FORMER_NAME: &str = "knowcode";
const API: &str = concat!("https://api.github.com/repos/", release_repo!(), "/releases/latest");
const DOWNLOAD: &str = concat!("https://github.com/", release_repo!(), "/releases/download");
/// The release pages. `latest` redirects to `tag/<tag>`; it is not the API, so the API's rate limit does not apply.
const PAGES: &str = concat!("https://github.com/", release_repo!(), "/releases");
/// This build's release archive (dist-workspace.toml's `targets`, checked by a test below), used when the API refuses.
const TARGET: Option<&str> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) { Some("aarch64-apple-darwin") }
    else if cfg!(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64")) { Some("x86_64-unknown-linux-gnu") }
    else if cfg!(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64")) { Some("aarch64-unknown-linux-gnu") }
    else { None };
const METADATA_LIMIT: u64 = 2 * 1024 * 1024;
const ARCHIVE_LIMIT: u64 = 64 * 1024 * 1024;
/// The Homebrew tap releases publish the formula to (dist-workspace.toml's `tap`, checked by a test below), and the
/// formula's name. Homebrew names a `<owner>/homebrew-<name>` repository `<owner>/<name>`.
macro_rules! tap_repo { () => { "bubstackdev/homebrew-tap" } }
const FORMULA: &str = "knowcode";
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
    /// When GitHub's API rate limit resets after it refused a request; until then checks read the release page.
    #[serde(default)] api_retry_at: u64,
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

/// The command that upgrades a Homebrew install: `brew upgrade bubstackdev/tap/knowcode`.
fn brew_upgrade() -> String {
    let (owner, name) = tap_repo!().split_once('/').expect("owner/name");
    format!("brew upgrade {owner}/{}/{FORMULA}", name.strip_prefix("homebrew-").unwrap_or(name))
}

/// Whether `exe` runs from a Homebrew keg: Homebrew writes INSTALL_RECEIPT.json at the root of each keg
/// (`Cellar/knowcode/<version>/`), above `bin/knowcode`. The `bin` symlink Homebrew links is resolved first.
fn homebrew_keg(exe: &Path) -> bool {
    let exe = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    exe.ancestors().skip(1).any(|dir| dir.join("INSTALL_RECEIPT.json").is_file())
}
fn homebrew() -> bool { std::env::current_exe().is_ok_and(|exe| homebrew_keg(&exe)) }

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn due(at: u64, clock: u64) -> bool { at == 0 || clock.saturating_sub(at) >= DAY || at > clock.saturating_add(DAY) }
fn version(tag: &str) -> Result<Version> { Ok(Version::parse(tag.strip_prefix('v').unwrap_or(tag))?) }
fn newer(tag: &str) -> bool { newer_than(tag, CURRENT) }
/// Whether release `tag` is one to offer a build at version `current`: a stable release of higher precedence. A
/// development build (`0.7.0-dev`) ranks below its release, so that release is offered, and above the ones before.
fn newer_than(tag: &str, current: &str) -> bool { version(tag).is_ok_and(|v| v.pre.is_empty() && v.cmp_precedence(&Version::parse(current).expect("package SemVer")).is_gt()) }

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
        let how = if homebrew() { brew_upgrade() } else { "knowcode self-update".into() };
        eprintln!("knowcode: {tag} is available; run `{how}`. Disable automatic checks with `knowcode config set update.check false`.");
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

/// One request. Redirects are followed only when `extra` asks with `--location`.
fn curl(url: &str, destination: &Path, install: bool, limit: u64, extra: &[&std::ffi::OsStr]) -> Result<()> {
    let mut cmd = Command::new("curl");
    // --disable must be first: user curl configuration must not add credentials, bodies, or unrelated requests.
    cmd.args(["--disable", "--fail", "--silent", "--show-error", "--proto", "=https", "--proto-redir", "=https",
        "--connect-timeout", "2", "--max-time", if install { "120" } else { "5" }, "--max-filesize"]).arg(limit.to_string())
        .arg("--user-agent").arg(format!("knowcode/{CURRENT}"))
        .args(extra).arg("--output").arg(destination).arg(url)
        .current_dir(destination.parent().context("download has no directory")?);
    let out = limits::output(cmd, if install { Kind::UpdateInstall } else { Kind::UpdateCheck }, "public release download", None)
        .context("running curl for the public release (curl is required)")?;
    if !out.status.success() { bail!("public release download failed: {}", String::from_utf8_lossy(&out.stderr).trim()); }
    // A redirect with no body leaves no file.
    if fs::metadata(destination).map_or(0, |m| m.len()) > limit { bail!("release response exceeds the size limit"); }
    Ok(())
}
fn download(url: &str, destination: &Path, install: bool) -> Result<()> {
    curl(url, destination, install, METADATA_LIMIT, &["--location".as_ref(), "--header".as_ref(), "Accept: application/vnd.github+json".as_ref()])
}

/// The status and header fields (names lowercased) of the last response in a curl `--dump-header` file.
fn response(text: &str) -> Option<(u16, Vec<(String, String)>)> {
    let mut last = None;
    for line in text.lines().map(str::trim_end) {
        if line.starts_with("HTTP/") {
            last = Some((line.split_whitespace().nth(1)?.parse().ok()?, Vec::new()));
        } else if let (Some((_, fields)), Some((name, value))) = (last.as_mut(), line.split_once(':')) {
            let fields: &mut Vec<(String, String)> = fields;
            fields.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    last
}
fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

/// GitHub's API declining a request: its status, whether a rate limit is why, and when to ask again if it said.
#[derive(Debug, PartialEq)]
struct Refusal { status: u16, limited: bool, reset: Option<u64> }
fn refusal(headers: &str, clock: u64) -> Option<Refusal> {
    let (status, fields) = response(headers)?;
    let spent = field(&fields, "x-ratelimit-remaining") == Some("0");
    if !(status == 403 || status == 429 || (status >= 400 && spent)) { return None; }
    let retry = field(&fields, "retry-after").and_then(|s| s.parse::<u64>().ok()).map(|s| clock.saturating_add(s));
    let reset = field(&fields, "x-ratelimit-reset").and_then(|s| s.parse::<u64>().ok());
    Some(Refusal { status, limited: spent || status == 429 || retry.is_some(), reset: retry.into_iter().chain(reset).max() })
}
impl Refusal {
    fn explain(&self) -> String {
        let when = self.reset.map_or_else(|| "later".to_string(), |at| format!("after {}", local_time(at)));
        if self.limited { format!("GitHub's API rate limit for this network is used up; try again {when}, or set KNOWCODE_GITHUB_TOKEN") }
        else { format!("GitHub's API refused the release check (HTTP {}); try again {when}, or set KNOWCODE_GITHUB_TOKEN", self.status) }
    }
}
/// `HH:MM` in local time.
fn local_time(at: u64) -> String {
    let time = at.min(i64::MAX as u64) as libc::time_t;
    // SAFETY: localtime_r writes only into the `tm` it is given; a null return leaves it unused.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() { return format!("{at} (Unix time)"); }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

struct Release { tag: String, version: Version, installer: Option<Installer> }
/// The shell installer and GitHub's digest of it, from the API. A release read from the release page has none:
/// its archive and the archive's published checksum are used instead.
struct Installer { url: String, digest: String }
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
    Ok(Release { tag: tag.into(), version: v, installer: Some(Installer { url: url.into(), digest: digest.to_ascii_lowercase() }) })
}
/// The tag `releases/latest` redirects to: only `<PAGES>/tag/<stable SemVer tag>`.
fn latest_tag(headers: &str) -> Result<String> {
    let (status, fields) = response(headers).context("the release page sent no response")?;
    if !(300..400).contains(&status) { bail!("the latest release page did not redirect to a release (HTTP {status})"); }
    let location = field(&fields, "location").context("the latest release page redirected nowhere")?;
    let tag = location.strip_prefix(PAGES).and_then(|rest| rest.strip_prefix("/tag/"))
        .context("the latest release page redirected outside KnowCode's releases")?;
    if tag.is_empty() || !tag.bytes().all(|c| c.is_ascii_alphanumeric() || b".+-_".contains(&c)) { bail!("unrecognized release tag"); }
    if !version(tag).context("release tag is not SemVer")?.pre.is_empty() { bail!("latest release is a prerelease"); }
    Ok(tag.into())
}

enum Answer { Release(Release), Refused(Refusal) }
fn ask_api(scratch: &Scratch) -> Result<Answer> {
    let json = scratch.path.join("release.json");
    let headers = scratch.path.join("release.headers");
    let mut extra: Vec<std::ffi::OsString> = ["--location", "--header", "Accept: application/vnd.github+json", "--dump-header"]
        .map(Into::into).into();
    extra.push(headers.clone().into());
    if let Some(token) = std::env::var("KNOWCODE_GITHUB_TOKEN").ok().filter(|t| !t.trim().is_empty()) {
        if token.chars().any(char::is_control) { bail!("KNOWCODE_GITHUB_TOKEN contains a control character"); }
        // In a private file, not in curl's arguments, which other local users can list. curl drops the header on a
        // redirect to another host.
        let auth = scratch.path.join("authorization");
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new().create_new(true).write(true).mode(0o600).open(&auth)?
            .write_all(format!("Authorization: Bearer {}\n", token.trim()).as_bytes())?;
        extra.push("--header".into());
        extra.push(format!("@{}", auth.display()).into());
    }
    let extra: Vec<&std::ffi::OsStr> = extra.iter().map(|s| s.as_os_str()).collect();
    match curl(API, &json, false, METADATA_LIMIT, &extra) {
        Ok(()) => Ok(Answer::Release(parse_release(&fs::read(json)?)?)),
        Err(error) => match fs::read_to_string(&headers).ok().and_then(|h| refusal(&h, now())) {
            Some(refused) => Ok(Answer::Refused(refused)),
            None => Err(error),
        },
    }
}
fn ask_page(scratch: &Scratch) -> Result<Release> {
    let body = scratch.path.join("latest.html");
    let headers = scratch.path.join("latest.headers");
    curl(&format!("{PAGES}/latest"), &body, false, METADATA_LIMIT, &["--dump-header".as_ref(), headers.as_os_str()])?;
    let tag = latest_tag(&fs::read_to_string(&headers).context("the release page sent no headers")?)?;
    Ok(Release { version: version(&tag)?, tag, installer: None })
}

/// When the API's refusal said to ask again, if that is still ahead (and not past a large clock change).
fn api_held(clock: u64) -> Option<u64> {
    let cache = Cache::open(true).ok().flatten()?;
    let at = cache.read().ok()?.api_retry_at;
    (at > clock && at <= clock.saturating_add(DAY)).then_some(at)
}
fn hold_api(until: u64) {
    if let Some(cache) = Cache::open(true).ok().flatten() && let Ok(mut state) = cache.read() {
        state.api_retry_at = until;
        let _ = cache.write(&state);
    }
}

/// The latest release from GitHub's API. When the API refuses, as it does once a network's unauthenticated rate
/// limit is spent, the release page names the tag, and the API is not asked again until the limit resets.
fn fetch(scratch: &Scratch) -> Result<Release> {
    let clock = now();
    let refused = match api_held(clock) {
        Some(reset) => Refusal { status: 403, limited: true, reset: Some(reset) },
        None => match ask_api(scratch)? {
            Answer::Release(release) => return Ok(release),
            Answer::Refused(refused) => {
                if refused.limited { hold_api(refused.reset.unwrap_or(clock + 60 * 60).min(clock + DAY)); }
                refused
            },
        },
    };
    ask_page(scratch).map_err(|error| anyhow!("{} (the release page could not be read either: {error:#})", refused.explain()))
}

/// The binary from this platform's release archive, checked against the checksum published beside it.
fn from_archive(scratch: &Scratch, tag: &str) -> Result<PathBuf> {
    let target = TARGET.context("no KnowCode release archive for this platform; re-run the shell installer")?;
    let name = format!("knowcode-{target}.tar.gz");
    let sums = scratch.path.join(format!("{name}.sha256"));
    download(&format!("{DOWNLOAD}/{tag}/{name}.sha256"), &sums, true)?;
    let digest = published_checksum(&fs::read_to_string(&sums)?, &name)?;
    let archive = scratch.path.join(&name);
    curl(&format!("{DOWNLOAD}/{tag}/{name}"), &archive, true, ARCHIVE_LIMIT, &["--location".as_ref()])?;
    verify(&archive, &digest)?;
    let dir = scratch.path.join("archive");
    fs::create_dir(&dir)?;
    let member = format!("knowcode-{target}/knowcode");
    let mut tar = Command::new("tar");
    tar.arg("-xzf").arg(&archive).arg("-C").arg(&dir).args(["--no-same-owner", &member]);
    let out = limits::output(tar, Kind::UpdateInstall, "release archive", None)?;
    if !out.status.success() { bail!("could not unpack the release archive: {}", String::from_utf8_lossy(&out.stderr).trim()); }
    let binary = dir.join(member);
    if !fs::symlink_metadata(&binary).is_ok_and(|m| m.is_file()) { bail!("the release archive has no knowcode binary"); }
    Ok(binary)
}
/// The SHA-256 a `<name>.sha256` file gives for `name` (`<digest> *<name>` or `<digest>  <name>`).
fn published_checksum(text: &str, name: &str) -> Result<String> {
    let digest = text.lines().find_map(|line| {
        let (digest, file) = line.trim().split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then_some(digest)
    }).context("the release's checksum file does not list its archive")?;
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) { bail!("invalid archive checksum"); }
    Ok(digest.to_ascii_lowercase())
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
/// The shell installer's receipt for this executable: the installation prefix, the receipt's path and its text.
pub(crate) fn installation() -> Result<(PathBuf, PathBuf, String)> {
    let exe = std::env::current_exe()?;
    let receipt = receipt_path()?;
    if fs::symlink_metadata(&receipt).is_ok_and(|m| m.is_symlink()) { bail!("installer receipt is symlinked; reinstall with the shell installer"); }
    let text = fs::read_to_string(&receipt).context("no matching shell-install receipt; update through your package manager or re-run the shell installer")?;
    let data: serde_json::Value = serde_json::from_str(&text)?;
    if data.pointer("/provider/source").and_then(|s| s.as_str()) != Some("cargo-dist")
        || data.pointer("/source/owner").and_then(|s| s.as_str()) != Some(owner())
        || !data.pointer("/source/name").and_then(|s| s.as_str()).is_some_and(|n| n == repo_name() || n == FORMER_NAME)
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
    if !out.status.success() || String::from_utf8_lossy(&out.stdout).split_whitespace().next() != Some(expected) { bail!("release checksum mismatch; nothing installed"); }
    Ok(())
}

pub fn self_update(check_only: bool) -> Result<()> {
    // `brew upgrade` replaces the keg without running self-update; `serve` then refreshes the plugin bundle.
    let brew = homebrew();
    if brew && !check_only { bail!("Homebrew manages this install; run `{}`", brew_upgrade()); }
    let installation = if check_only { None } else { Some(installation()?) };
    let scratch = Scratch::new(&kc_index::cache_dir()?.join("updates"))?;
    let release = fetch(&scratch)?;
    if !newer(&release.tag) {
        println!("KnowCode {CURRENT} is current (latest stable release: {}).", release.version);
        return Ok(());
    }
    if check_only {
        let how = if brew { format!("`{}`", brew_upgrade()) } else { "`knowcode self-update` or upgrade through your package manager".into() };
        println!("KnowCode {} is available (installed: {CURRENT}); run {how}.", release.version);
        return Ok(());
    }
    let (prefix, receipt, receipt_before) = installation.expect("checked above");
    let Some(_cache) = Cache::open(false)? else { bail!("another update check is running; retry shortly"); };
    eprintln!("knowcode: downloading {}", release.tag);
    // Both are checked against a SHA-256 from GitHub before anything runs: the installer against the digest the API
    // gave, the archive against the checksum file published beside it.
    let payload = match &release.installer {
        Some(Installer { url, digest }) => {
            let installer = scratch.path.join("installer.sh");
            download(url, &installer, true)?;
            verify(&installer, digest)?;
            Payload::Installer(installer)
        },
        None => Payload::Binary(from_archive(&scratch, &release.tag)?),
    };
    let mut backup = Scratch::new(&prefix)?;
    let old = backup.path.join("knowcode");
    fs::copy(prefix.join("knowcode"), &old)?;
    let install = (|| -> Result<()> {
        eprintln!("knowcode: installing {}", release.tag);
        let installer = match &payload {
            Payload::Installer(installer) => installer,
            Payload::Binary(binary) => {
                // What the installer does with KNOWCODE_DISABLE_UPDATE set: the binary replaces the old one, and the
                // receipt is left as it is.
                let new = backup.path.join("knowcode.new");
                fs::copy(binary, &new)?;
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&new, fs::Permissions::from_mode(0o755))?;
                fs::rename(&new, prefix.join("knowcode"))?;
                return check_installed(&prefix, &release.version);
            },
        };
        let mut cmd = Command::new("sh");
        cmd.arg(installer).current_dir(&scratch.path).env("KNOWCODE_INSTALL_DIR", &prefix)
            .env("KNOWCODE_NO_MODIFY_PATH", "1").env("KNOWCODE_DISABLE_UPDATE", "1");
        for key in ["KNOWCODE_DOWNLOAD_URL", "INSTALLER_DOWNLOAD_URL", "KNOWCODE_INSTALLER_GHE_BASE_URL", "KNOWCODE_INSTALLER_GITHUB_BASE_URL", "KNOWCODE_GITHUB_TOKEN", "KNOWCODE_UNMANAGED_INSTALL"] { cmd.env_remove(key); }
        let out = limits::output(cmd, Kind::UpdateInstall, "KnowCode installer", None)?;
        if !out.status.success() { bail!("installer failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)); }
        check_installed(&prefix, &release.version)
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
    refresh_plugin(&prefix.join("knowcode"));
    Ok(())
}

/// What self-update installs: a checked shell installer to run, or a checked binary from a release archive.
enum Payload { Installer(PathBuf), Binary(PathBuf) }

fn check_installed(prefix: &Path, expected: &Version) -> Result<()> {
    let mut cmd = Command::new(prefix.join("knowcode")); cmd.arg("--version");
    let out = limits::output(cmd, Kind::UpdateCheck, "installed version check", None)?;
    if !out.status.success() || String::from_utf8_lossy(&out.stdout).trim() != format!("knowcode {expected}") { bail!("installed binary did not report the expected version"); }
    Ok(())
}

/// The installed binary, not this one, carries the new plugin bundle: it rewrites a Claude Code bundle an older
/// version wrote and has Claude Code update its copy. A problem is reported, never a failed update.
fn refresh_plugin(binary: &Path) {
    let mut cmd = Command::new(binary);
    cmd.arg("_refresh-plugin").stdin(Stdio::null());
    match limits::output(cmd, Kind::Setup, "Claude Code plugin refresh", None) {
        Ok(out) => eprint!("{}", String::from_utf8_lossy(&out.stderr)),
        Err(e) => eprintln!("knowcode: could not update the Claude Code plugin: {e:#}. Run `knowcode setup claude` to finish."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_repository_is_named_once_and_matches_the_package() {
        assert_eq!(env!("CARGO_PKG_REPOSITORY"), format!("https://github.com/{REPO}"));
        assert_eq!((owner(), repo_name()), ("bubstackdev", "know-code"));
        // Releases publish the formula to the tap that self-update names, under the package's name.
        let dist = include_str!("../../../dist-workspace.toml");
        assert_eq!(tap_repo!(), format!("{}/homebrew-tap", owner()));
        assert!(dist.contains(&format!("\ntap = \"{}\"\n", tap_repo!())), "dist-workspace.toml's tap");
        assert!(dist.contains(&format!("\npackages = [\"{FORMULA}\"]\n")) && !dist.contains("\nformula ="), "formula name");
        assert_eq!(FORMULA, env!("CARGO_PKG_NAME"));
        assert_eq!(brew_upgrade(), "brew upgrade bubstackdev/tap/knowcode");
    }

    #[test]
    fn this_build_names_a_release_archive_target() {
        let dist = include_str!("../../../dist-workspace.toml");
        // A build for another platform (an Intel Mac, say) has no archive and keeps to the installer.
        let Some(target) = TARGET else { return };
        assert!(dist.lines().any(|l| l.starts_with("targets = [") && l.contains(&format!("\"{target}\""))), "{target}");
    }

    #[test]
    fn the_latest_release_page_redirect_names_only_a_stable_tag_of_this_repository() {
        let redirect = |location: &str| format!("HTTP/2 302 \r\ndate: Mon, 26 Oct 2026 10:00:00 GMT\r\nLocation: {location}\r\n\r\n");
        assert_eq!(latest_tag(&redirect(&format!("{PAGES}/tag/v0.4.0"))).unwrap(), "v0.4.0");
        assert_eq!(latest_tag(&redirect(&format!("{PAGES}/tag/v0.4.0+build.1"))).unwrap(), "v0.4.0+build.1");
        for bad in [format!("{PAGES}/tag/v0.4.0-rc.1"), format!("{PAGES}/tag/latest"), format!("{PAGES}/tag/"),
            format!("{PAGES}/tag/v0.4.0/../../x"), format!("{PAGES}/tag/v0.4.0?x=1"), format!("{PAGES}/v0.4.0"),
            "https://github.com/someone/else/releases/tag/v9.0.0".into(), "https://example.com/releases/tag/v0.4.0".into(),
            "/bubstackdev/know-code/releases/tag/v0.4.0".into(), format!("{PAGES}tag/v0.4.0")] {
            assert!(latest_tag(&redirect(&bad)).is_err(), "{bad}");
        }
        // Not a redirect, or a redirect with no location.
        assert!(latest_tag("HTTP/2 200\r\ncontent-type: text/html\r\n\r\n").is_err());
        assert!(latest_tag("HTTP/2 302\r\n\r\n").is_err());
        assert!(latest_tag("").is_err());
        // Only the last response counts when more than one is in the file.
        let two = format!("HTTP/1.1 302 Found\r\nLocation: {PAGES}/tag/v0.1.0\r\n\r\n{}", redirect(&format!("{PAGES}/tag/v0.5.0")));
        assert_eq!(latest_tag(&two).unwrap(), "v0.5.0");
    }

    #[test]
    fn an_api_refusal_is_read_from_its_status_and_rate_limit_headers() {
        let limited = "HTTP/2 403 \r\nx-ratelimit-limit: 60\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1790000000\r\n\r\n";
        assert_eq!(refusal(limited, 1), Some(Refusal { status: 403, limited: true, reset: Some(1_790_000_000) }));
        assert_eq!(refusal("HTTP/2 429\r\nretry-after: 60\r\n\r\n", 1000), Some(Refusal { status: 429, limited: true, reset: Some(1060) }));
        assert_eq!(refusal("HTTP/2 403\r\nx-ratelimit-remaining: 12\r\n\r\n", 1), Some(Refusal { status: 403, limited: false, reset: None }));
        assert_eq!(refusal("HTTP/2 503\r\nx-ratelimit-remaining: 0\r\n\r\n", 1).map(|r| r.limited), Some(true));
        for other in ["HTTP/2 404\r\n\r\n", "HTTP/2 500\r\nx-ratelimit-remaining: 3\r\n\r\n", "HTTP/2 200\r\nx-ratelimit-remaining: 0\r\n\r\n", ""] {
            assert_eq!(refusal(other, 1), None, "{other}");
        }
        let said = refusal(limited, 1).unwrap().explain();
        assert!(said.starts_with("GitHub's API rate limit for this network is used up; try again after ")
            && said.ends_with(", or set KNOWCODE_GITHUB_TOKEN"), "{said}");
        let at = said.split("after ").nth(1).unwrap().split(',').next().unwrap();
        assert!(at.len() == 5 && at.as_bytes()[2] == b':', "{at}");
        assert!(Refusal { status: 403, limited: true, reset: None }.explain().contains("try again later"));
    }

    #[test]
    fn an_archive_checksum_comes_only_from_its_own_line() {
        let digest = "ab".repeat(32);
        let name = "knowcode-aarch64-apple-darwin.tar.gz";
        assert_eq!(published_checksum(&format!("{} *{name}\n", digest.to_uppercase()), name).unwrap(), digest);
        assert_eq!(published_checksum(&format!("{} *other.tar.gz\n{digest}  {name}\n", "cd".repeat(32)), name).unwrap(), digest);
        assert!(published_checksum(&format!("{digest} *{name}.sig\n"), name).is_err());
        assert!(published_checksum(&format!("{} *{name}\n", &digest[..62]), name).is_err());
        assert!(published_checksum("", name).is_err());
    }

    #[test]
    fn a_binary_in_a_homebrew_keg_is_recognized_through_its_bin_symlink() {
        let base = std::env::temp_dir().join(format!("knowcode-keg-{}", std::process::id()));
        let keg = base.join("Cellar/knowcode/0.1.1");
        fs::create_dir_all(keg.join("bin")).unwrap();
        fs::create_dir_all(base.join("bin")).unwrap();
        let exe = keg.join("bin/knowcode");
        fs::write(&exe, "").unwrap();
        let link = base.join("bin/knowcode");
        std::os::unix::fs::symlink(&exe, &link).unwrap();
        assert!(!homebrew_keg(&exe) && !homebrew_keg(&link), "no receipt: not a Homebrew install");
        fs::write(keg.join("INSTALL_RECEIPT.json"), "{}").unwrap();
        assert!(homebrew_keg(&exe) && homebrew_keg(&link));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn only_newer_stable_precedence_not_build_metadata_triggers_a_notice() {
        // A release build.
        assert!(!newer_than("v0.7.0", "0.7.0"));
        assert!(!newer_than("0.7.0+other-build", "0.7.0"));
        assert!(!newer_than("v0.8.0-rc.1", "0.7.0"));
        assert!(!newer_than("not-a-version", "0.7.0"));
        assert!(newer_than("v0.7.1", "0.7.0"));
        assert!(newer_than("0.8.0+build", "0.7.0"));
        // A development build ranks below its release and above the ones before it: an integration branch's
        // `0.7.0-dev` is offered 0.7.0 and later, never 0.6.1.
        assert!(!newer_than("v0.6.1", "0.7.0-dev"));
        assert!(newer_than("v0.7.0", "0.7.0-dev"));
        assert!(newer_than("v0.7.1", "0.7.0-dev"));
        assert!(!newer_than("v0.7.0-rc.1", "0.7.0-dev"));
        // And this build's own version parses, whatever it is.
        assert!(!newer(CURRENT));
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
