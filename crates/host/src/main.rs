mod coordinator;
mod install;
mod mcp;
mod runtime;
use know_code_core::{control, protocol::ControlRequest};
use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let data = control::data_root()?;
    match args.get(1).map(String::as_str) {
        Some("--version") => println!("know-code {}", env!("CARGO_PKG_VERSION")),
        Some("runtime-info") if args.len()==2 => println!("{}",runtime::info()?),
        Some("cursors") => {
            let mut count = None;
            let mut bytes = None;
            for pair in args[2..].chunks(2) {
                if pair.len() != 2 { return Err("usage: know-code cursors [--max-count COUNT] [--retention-bytes BYTES]".into()); }
                match pair[0].as_str() {
                    "--max-count" => count = Some(pair[1].parse::<u32>()?),
                    "--retention-bytes" => bytes = Some(pair[1].parse::<u64>()?),
                    _ => return Err("expected --max-count or --retention-bytes".into()),
                }
            }
            let mut reply = know_code_core::cursor::settings(&data, None, Some(&std::env::current_exe()?))?;
            if count.is_some() || bytes.is_some() {
                let current = reply.settings.as_ref().ok_or("missing cursor settings")?;
                reply = know_code_core::cursor::settings(&data, Some(know_code_core::cursor::CursorLimits {
                    max_cursors: count.unwrap_or(current.max_cursors),
                    retention_bytes: bytes.unwrap_or(current.retention_bytes),
                }), None)?;
            }
            println!("{}", serde_json::to_string(&reply)?);
        }
        Some("install" | "update" | "rollback" | "uninstall" | "install-info" | "service") => println!("{}", install::run(&args[1..])?),
        Some("resources") => {
            let mut cpu = None;
            let mut memory = None;
            for pair in args[2..].chunks(2) {
                if pair.len() != 2 { return Err("usage: know-code resources [--cpu CORES] [--memory-mib MIB]".into()); }
                match pair[0].as_str() {
                    "--cpu" => cpu = Some(pair[1].parse::<f64>()?),
                    "--memory-mib" => memory = Some(pair[1].parse::<u64>()?),
                    _ => return Err("expected --cpu or --memory-mib".into()),
                }
            }
            let mut limits = control::resources(&data, None, Some(&std::env::current_exe()?))?;
            if cpu.is_some() || memory.is_some() {
                limits.cpu_cores = cpu.unwrap_or(limits.cpu_cores);
                limits.memory_mib = memory.unwrap_or(limits.memory_mib);
                limits = control::resources(&data, Some(limits), None)?;
            }
            println!("{}", serde_json::to_string(&limits)?);
        }
        Some("cancel") if args.len() == 3 => {
            control::ensure_running(&data, Some(&std::env::current_exe()?))?;
            control::require_resource_control(&data)?;
            let root = std::fs::canonicalize(&args[2]).unwrap_or(absolute(Path::new(&args[2]))?);
            let response = control::request(&data, ControlRequest { operation: "status".into(), ..Default::default() })?;
            let status = response.workspaces.iter().find(|s| Path::new(&s.workspace_root) == root).ok_or("workspace is not registered; inspect coordinator status")?;
            let result = control::workspace_operation(control::WorkspaceRequest {
                op: control::WorkspaceOperation::Cancel, database: Some(status.database.clone()), data_directory: Some(data),
                workspace_root: None, coordinator_executable: None, configuration: None, wait_ms: None, personal_settings: None,
            })?;
            println!("{}", serde_json::to_string(&result.status)?);
        }
        Some("query") if args.len() == 4 => println!("{}", know_code_core::query_json(&args[2], &args[3])),
        Some("settings") if args.len() == 3 || args.len() == 4 => {
            let personal_settings = args.get(3).map(|s| serde_json::from_str(s)).transpose()?;
            let status = coordinator::attach_workspace(&data, Path::new(&args[2]), String::new(), false)?;
            let result = control::workspace_operation(control::WorkspaceRequest {
                op: if personal_settings.is_some() { control::WorkspaceOperation::Configure } else { control::WorkspaceOperation::Settings }, database: Some(status.database), data_directory: Some(data),
                workspace_root: None, coordinator_executable: Some(std::env::current_exe()?), configuration: None, wait_ms: None,
                personal_settings,
            })?;
            println!("{}", serde_json::to_string(&result.status.requested_settings)?);
        }
        Some(operation @ ("index" | "watch")) if args.len() == 3 || args.len() == 5 => {
            let root = Path::new(&args[2]);
            let configuration = if args.len() == 5 && args[3] == "--configuration" { args[4].clone() }
                else if args.len() == 3 { String::new() } else { return Err("expected --configuration NAME".into()); };
            let target = coordinator::attach_workspace(&data, root, configuration, operation == "index")?;
            if operation == "watch" { println!("{}", serde_json::to_string(&target)?); }
            else {
                let completed = control::request(&data, ControlRequest {
                    operation: "wait".into(), database: target.database.clone(), wait_ms: 120000, ..Default::default()
                })?;
                let status = completed.workspaces.first().ok_or("workspace disappeared during refresh")?;
                if status.generation == 0 || status.published_epoch < target.observed_epoch {
                    return Err(format!("index not ready ({}): {}", status.phase, status.error).into());
                }
                println!("{}", serde_json::json!({"database": status.database, "generation": status.generation}));
            }
        }
        Some("mcp") => {
            let (workspace, configuration) = match args.as_slice() {
                [_, _, flag, workspace] if flag == "--workspace" => (workspace, String::new()),
                [_, _, flag, workspace, option, configuration] if flag == "--workspace" && option == "--configuration" => (workspace, configuration.clone()),
                _ => return Err("usage: know-code mcp --workspace PATH [--configuration NAME]".into()),
            };
            let status = coordinator::attach_workspace(&data, Path::new(workspace), configuration, false)?;
            mcp::run(data, status)?;
        }
        Some("unwatch") if args.len()==3 => {
            control::request(&data,ControlRequest{operation:"unregister".into(),workspace_root:absolute(Path::new(&args[2]))?.to_string_lossy().into_owned(),..Default::default()})?;
        }
        Some("coordinator") => match args.get(2).map(String::as_str) {
            Some("run") => coordinator::run(&data, &runtime::locate()?)?,
            Some("status") => println!("{}", serde_json::to_string(&control::request(&data, ControlRequest { operation: "status".into(), ..Default::default() })?)?),
            Some("stop") => { control::request(&data, ControlRequest { operation: "shutdown".into(), ..Default::default() })?; }
            Some("login-plist") => print!("{}", coordinator::login_plist(&std::env::current_exe()?, &absolute(&data)?)),
            _ => return Err("usage: know-code coordinator run|status|stop|login-plist".into()),
        },
        _ => return Err("usage: know-code resources [--cpu CORES] [--memory-mib MIB] | cancel WORKSPACE | settings WORKSPACE [JSON] | index|watch WORKSPACE [--configuration NAME] | query DATABASE JSON | mcp --workspace PATH [--configuration NAME] | coordinator run|status|stop|login-plist | install|update [--from BUNDLE] [--prefix DIR] | rollback|uninstall|install-info [--prefix DIR] | service enable|disable|status [--prefix DIR]".into()),
    }
    Ok(())
}
fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
