//! Shared workspace settings resolution; trust grants are personal only.
use crate::{Error, Result, protocol::*, workspace};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub const VERSION: u32 = 1;
pub const PROJECT_FILE: &str = ".know-code.json";
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// Missing inherits; null explicitly selects repository/compiler defaults.
#[derive(Clone, Debug, Default)]
pub enum Selection {
    #[default]
    Inherit,
    Automatic,
    Named(String),
}
impl Selection {
    fn inherited(&self) -> bool {
        matches!(self, Self::Inherit)
    }
}
impl<'de> Deserialize<'de> for Selection {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match Option::<String>::deserialize(d)? {
            Some(value) => Self::Named(value),
            None => Self::Automatic,
        })
    }
}
impl Serialize for Selection {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Named(value) => s.serialize_str(value),
            _ => s.serialize_none(),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CSharpSettings {
    #[serde(skip_serializing_if = "Selection::inherited")]
    pub configuration: Selection,
    #[serde(skip_serializing_if = "Selection::inherited")]
    pub solution: Selection,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RepositorySettings {
    csharp: CSharpSettings,
}

/// Mutations replace this whole object. Omitted fields inherit/default.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PersonalSettings {
    pub csharp: CSharpSettings,
    pub allow_install_scripts: bool,
}

pub fn paths(root: &Path, data: &Path) -> [PathBuf; 2] {
    [
        root.join(PROJECT_FILE),
        workspace::database_path(root, data).with_file_name("settings.json"),
    ]
}

fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Err(Error::InvalidRequest(format!(
            "settings file exceeds 64 KiB: {}",
            path.display()
        )));
    }
    let mut bytes = vec![];
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::InvalidRequest(format!(
            "settings file exceeds 64 KiB: {}",
            path.display()
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::InvalidRequest(format!("invalid settings in {}: {e}", path.display())))
}

fn selected(
    personal: &Selection,
    project: &Selection,
    legacy: Option<&str>,
) -> Result<(Option<String>, String)> {
    let (value, origin) = if !personal.inherited() {
        (personal, "personal")
    } else if !project.inherited() {
        (project, "project_file")
    } else {
        return Ok(match legacy.filter(|s| !s.is_empty()) {
            Some(value) => (Some(value.into()), "legacy_registration".into()),
            None => (None, "repository_default".into()),
        });
    };
    match value {
        Selection::Named(value) if value.trim().is_empty() => Err(Error::InvalidRequest(
            "settings selections must be nonempty strings or null".into(),
        )),
        Selection::Named(value) => Ok((Some(value.clone()), origin.into())),
        _ => Ok((None, origin.into())),
    }
}

pub fn resolve(
    root: &Path,
    data: &Path,
    legacy_configuration: &str,
) -> Result<WorkspaceAnalysisSettings> {
    resolve_personal(
        root,
        data,
        legacy_configuration,
        &read(&paths(root, data)[1])?,
    )
}
fn resolve_personal(
    root: &Path,
    data: &Path,
    legacy: &str,
    personal: &PersonalSettings,
) -> Result<WorkspaceAnalysisSettings> {
    let [project_file, personal_file] = paths(root, data);
    let project: RepositorySettings = read(&project_file)?;
    let (configuration, configuration_origin) = selected(
        &personal.csharp.configuration,
        &project.csharp.configuration,
        Some(legacy),
    )?;
    let (solution, solution_origin) =
        selected(&personal.csharp.solution, &project.csharp.solution, None)?;
    let solution_path = solution
        .map(|s| {
            let path = PathBuf::from(s);
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .map(|p| p.to_string_lossy().into_owned());
    Ok(WorkspaceAnalysisSettings {
        csharp: Some(CSharpBuildContext {
            configuration,
            solution_path,
        }),
        node: Some(NodePreparationPolicy {
            cache_directory: data.join("package-tools").to_string_lossy().into_owned(),
            allow_install_scripts: personal.allow_install_scripts,
        }),
        configuration_origin,
        solution_origin,
        project_file: project_file.to_string_lossy().into_owned(),
        personal_file: personal_file.to_string_lossy().into_owned(),
    })
}

pub fn personal(root: &Path, data: &Path) -> Result<PersonalSettings> {
    read(&paths(root, data)[1])
}

pub fn save(
    root: &Path,
    data: &Path,
    legacy: &str,
    value: &PersonalSettings,
) -> Result<WorkspaceAnalysisSettings> {
    let effective = resolve_personal(root, data, legacy, value)?;
    write_personal(root, data, value)?;
    Ok(effective)
}

/// Explicit removal revokes the grant before a different workspace can reuse the path.
/// Do not require valid project settings merely to revoke permission.
pub fn revoke_scripts(root: &Path, data: &Path) -> Result<()> {
    let mut value = personal(root, data)?;
    if value.allow_install_scripts {
        value.allow_install_scripts = false;
        write_personal(root, data, &value)?;
    }
    Ok(())
}

fn write_personal(root: &Path, data: &Path, value: &PersonalSettings) -> Result<()> {
    let path = paths(root, data)[1].clone();
    let parent = path.parent().expect("workspace settings parent");
    fs::create_dir_all(parent)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pending = parent.join(format!("settings.{}.{nonce}.pending", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pending)?;
        let encoded = serde_json::to_vec_pretty(value)?;
        if encoded.len() as u64 + 1 > MAX_FILE_BYTES {
            return Err(Error::InvalidRequest("settings exceed 64 KiB".into()));
        }
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&pending, &path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(pending);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/settings-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        let root = path.join("repo");
        fs::create_dir_all(&root).unwrap();
        (root, path.join("data"))
    }
    #[test]
    fn defaults_precedence_and_persistent_explicit_clearing() {
        let (root, data) = fixture();
        let old = resolve(&root, &data, "Debug").unwrap();
        assert_eq!(old.configuration_origin, "legacy_registration");
        fs::write(
            root.join(PROJECT_FILE),
            r#"{"csharp":{"configuration":"Release","solution":"App.sln"}}"#,
        )
        .unwrap();
        let scoped = resolve(&root, &data, "Debug").unwrap();
        assert_eq!(
            scoped.csharp.unwrap().configuration.as_deref(),
            Some("Release")
        );
        let value: PersonalSettings = serde_json::from_str(
            r#"{"csharp":{"configuration":null},"allow_install_scripts":true}"#,
        )
        .unwrap();
        let saved = save(&root, &data, "Debug", &value).unwrap();
        assert_eq!(saved.configuration_origin, "personal");
        let loaded = resolve(&root, &data, "Debug").unwrap();
        let cs = loaded.csharp.unwrap();
        assert_eq!(cs.configuration, None);
        assert_eq!(
            cs.solution_path,
            Some(root.join("App.sln").to_string_lossy().into_owned())
        );
        assert!(loaded.node.unwrap().allow_install_scripts);
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(paths(&root, &data)[1].clone()).unwrap()).unwrap();
        assert!(stored["csharp"].get("configuration").unwrap().is_null());
        let reset = save(&root, &data, "Debug", &PersonalSettings::default()).unwrap();
        assert_eq!(reset.configuration_origin, "project_file");
        assert!(!reset.node.unwrap().allow_install_scripts);
    }
    #[test]
    fn repository_cannot_grant_script_execution_and_bad_settings_preserve_file() {
        let (root, data) = fixture();
        fs::write(root.join(PROJECT_FILE), r#"{"allow_install_scripts":true}"#).unwrap();
        assert!(resolve(&root, &data, "").is_err());
        fs::remove_file(root.join(PROJECT_FILE)).unwrap();
        save(&root, &data, "", &PersonalSettings::default()).unwrap();
        let path = paths(&root, &data)[1].clone();
        let before = fs::read(&path).unwrap();
        let invalid: PersonalSettings =
            serde_json::from_str(r#"{"csharp":{"configuration":" "}}"#).unwrap();
        assert!(save(&root, &data, "", &invalid).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
    #[test]
    fn removal_revokes_grant_even_with_invalid_project_settings() {
        let (root, data) = fixture();
        save(
            &root,
            &data,
            "",
            &PersonalSettings {
                allow_install_scripts: true,
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(root.join(PROJECT_FILE), "broken").unwrap();
        revoke_scripts(&root, &data).unwrap();
        assert!(!personal(&root, &data).unwrap().allow_install_scripts);
    }
}
