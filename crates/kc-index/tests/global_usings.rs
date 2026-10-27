//! What a C# file sees besides its own `using`s: its project's `global using` directives (a GlobalUsings.cs), the
//! project file's `<Using>` items, and the SDK's implicit usings. Imports that can't be read rule nothing out.

use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

/// (file, confidence, targets' files) of the calls of `name`.
fn calls_of(store: &Store, name: &str) -> Vec<(String, &'static str, Vec<String>)> {
    let (hits, _) = query::refs(store.conn(), name, &["call"], 50).unwrap();
    hits.iter().map(|h| (h.file.clone(), h.confidence, h.targets.iter().map(|t| t.file.clone()).collect())).collect()
}

#[test]
fn a_projects_global_and_implicit_usings_are_seen_by_its_files() {
    let root = std::env::temp_dir().join(format!("knowcode-test-global-usings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&root).status().unwrap().success());
    let files = [
        // Two same-named services in two namespaces each: which one a file means is what it imports.
        ("src/Domain/IRepo.cs", "namespace Shop.Domain;\npublic interface IRepo { void Load(int id); }\n"),
        ("src/Legacy/IRepo.cs", "namespace Shop.Legacy;\npublic interface IRepo { void Load(int id); }\n"),
        ("src/Billing/IInvoices.cs", "namespace Shop.Billing;\npublic interface IInvoices { void Send(int id); }\n"),
        ("src/Legacy2/IInvoices.cs", "namespace Shop.Legacy;\npublic interface IInvoices { void Send(int id); }\n"),
        // The API project imports `Shop.Domain` in a GlobalUsings.cs, and `Shop.Billing` with a `<Using>` item.
        (
            "src/Api/Api.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <PropertyGroup><ImplicitUsings>enable</ImplicitUsings></PropertyGroup>\n  \
             <ItemGroup><Using Include=\"Shop.Billing\" /></ItemGroup>\n  <ItemGroup><ProjectReference Include=\"..\\Domain\\Domain.csproj\" /><ProjectReference Include=\"..\\Legacy\\Legacy.csproj\" /><ProjectReference Include=\"..\\Billing\\Billing.csproj\" /><ProjectReference Include=\"..\\Legacy2\\Legacy2.csproj\" /></ItemGroup>\n</Project>\n",
        ),
        ("src/Api/GlobalUsings.cs", "global using Shop.Domain;\n"),
        (
            "src/Api/Handler.cs",
            "namespace Shop.Api;\npublic class Handler\n{\n    private readonly IRepo _repo;\n    private readonly IInvoices _invoices;\n    \
             public void Run() { _repo.Load(1); _invoices.Send(2); }\n}\n",
        ),
        // A project whose implicit usings a property decides: what its files import can't be read.
        ("src/Jobs/Jobs.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><ImplicitUsings>$(UseImplicit)</ImplicitUsings></PropertyGroup>\n  <ItemGroup><ProjectReference Include=\"..\\Domain\\Domain.csproj\" /><ProjectReference Include=\"..\\Legacy\\Legacy.csproj\" /><ProjectReference Include=\"..\\Billing\\Billing.csproj\" /><ProjectReference Include=\"..\\Legacy2\\Legacy2.csproj\" /></ItemGroup>\n</Project>\n"),
        ("src/Jobs/Job.cs", "namespace Shop.Jobs;\npublic class Job\n{\n    private readonly IRepo _repo;\n    public void Tick() { _repo.Load(3); }\n}\n"),
        ("src/Domain/Domain.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("src/Legacy/Legacy.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("src/Billing/Billing.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("src/Legacy2/Legacy2.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
    ];
    for (path, text) in files {
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join(path), text).unwrap();
    }
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let load = calls_of(&store, "Load");
    let send = calls_of(&store, "Send");
    let api = |calls: &[(String, &'static str, Vec<String>)]| calls.iter().find(|c| c.0 == "src/Api/Handler.cs").cloned().unwrap();
    // The global using picks `Shop.Domain.IRepo`; the `<Using>` item `Shop.Billing.IInvoices`.
    assert_eq!(api(&load).2, ["src/Domain/IRepo.cs"], "{load:?}");
    assert_ne!(api(&load).1, "ambiguous", "{load:?}");
    assert_eq!(api(&send).2, ["src/Billing/IInvoices.cs"], "{send:?}");
    // Imports that can't be read rule nothing out: never one confident pick.
    let job = load.iter().find(|c| c.0 == "src/Jobs/Job.cs").unwrap();
    assert_eq!(job.1, "ambiguous", "{job:?}");
    // And the project is named as one whose imports aren't read, for answers to say so.
    let unread = kc_store::unsupported::in_area(store.conn(), "imports").unwrap();
    let at: Vec<(&str, &str)> = unread.iter().map(|u| (u.file.as_str(), u.construct.as_str())).collect();
    assert_eq!(at, [("src/Jobs/Jobs.csproj", "<ImplicitUsings>$(UseImplicit)</ImplicitUsings>")]);
    let _ = std::fs::remove_dir_all(&root);
}
