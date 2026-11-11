mod coordinator;
mod discovery;
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
        Some("parents") => {
            let action=args.get(2).ok_or("usage: know-code parents add PATH [--exclude RELATIVE_PATH ...] | list | scan PATH | remove PATH")?;
            if !matches!(action.as_str(),"add"|"list"|"scan"|"remove") {return Err("parents action must be add, list, scan or remove".into());}
            let mut request=know_code_core::protocol::ParentDiscoveryRequest{action:action.clone(),..Default::default()};
            if action=="list" { if args.len()!=3 {return Err("parents list takes no arguments".into());} }
            else {
                request.root=absolute(Path::new(args.get(3).ok_or("parent path is required")?))?.to_string_lossy().into_owned();
                for pair in args[4..].chunks(2) {
                    if action!="add" || pair.len()!=2 || pair[0]!="--exclude" {return Err("only parents add accepts repeated --exclude RELATIVE_PATH".into());}
                    request.exclusions.push(pair[1].clone());
                }
            }
            control::ensure_running(&data,Some(&std::env::current_exe()?))?;
            let capabilities=control::request(&data,ControlRequest{operation:"status".into(),..Default::default()})?;
            if capabilities.parent_discovery_version!=discovery::VERSION {return Err("update required: parent discovery is unavailable or incompatible; update runtime and clients together".into());}
            let response=control::request(&data,ControlRequest{operation:"parents".into(),parent_discovery:Some(request),..Default::default()})?;
            if response.parent_discovery_version!=discovery::VERSION {return Err("update required: parent discovery is unavailable or incompatible; update runtime and clients together".into());}
            println!("{}",serde_json::to_string(&response)?);
        }
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
        Some("queries") => {
            let mut maximum = None;
            let mut scope = None;
            for pair in args[2..].chunks(2) {
                if pair.len() != 2 {
                    return Err("usage: know-code queries [--max-active COUNT] [--scope DIRECTORY]".into());
                }
                match pair[0].as_str() {
                    "--max-active" => maximum = Some(pair[1].parse::<u32>()?),
                    "--scope" => scope = Some(know_code_core::admission::AdmissionScope::at(&pair[1])?),
                    _ => return Err("expected --max-active or --scope".into()),
                }
            }
            let scope = scope.map(Ok).unwrap_or_else(know_code_core::admission::AdmissionScope::local)?;
            let policy = match maximum {
                Some(maximum) => scope.configure(maximum),
                None => scope.policy(),
            };
            match policy {
                Ok(policy) => println!("{}", serde_json::json!({"result":policy})),
                Err(error) => println!("{}", serde_json::json!({"error":{"code":error.code(),"message":error.to_string()}})),
            }
        }
        Some("install" | "update" | "rollback" | "uninstall" | "install-info" | "service") => println!("{}", install::run(&args[1..])?),
        Some("resources") => {
            let mut cpu = None;
            let mut memory = None;
            let mut rebuild_disk = None;
            let mut min_free_disk = None;
            for pair in args[2..].chunks(2) {
                if pair.len() != 2 { return Err("usage: know-code resources [--cpu CORES] [--memory-mib MIB] [--rebuild-disk-mib MIB] [--min-free-disk-mib MIB]".into()); }
                match pair[0].as_str() {
                    "--cpu" => cpu = Some(pair[1].parse::<f64>()?),
                    "--memory-mib" => memory = Some(pair[1].parse::<u64>()?),
                    "--rebuild-disk-mib" => rebuild_disk = Some(pair[1].parse::<u64>()?),
                    "--min-free-disk-mib" => min_free_disk = Some(pair[1].parse::<u64>()?),
                    _ => return Err("expected --cpu, --memory-mib, --rebuild-disk-mib or --min-free-disk-mib".into()),
                }
            }
            let mut limits = control::resources(&data, None, Some(&std::env::current_exe()?))?;
            if cpu.is_some() || memory.is_some() || rebuild_disk.is_some() || min_free_disk.is_some() {
                limits.cpu_cores = cpu.unwrap_or(limits.cpu_cores);
                limits.memory_mib = memory.unwrap_or(limits.memory_mib);
                limits.rebuild_disk_mib = rebuild_disk.unwrap_or(limits.rebuild_disk_mib);
                limits.min_free_disk_mib = min_free_disk.unwrap_or(limits.min_free_disk_mib);
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
            let root=std::fs::canonicalize(&args[2]).unwrap_or(absolute(Path::new(&args[2]))?);
            control::request(&data,ControlRequest{operation:"unregister".into(),workspace_root:root.to_string_lossy().into_owned(),..Default::default()})?;
        }
        Some("coordinator") => match args.get(2).map(String::as_str) {
            Some("run") => coordinator::run(&data, &runtime::locate()?)?,
            Some("status") => println!("{}", serde_json::to_string(&control::request(&data, ControlRequest { operation: "status".into(), ..Default::default() })?)?),
            Some("stop") => { control::request(&data, ControlRequest { operation: "shutdown".into(), ..Default::default() })?; }
            Some("login-plist") => print!("{}", coordinator::login_plist(&std::env::current_exe()?, &absolute(&data)?)),
            _ => return Err("usage: know-code coordinator run|status|stop|login-plist".into()),
        },
        _ => return Err("usage: know-code resources [--cpu CORES] [--memory-mib MIB] [--rebuild-disk-mib MIB] [--min-free-disk-mib MIB] | cancel WORKSPACE | settings WORKSPACE [JSON] | index|watch WORKSPACE [--configuration NAME] | query DATABASE JSON | mcp --workspace PATH [--configuration NAME] | coordinator run|status|stop|login-plist | install|update [--from BUNDLE] [--prefix DIR] | rollback|uninstall|install-info [--prefix DIR] | service enable|disable|status [--prefix DIR]".into()),
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
