//! User-scoped runtime lifecycle. Index data never belongs to the binary install tree.
use know_code_core::{
    control,
    protocol::{ControlRequest, ControlResponse},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    format: u32,
    prefix: PathBuf,
    home: PathBuf,
    data: PathBuf,
    label: String,
    path: String,
    login: bool,
    #[serde(skip, default = "current_lifecycle")]
    control_version: u32,
}
fn current_lifecycle() -> u32 {
    know_code_core::protocol::INDEX_LIFECYCLE_VERSION
}
fn install_request(
    s: &Settings,
    request: ControlRequest,
) -> know_code_core::Result<ControlResponse> {
    control::request_installer(&s.data, request, s.control_version)
}
struct Layout {
    prefix: PathBuf,
    base: PathBuf,
    versions: PathBuf,
    current: PathBuf,
    previous: PathBuf,
    launcher: PathBuf,
    state: PathBuf,
}
impl Layout {
    fn new(prefix: PathBuf) -> Self {
        let base = prefix.join("share/know-code");
        Self {
            versions: base.join("versions"),
            current: base.join("current"),
            previous: base.join("previous"),
            state: base.join("installation.json"),
            launcher: prefix.join("bin/know-code"),
            prefix,
            base,
        }
    }
    fn lock(&self) -> Result<File> {
        private_dir(&self.base)?;
        if self.base.canonicalize()? != self.base {
            return Err("managed runtime directories must not traverse symlinks".into());
        }
        if self.versions.symlink_metadata().is_ok() && self.versions.is_symlink() {
            return Err("managed versions directory must not be a symlink".into());
        }
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.base.join("operation.lock"))?;
        f.try_lock()
            .map_err(|_| "another install operation is running")?;
        Ok(f)
    }
    fn settings(&self) -> Result<Option<Settings>> {
        if !self.state.exists() {
            return Ok(None);
        }
        let s: Settings = serde_json::from_slice(&fs::read(&self.state)?)?;
        if s.format != 1 || s.prefix != self.prefix {
            return Err("installation state does not match this prefix".into());
        }
        Ok(Some(s))
    }
    fn selected(&self, link: &Path) -> Result<Option<PathBuf>> {
        match fs::read_link(link) {
            Ok(target) => {
                let resolved = link.parent().unwrap().join(target).canonicalize()?;
                if !resolved.starts_with(&self.versions) {
                    return Err("runtime selection escapes the managed versions directory".into());
                }
                Ok(Some(resolved))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn verify_launcher(&self) -> Result<()> {
        match fs::read_link(&self.launcher) {
            Ok(path) if path == Path::new("../share/know-code/current/bin/know-code") => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !self.launcher.exists() => Ok(()),
            _ => Err(format!(
                "refusing to replace an unmanaged launcher at {}",
                self.launcher.display()
            )
            .into()),
        }
    }
}
fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(())
}
fn absolute(path: PathBuf) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
fn atomic_file(path: &Path, bytes: &[u8]) -> Result<()> {
    private_dir(path.parent().unwrap())?;
    let temp = path.with_extension(format!("{}.pending", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn atomic_link(path: &Path, target: &Path) -> Result<()> {
    private_dir(path.parent().unwrap())?;
    let temp = path.with_extension(format!("{}.pending", std::process::id()));
    if temp.is_symlink() {
        fs::remove_file(&temp)?;
    }
    symlink(target, &temp)?;
    fs::rename(&temp, path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn save(layout: &Layout, settings: &Settings) -> Result<()> {
    atomic_file(&layout.state, &serde_json::to_vec_pretty(settings)?)
}
fn plist_path(settings: &Settings) -> PathBuf {
    settings
        .home
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", settings.label))
}
fn plist(layout: &Layout, s: &Settings) -> String {
    super::coordinator::managed_login_plist(&layout.launcher, &s.data, &s.label, &s.path)
}
fn verify_plist(layout: &Layout, s: &Settings) -> Result<()> {
    let p = plist_path(s);
    if p.exists() && fs::read(&p)? != plist(layout, s).as_bytes() {
        return Err(format!(
            "refusing to change an unmanaged or modified login plist: {}",
            p.display()
        )
        .into());
    }
    Ok(())
}
fn output(program: &Path, args: &[&str]) -> Result<std::process::Output> {
    Ok(Command::new(program)
        .args(args)
        .env_remove("KNOW_CODE_RUNTIME_DIR")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .output()?)
}
fn checked(program: &Path, args: &[&str]) -> Result<String> {
    let o = output(program, args)?;
    if !o.status.success() {
        return Err(format!(
            "{} {} failed: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(o.stdout)?)
}
fn domain() -> Result<String> {
    let uid = checked(Path::new("/usr/bin/id"), &["-u"])?;
    Ok(format!("gui/{}", uid.trim()))
}
fn service_id(s: &Settings) -> Result<String> {
    Ok(format!("{}/{}", domain()?, s.label))
}
fn loaded(s: &Settings) -> Result<bool> {
    Ok(
        output(Path::new("/bin/launchctl"), &["print", &service_id(s)?])?
            .status
            .success(),
    )
}
fn unload(layout: &Layout, s: &Settings) -> Result<()> {
    verify_plist(layout, s)?;
    if loaded(s)? {
        if !plist_path(s).exists() {
            return Err("login label belongs to another installation; refusing to stop it".into());
        }
        checked(Path::new("/bin/launchctl"), &["bootout", &service_id(s)?])?;
    }
    Ok(())
}
fn load(layout: &Layout, s: &Settings) -> Result<()> {
    verify_plist(layout, s)?;
    if loaded(s)? && !plist_path(s).exists() {
        return Err("login label is already loaded by another installation".into());
    }
    atomic_file(&plist_path(s), plist(layout, s).as_bytes())?;
    checked(Path::new("/bin/launchctl"), &["enable", &service_id(s)?])?;
    if !loaded(s)? {
        checked(
            Path::new("/bin/launchctl"),
            &[
                "bootstrap",
                &domain()?,
                plist_path(s).to_str().ok_or("non-UTF-8 plist path")?,
            ],
        )?;
    }
    Ok(())
}
fn status(s: &Settings) -> Result<Option<ControlResponse>> {
    match install_request(
        s,
        ControlRequest {
            operation: "status".into(),
            ..Default::default()
        },
    ) {
        Ok(r) => Ok(Some(r)),
        Err(know_code_core::Error::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}
fn owns(layout: &Layout, response: &ControlResponse) -> bool {
    let p = Path::new(&response.coordinator_executable);
    p.is_absolute() && p.starts_with(&layout.versions) && p.ends_with("bin/know-code")
}
struct Maintenance {
    data: PathBuf,
    token: String,
    owned: bool,
    control_version: u32,
}
impl Drop for Maintenance {
    fn drop(&mut self) {
        if self.owned {
            let _ = control::request_installer(
                &self.data,
                ControlRequest {
                    operation: "resume".into(),
                    maintenance_token: self.token.clone(),
                    ..Default::default()
                },
                self.control_version,
            );
        }
    }
}
fn pause(layout: &Layout, s: &Settings) -> Result<Maintenance> {
    let mut guard = Maintenance {
        data: s.data.clone(),
        token: format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ),
        owned: false,
        control_version: s.control_version,
    };
    let Some(response) = status(s)? else {
        return Ok(guard);
    };
    if !owns(layout, &response) {
        return Err("coordinator is not managed by this installation; stop it explicitly before changing this installation".into());
    }
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        // If the reply is lost after acceptance, still attempt to release our lease.
        guard.owned = true;
        let response = install_request(
            s,
            ControlRequest {
                operation: "maintenance".into(),
                maintenance_token: guard.token.clone(),
                maintenance_lease_ms: 30000,
                ..Default::default()
            },
        )
        .map_err(|e| format!("cannot pause this coordinator for a safe runtime change: {e}"))?;
        if !response.analysis_running {
            return Ok(guard);
        }
        if Instant::now() >= until {
            return Err("indexing is still busy after 30 seconds; indexing resumed and the selected runtime was left unchanged; retry later".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
fn gate(s: &Settings) -> Result<File> {
    private_dir(&s.data)?;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(s.data.join("lifecycle.lock"))?;
    f.try_lock()
        .map_err(|_| "another coordinator startup/runtime operation is in progress")?;
    Ok(f)
}
fn stop(layout: &Layout, s: &Settings) -> Result<()> {
    let was_loaded = loaded(s)?;
    unload(layout, s)?;
    if !was_loaded && let Some(response) = status(s)? {
        if !owns(layout, &response) {
            return Err("refusing to stop a coordinator outside this installation".into());
        }
        install_request(
            s,
            ControlRequest {
                operation: "shutdown".into(),
                ..Default::default()
            },
        )?;
    }
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(s.data.join("coordinator.lock"))?;
        if f.try_lock().is_ok() {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err("coordinator did not release its writer fence; runtime selection was left unchanged".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn ready(layout: &Layout, s: &Settings) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(response) = status(s)? {
            if owns(layout, &response) {
                return Ok(());
            }
            return Err("another coordinator took the data directory".into());
        }
        if Instant::now() >= until {
            return Err("managed coordinator did not become ready; inspect the login job and its toolchain PATH".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn info(bundle: &Path) -> Result<Value> {
    Ok(serde_json::from_str(&checked(
        &bundle.join("bin/know-code"),
        &["runtime-info"],
    )?)?)
}
fn same_contract(a: &Path, b: &Path) -> Result<()> {
    compatible_info(&info(a)?, &info(b)?)
}
fn compatible_info(a: &Value, b: &Value) -> Result<()> {
    if b["index_lifecycle_version"] != know_code_core::protocol::INDEX_LIFECYCLE_VERSION {
        return Err("target runtime lacks the required index-lifecycle safety contract; use a compatible runtime".into());
    }
    if b["analysis_settings_version"] != know_code_core::settings::VERSION {
        return Err("target runtime lacks the required workspace analysis-settings contract; use a compatible runtime rather than rolling back to legacy settings".into());
    }
    if b["resource_control_version"] != know_code_core::resource::CONTROL_VERSION {
        return Err("target runtime lacks the required recorded cancellation boundary; use a compatible runtime".into());
    }
    if !a["index_lifecycle_version"]
        .as_u64()
        .is_some_and(|version| {
            matches!(version, 1..=3)
                || version == u64::from(know_code_core::protocol::INDEX_LIFECYCLE_VERSION)
        })
        || a["resource_control_version"] != know_code_core::resource::CONTROL_VERSION
        || a["analysis_settings_version"] != know_code_core::settings::VERSION
        || a["worker_protocol"] != b["worker_protocol"]
    {
        return Err(
            "source runtime has no supported quiescence/upgrade contract; update required".into(),
        );
    }
    let source = a["store_format"]
        .as_i64()
        .ok_or("source runtime lacks store-format evidence")?;
    let target = b["store_format"]
        .as_i64()
        .ok_or("target runtime lacks store-format evidence")?;
    if target < source || !supports_store(b, source) {
        return Err("target runtime has no supported forward store transition; update required; runtime selection was left unchanged".into());
    }
    Ok(())
}
fn contains_version(info: &Value, field: &str, version: i64) -> bool {
    info[field]
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v.as_i64() == Some(version)))
}
fn supports_store(info: &Value, version: i64) -> bool {
    [
        "store_read_versions",
        "store_migrate_versions",
        "store_rebuild_versions",
    ]
    .iter()
    .any(|field| contains_version(info, field, version))
}
/// Inspect actual registered contracts under the installer maintenance fence.
/// Unknown state is never permission to rewrite or reverse an index.
fn compatible_data(s: &Settings, target: &Value, legacy_fallback: bool) -> Result<()> {
    let path = s.data.join("registry.db");
    match fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let registry =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut query = registry.prepare("SELECT database FROM workspaces ORDER BY database")?;
    let databases = query.query_map([], |r| r.get::<_, String>(0))?;
    for database in databases {
        let database = PathBuf::from(database?);
        if control::root_for_database(&database) != Some(s.data.as_path()) {
            return Err(
                "registry contains a workspace outside this installation's data root".into(),
            );
        }
        if let Some(version) = know_code_core::recovery::lifecycle_version(&database)? {
            let supported =
                contains_version(target, "index_lifecycle_read_versions", i64::from(version))
                    || (legacy_fallback && target["index_lifecycle_version"] == 1 && version == 1);
            if !supported {
                return Err("target runtime cannot read a registered workspace lifecycle; update required; selections were left unchanged".into());
            }
        }
        let physical = know_code_core::recovery::selected_path(&database)?;
        if !physical.is_file() {
            continue;
        }
        let connection = rusqlite::Connection::open_with_flags(
            physical,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let format = match connection.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)) {
            Ok(format) => format,
            Err(error)
                if know_code_core::recovery::sqlite_corruption(&error) && !legacy_fallback =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let supported = supports_store(target, format)
            || (legacy_fallback
                && target["index_lifecycle_version"] == 1
                && (1..=8).contains(&format));
        if !supported {
            return Err(format!("target runtime has no supported path for registered index format {format}; update required; selections were left unchanged").into());
        }
    }
    Ok(())
}
fn start(layout: &Layout, s: &Settings, login: bool, was_running: bool) -> Result<()> {
    if login {
        load(layout, s)?;
    } else if was_running {
        Command::new(&layout.launcher)
            .args(["coordinator", "run"])
            .env_remove("KNOW_CODE_RUNTIME_DIR")
            .env("KNOW_CODE_DATA_DIR", &s.data)
            .env("PATH", &s.path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    }
    if login || was_running {
        ready(layout, s)?;
    }
    Ok(())
}
fn activate(layout: &Layout, s: &Settings, target: &Path, keep_previous: bool) -> Result<()> {
    layout.verify_launcher()?;
    verify_plist(layout, s)?;
    let old = layout.selected(&layout.current)?;
    let previous = layout.selected(&layout.previous)?;
    if let Some(old) = &old {
        same_contract(old, target)?;
    }
    let target_info = info(target)?;
    let mut target_settings = s.clone();
    target_settings.control_version = target_info["index_lifecycle_version"]
        .as_u64()
        .ok_or("target lacks lifecycle contract")? as u32;
    let prior_login = layout.settings()?.is_some_and(|s| s.login);
    if old.as_deref() == Some(target) && prior_login == s.login && (!s.login || loaded(s)?) {
        return Ok(());
    }
    let _gate = gate(s)?;
    let was_running = status(s)?.is_some();
    let _pause = pause(layout, s)?;
    compatible_data(s, &target_info, false)?;
    let was_loaded = loaded(s)?;
    let result = (|| -> Result<()> {
        stop(layout, s)?;
        if keep_previous && let Some(old) = &old {
            atomic_link(&layout.previous, old)?;
        }
        atomic_link(&layout.current, target)?;
        start(layout, &target_settings, s.login, was_running)?;
        Ok(())
    })();
    if let Err(error) = result {
        drop(_pause);
        let restored = (|| -> Result<()> {
            // A failed new coordinator may already own the writer fence. Quiesce it
            // before restoring the old selection, just as for a normal update.
            let active_settings = if layout.selected(&layout.current)?.as_deref() == Some(target) {
                &target_settings
            } else {
                s
            };
            let _pause = pause(layout, active_settings)?;
            stop(layout, active_settings)?;
            if let Some(old) = &old {
                compatible_data(s, &info(old)?, true)?;
                atomic_link(&layout.current, old)?;
                start(layout, s, was_loaded, was_running)?;
            } else if layout.current.is_symlink() {
                fs::remove_file(&layout.current)?;
            }
            if let Some(previous) = previous {
                atomic_link(&layout.previous, &previous)?;
            } else if layout.previous.is_symlink() {
                fs::remove_file(&layout.previous)?;
            }
            Ok(())
        })();
        if let Err(restore_error) = restored {
            return Err(format!("activation failed: {error}; recovery also failed: {restore_error}; inspect install-info before retrying").into());
        }
        return Err(format!("activation failed; previous selection restored: {error}").into());
    }
    Ok(())
}
fn tree_digest(root: &Path) -> Result<String> {
    fn visit(root: &Path, dir: &Path, digest: &mut Sha256) -> Result<()> {
        let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
                return Err("runtime tree may contain only regular files/directories".into());
            }
            let relative = path.strip_prefix(root)?;
            let name = relative.as_os_str().as_encoded_bytes();
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name);
            if kind.is_dir() {
                digest.update(b"d");
                visit(root, &path, digest)?;
            } else {
                digest.update(b"f");
                let metadata = fs::metadata(&path)?;
                digest.update((metadata.permissions().mode() & 0o777).to_le_bytes());
                digest.update(metadata.len().to_le_bytes());
                let mut file = File::open(path)?;
                let mut buffer = [0; 65536];
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    digest.update(&buffer[..n]);
                }
            }
        }
        Ok(())
    }
    let mut digest = Sha256::new();
    visit(root, root, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}
fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    private_dir(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if ty.is_symlink() {
            return Err("runtime tree contains a symlink".into());
        }
        if ty.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &target)?;
            File::open(target)?.sync_all()?;
        } else {
            return Err("runtime contains a non-regular entry".into());
        }
    }
    fs::set_permissions(dest, fs::metadata(source)?.permissions())?;
    File::open(dest)?.sync_all()?;
    Ok(())
}
fn stage(layout: &Layout, source: &Path) -> Result<PathBuf> {
    let source = source.canonicalize()?;
    let details = info(&source)?;
    if details["layout"] != "bundle" {
        return Err("install requires an extracted runtime bundle".into());
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(source.join("lib/know-code/runtime.json"))?)?;
    let version = manifest["version"]
        .as_str()
        .ok_or("runtime version is missing")?;
    if version.is_empty()
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
    {
        return Err("runtime version is invalid".into());
    }
    let digest = tree_digest(&source)?;
    private_dir(&layout.versions)?;
    let dest = layout.versions.join(format!("{version}-{}", &digest[..16]));
    if dest.exists() {
        if tree_digest(&dest)? != digest {
            return Err("installed runtime content was modified".into());
        }
        return Ok(dest);
    }
    let staging = layout
        .versions
        .join(format!(".staging-{}", std::process::id()));
    if staging.exists() {
        return Err("an interrupted staging directory exists; inspect/remove that staging directory before retrying".into());
    }
    let result = (|| -> Result<()> {
        copy_tree(&source, &staging)?;
        if tree_digest(&staging)? != digest {
            return Err("runtime changed while copying".into());
        }
        info(&staging)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    fs::rename(&staging, &dest)?;
    File::open(&layout.versions)?.sync_all()?;
    Ok(dest)
}
fn prerequisites() -> Result<()> {
    let node = checked(Path::new("node"), &["--version"])
        .map_err(|e| format!("install Node 24 separately and put it on PATH: {e}"))?;
    if !node.trim().starts_with("v24.") {
        return Err("install/use Node 24 separately; this candidate supports Node 24.x".into());
    }
    let sdks = checked(Path::new("dotnet"), &["--list-sdks"])
        .map_err(|e| format!("install the .NET 10 SDK separately and put it on PATH: {e}"))?;
    if !sdks.lines().any(|l| l.starts_with("10.")) {
        return Err("install the .NET 10 SDK separately; no SDK was installed by Know Code".into());
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<Value> {
    let operation = args.first().ok_or("missing install operation")?.as_str();
    let mut index = 1;
    let service_action = if operation == "service" {
        let action = args
            .get(index)
            .ok_or("service requires enable|disable|status")?
            .clone();
        index += 1;
        Some(action)
    } else {
        None
    };
    let home = absolute(PathBuf::from(
        std::env::var_os("HOME").ok_or("HOME is required")?,
    ))?;
    let mut prefix = home.join(".local");
    let mut from = None;
    let mut no_login = false;
    let mut label = None;
    while index < args.len() {
        match args[index].as_str() {
            "--no-login" => no_login = true,
            "--prefix" | "--from" | "--label" => {
                let flag = &args[index];
                index += 1;
                let value = args.get(index).ok_or("option requires a value")?;
                match flag.as_str() {
                    "--prefix" => prefix = absolute(PathBuf::from(value))?,
                    "--from" => from = Some(PathBuf::from(value)),
                    _ => label = Some(value.clone()),
                }
            }
            _ => return Err(format!("unknown lifecycle option {}", args[index]).into()),
        };
        index += 1;
    }
    if !matches!(operation, "install" | "update") && from.is_some() {
        return Err("--from is only valid for install/update".into());
    }
    if operation != "install" && (no_login || label.is_some()) {
        return Err("--no-login and --label are only valid for a new install".into());
    }
    if operation == "service"
        && !matches!(
            service_action.as_deref(),
            Some("enable" | "disable" | "status")
        )
    {
        return Err("service requires enable|disable|status".into());
    }
    if operation == "install-info" || service_action.as_deref() == Some("status") {
        if prefix.exists() {
            prefix = prefix.canonicalize()?;
        }
        let layout = Layout::new(prefix);
        let Some(s) = layout.settings()? else {
            return Ok(json!({"installed":false,"prefix":layout.prefix}));
        };
        return Ok(
            json!({"installed":true,"prefix":s.prefix,"data_directory":s.data,"selected":layout.selected(&layout.current)?,"previous":layout.selected(&layout.previous)?,"login_enabled":s.login,"login_loaded":loaded(&s)?,"launcher":layout.launcher,"runtime_versions":fs::read_dir(&layout.versions)?.count()}),
        );
    }
    private_dir(&prefix)?;
    let layout = Layout::new(prefix.canonicalize()?);
    let _operation = layout.lock()?;
    let existing = layout.settings()?;
    if existing.is_some() && (no_login || label.is_some()) {
        return Err(
            "use service enable/disable to change an existing installation's login policy".into(),
        );
    }
    let mut s = if let Some(s) = existing.clone() {
        s
    } else {
        if operation != "install" {
            return Err("this prefix has no managed installation".into());
        }
        if layout.launcher.symlink_metadata().is_ok() {
            return Err("an unmanaged launcher already exists; choose another prefix".into());
        }
        let label = label.unwrap_or_else(|| "dev.bubstack.know-code".into());
        if label.is_empty()
            || label.len() > 128
            || !label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
        {
            return Err("invalid login service label".into());
        }
        Settings {
            format: 1,
            prefix: layout.prefix.clone(),
            home,
            data: absolute(control::data_root()?)?,
            label,
            path: know_code_core::update::trusted_path(&std::env::join_paths(
                std::iter::once(layout.prefix.join("bin")).chain(std::env::split_paths(
                    &std::env::var_os("PATH").unwrap_or_default(),
                )),
            )?)?
            .into_string()
            .map_err(|_| "toolchain PATH must be valid UTF-8")?,
            login: !no_login,
            control_version: current_lifecycle(),
        }
    };
    s.path = know_code_core::update::trusted_path(std::ffi::OsStr::new(&s.path))?
        .into_string()
        .map_err(|_| "toolchain PATH must be valid UTF-8")?;
    if let Some(selected) = layout.selected(&layout.current)? {
        let version = info(&selected)?["index_lifecycle_version"]
            .as_u64()
            .ok_or("installed runtime lacks lifecycle evidence")?;
        if !matches!(version, 1..=3)
            && version != u64::from(know_code_core::protocol::INDEX_LIFECYCLE_VERSION)
        {
            return Err("installed lifecycle is unsupported; use a compatible installer".into());
        }
        s.control_version = version as u32;
    }
    private_dir(&s.data)?;
    s.data = s.data.canonicalize()?;
    if s.data.starts_with(&layout.base) {
        return Err("index/settings data must be outside the managed runtime tree".into());
    }
    layout.verify_launcher()?;
    verify_plist(&layout, &s)?;
    if existing.is_none() && s.login && (loaded(&s)? || plist_path(&s).exists()) {
        return Err("an unmanaged login job already uses this label; the running development service was left unchanged".into());
    }
    match operation {
        "install" | "update" => {
            prerequisites()?;
            let source = match from {
                Some(p) => p,
                None => super::runtime::locate()?
                    .parent()
                    .and_then(Path::parent)
                    .ok_or("cannot infer runtime bundle; use --from")?
                    .to_path_buf(),
            };
            let target = stage(&layout, &source)?;
            if existing.is_none() {
                save(&layout, &s)?;
            }
            atomic_link(
                &layout.launcher,
                Path::new("../share/know-code/current/bin/know-code"),
            )?;
            activate(&layout, &s, &target, true)?;
        }
        "rollback" => {
            let previous = layout
                .selected(&layout.previous)?
                .ok_or("no previous runtime is retained")?;
            let old = layout.selected(&layout.current)?;
            activate(&layout, &s, &previous, false)?;
            if let Some(old) = old {
                atomic_link(&layout.previous, &old)?;
            }
        }
        "service" => match service_action.as_deref() {
            Some("enable") => {
                prerequisites()?;
                let target = layout
                    .selected(&layout.current)?
                    .ok_or("installation has no selected runtime")?;
                s.login = true;
                activate(&layout, &s, &target, false)?;
                save(&layout, &s)?;
            }
            Some("disable") => {
                let _gate = gate(&s)?;
                let _pause = pause(&layout, &s)?;
                stop(&layout, &s)?;
                checked(Path::new("/bin/launchctl"), &["disable", &service_id(&s)?])?;
                if plist_path(&s).exists() {
                    fs::remove_file(plist_path(&s))?;
                }
                s.login = false;
                save(&layout, &s)?;
            }
            _ => return Err("service requires enable|disable|status".into()),
        },
        "uninstall" => {
            let _gate = gate(&s)?;
            let _pause = pause(&layout, &s)?;
            stop(&layout, &s)?;
            checked(Path::new("/bin/launchctl"), &["disable", &service_id(&s)?])?;
            if plist_path(&s).exists() {
                fs::remove_file(plist_path(&s))?;
            }
            for link in [&layout.launcher, &layout.current, &layout.previous] {
                if link.symlink_metadata().is_ok() {
                    fs::remove_file(link)?;
                }
            }
            if layout.versions.exists() {
                fs::remove_dir_all(&layout.versions)?;
            }
            fs::remove_file(&layout.state)?;
            return Ok(json!({"installed":false,"data_retained":s.data}));
        }
        _ => return Err("expected install|update|rollback|uninstall|install-info|service".into()),
    }
    Ok(
        json!({"installed":true,"prefix":layout.prefix,"selected":layout.selected(&layout.current)?,"previous":layout.selected(&layout.previous)?,"launcher":layout.launcher,"login_enabled":s.login,"data_directory":s.data}),
    )
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    fn supported() -> Value {
        json!({
            "index_lifecycle_version": know_code_core::protocol::INDEX_LIFECYCLE_VERSION,
            "analysis_settings_version": know_code_core::settings::VERSION,
            "resource_control_version": know_code_core::resource::CONTROL_VERSION,
            "worker_protocol": know_code_core::protocol::VERSION,
            "store_format": 8,
            "index_lifecycle_read_versions": [1,2],
            "store_read_versions": [2,3,4,5,6,7,8],
            "store_migrate_versions": [3,4,5,6,7],
            "store_rebuild_versions": [1,2]
        })
    }

    #[test]
    fn runtime_switch_requires_explicit_lifecycle_safety() {
        let supported = supported();
        assert!(compatible_info(&supported, &supported).is_ok());
        for value in [Value::Null, json!(0), json!(5), json!(2), json!(3)] {
            let mut target = supported.clone();
            target["index_lifecycle_version"] = value;
            assert!(
                compatible_info(&supported, &target)
                    .unwrap_err()
                    .to_string()
                    .contains("index-lifecycle")
            );
        }
        let mut old = supported.clone();
        old["index_lifecycle_version"] = json!(1);
        assert!(compatible_info(&old, &supported).is_ok());
        let mut previous = supported.clone();
        previous["index_lifecycle_version"] = json!(2);
        assert!(compatible_info(&previous, &supported).is_ok());
        previous["index_lifecycle_version"] = json!(3);
        assert!(compatible_info(&previous, &supported).is_ok());
        assert!(compatible_info(&supported, &old).is_err());
        let mut missing_catalog = supported.clone();
        for field in [
            "store_read_versions",
            "store_migrate_versions",
            "store_rebuild_versions",
        ] {
            missing_catalog.as_object_mut().unwrap().remove(field);
        }
        assert!(compatible_info(&old, &missing_catalog).is_err());
        old["store_format"] = json!(9);
        assert!(compatible_info(&old, &supported).is_err());
        let mut mismatch = supported.clone();
        mismatch["store_format"] = json!(7);
        assert!(compatible_info(&supported, &mismatch).is_err());
    }
    #[test]
    fn registered_store_and_lifecycle_evidence_block_unsafe_switches_without_rewrites() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
            .join(".artifacts/ig")
            .join(format!(
                "{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let database = root
            .join("d/workspaces")
            .join("a".repeat(24))
            .join("index.db");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(include_str!("../../../tests/fixtures/schema-v2.sql"))
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO generations VALUES(42); UPDATE state SET current_generation=42;",
            )
            .unwrap();
        drop(connection);
        know_code_core::recovery::adopt(&database, 0).unwrap();
        let s = Settings {
            format: 1,
            prefix: root.join("p"),
            home: root.join("h"),
            data: root.join("d"),
            label: "unused-test".into(),
            path: String::new(),
            login: false,
            control_version: current_lifecycle(),
        };
        let registry = rusqlite::Connection::open(s.data.join("registry.db")).unwrap();
        registry
            .execute_batch("CREATE TABLE workspaces(database TEXT);")
            .unwrap();
        registry
            .execute(
                "INSERT INTO workspaces VALUES(?)",
                [database.to_str().unwrap()],
            )
            .unwrap();
        drop(registry);
        let target = supported();
        assert!(compatible_data(&s, &target, false).is_ok());
        let selector = database.with_extension("lifecycle.json");
        let receipt = fs::read(&selector).unwrap();
        let mut legacy = target.clone();
        legacy["index_lifecycle_version"] = json!(1);
        legacy["index_lifecycle_read_versions"] = json!([1]);
        assert!(compatible_data(&s, &legacy, true).is_err());
        assert_eq!(fs::read(&selector).unwrap(), receipt);
        let physical = know_code_core::recovery::selected_path(&database).unwrap();
        let connection = rusqlite::Connection::open(&physical).unwrap();
        connection
            .execute_batch("PRAGMA user_version=999;")
            .unwrap();
        drop(connection);
        let bytes = fs::read(&physical).unwrap();
        assert!(compatible_data(&s, &target, false).is_err());
        assert_eq!(fs::read(&physical).unwrap(), bytes);
        assert_eq!(fs::read(&selector).unwrap(), receipt);
    }
}
