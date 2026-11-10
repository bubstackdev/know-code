use know_code_core::{Error, Result, protocol::VERSION};
use serde::Deserialize;
use serde_json::json;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Deserialize)]
struct Manifest {
    layout_version: u32,
    worker_protocol: u32,
    build_profile: String,
    providers: Vec<Provider>,
}
#[derive(Debug, Deserialize)]
struct Provider {
    language: String,
    entrypoint: PathBuf,
}

pub fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::UpdateRequired(format!(
        "{}; install a complete matching Know Code runtime or set KNOW_CODE_RUNTIME_DIR to its resource directory",
        message.into()
    ))
}
pub fn locate() -> Result<PathBuf> {
    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    if let Some(root) = std::env::var_os("KNOW_CODE_RUNTIME_DIR") {
        return validate(Path::new(&root));
    }
    let parent = executable
        .parent()
        .ok_or_else(|| invalid("cannot locate executable directory"))?;
    let bundle = parent.join("../lib/know-code");
    // A bundle is authoritative even when damaged; never fall back to a checkout.
    if bundle.exists() {
        return validate(&bundle);
    }
    if parent.file_name().is_some_and(|s| s == profile())
        && parent
            .parent()
            .is_some_and(|p| p.file_name().is_some_and(|s| s == "target"))
        && let Some(root) = parent.parent().and_then(Path::parent)
    {
        return validate(root);
    }
    Err(invalid(
        "runtime resources were not found beside the executable",
    ))
}
fn validate(root: &Path) -> Result<PathBuf> {
    let root = std::fs::canonicalize(root)
        .map_err(|e| invalid(format!("cannot open runtime {}: {e}", root.display())))?;
    let manifest = root.join("runtime.json");
    if manifest.is_file() {
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(manifest)?)
            .map_err(|e| invalid(format!("invalid runtime manifest: {e}")))?;
        if manifest.layout_version != 1
            || manifest.worker_protocol != VERSION
            || manifest.build_profile != profile()
        {
            return Err(invalid(
                "runtime layout/protocol/build profile is incompatible",
            ));
        }
        for required in ["typescript", "csharp"] {
            if !manifest.providers.iter().any(|p| p.language == required) {
                return Err(invalid(format!(
                    "runtime lacks required {required} provider"
                )));
            }
        }
        for p in manifest.providers {
            if p.entrypoint
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
                || !root.join(&p.entrypoint).is_file()
            {
                return Err(invalid(format!(
                    "runtime {} provider is missing or has an invalid relative path",
                    p.language
                )));
            }
        }
        for path in [
            "proto/analysis.proto",
            "providers/shared/protocol.mjs",
            "node_modules/protobufjs/package.json",
            "node_modules/typescript/package.json",
            "providers/dotnet/KnowCode.Provider.DotNet.runtimeconfig.json",
        ] {
            if !root.join(path).is_file() {
                return Err(invalid(format!("runtime resource is missing: {path}")));
            }
        }
        return Ok(root);
    }
    // Development is recognized only by explicit override or target/<profile>
    // executable layout. No compiled-in source path or current-directory search.
    if root.join("Cargo.toml").is_file()
        && root.join("proto/analysis.proto").is_file()
        && root.join("providers/typescript/worker.mjs").is_file()
    {
        return Ok(root);
    }
    Err(invalid("runtime manifest is missing"))
}
pub fn info() -> Result<serde_json::Value> {
    let root = locate()?;
    let connection = rusqlite::Connection::open_in_memory()?;
    let fts5: bool =
        connection.query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| {
            r.get(0)
        })?;
    Ok(
        json!({"version":env!("CARGO_PKG_VERSION"),"target":"darwin-arm64","profile":profile(),"resource_directory":root,
        "layout":if root.join("runtime.json").is_file(){"bundle"}else{"development"},"worker_protocol":VERSION,"store_format":know_code_core::store::SCHEMA_VERSION,"resource_control_version":know_code_core::resource::CONTROL_VERSION,"analysis_settings_version":know_code_core::settings::VERSION,"index_lifecycle_version":know_code_core::protocol::INDEX_LIFECYCLE_VERSION,
        "sqlite":{"version":rusqlite::version(),"bundled":true,"fts5":fts5},"telemetry":"none"}),
    )
}
