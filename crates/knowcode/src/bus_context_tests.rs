//! A bus message type's context (fix 4): `callers` holds the uses inside its publishers and consumers, tagged with
//! their bus role; its other uses (constructions in a mapper or a test builder, type mentions) are `users`.

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, Scope};
use serde_json::Value;

const MAPPERS: usize = 5;

fn ledgerd() -> Vec<(String, String)> {
    let mapper: String = (0..MAPPERS).map(|k| format!("    public InvoiceIssued Map{k}() => new InvoiceIssued();\n")).collect();
    vec![
        (
            "src/Ledgerd.Models.csproj".into(),
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Ledgerd.Models</PackageId></PropertyGroup>\n</Project>\n".into(),
        ),
        ("src/InvoiceIssued.cs".into(), "namespace Ledgerd.Models.Events;\n[ShopMessage(ShopHosts.Billing)]\npublic class InvoiceIssued { }\n".into()),
        (
            "src/Billing.cs".into(),
            "using Ledgerd.Models.Events;\nnamespace Ledgerd.Service;\npublic class Billing\n{\n    private readonly IShopBus _bus;\n    public async Task Issue()\n    {\n        await _bus.Publish(new InvoiceIssued());\n    }\n}\n".into(),
        ),
        ("src/InvoiceMapper.cs".into(), format!("using Ledgerd.Models.Events;\nnamespace Ledgerd.Service;\npublic class InvoiceMapper\n{{\n{mapper}}}\n")),
    ]
}

fn mailer() -> Vec<(String, String)> {
    vec![
        (
            "src/Mailer.csproj".into(),
            "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Ledgerd.Models\" Version=\"1.*\" /></ItemGroup>\n</Project>\n".into(),
        ),
        (
            "src/InvoiceIssuedConsumer.cs".into(),
            "using Ledgerd.Models.Events;\nnamespace Mailer.Consumers;\npublic class InvoiceIssuedConsumer : ShopConsumer<InvoiceIssued, InvoiceIssuedConsumer>\n{\n    public override Task Consume(InvoiceIssued message, ConsumeContext<InvoiceIssued> context) => null;\n}\n".into(),
        ),
    ]
}

fn as_refs(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect()
}

fn rows(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(|r| format!("{}:{}:{}", r["file"].as_str().unwrap_or(""), r["line"], r["from"]["name"].as_str().unwrap_or(""))).collect()).unwrap_or_default()
}

#[test]
fn a_message_types_publishers_are_its_callers_and_its_other_uses_are_users() {
    let cache = test_cache();
    let base = cache.dir("bus-context");
    let l = ledgerd();
    let m = mailer();
    let ledger = repo(&base, "ledgerd", &as_refs(&l));
    repo(&base, "mailer", &as_refs(&m));
    let scope = Scope { repos: vec![ledger.clone()], no_sync: false };
    let whole = ops::context("InvoiceIssued", 40_000, Default::default(), &scope).unwrap().env;
    let item = &whole["items"][0];
    // The publish call, tagged; the mapper's constructions and type mentions apart, none dropped.
    assert_eq!(rows(&item["callers"]), ["src/Billing.cs:8:Issue"], "{whole}");
    assert_eq!(item["callers"][0]["bus"], "publisher", "{whole}");
    let users = rows(&item["users"]);
    assert_eq!((users.len(), item["users_total"].as_u64()), (2 * MAPPERS, Some(2 * MAPPERS as u64)), "{whole}");
    assert!(users.iter().all(|u| u.starts_with("src/InvoiceMapper.cs:")), "{users:?}");
    assert!(item["users_note"].as_str().is_some(), "{whole}");
    assert_eq!(whole["complete"], true, "{whole}");
    // A page of 3: the users continue by their own cursor, to exactly the rest, in order.
    let first = ops::context("InvoiceIssued", 360, Default::default(), &scope).unwrap().env;
    let item = &first["items"][0];
    assert_eq!(rows(&item["callers"]), ["src/Billing.cs:8:Issue"], "{first}");
    assert_eq!((rows(&item["users"]).len(), item["users_total"].as_u64()), (3, Some(2 * MAPPERS as u64)), "{first}");
    // Cut like `callers`, whose rows they were: a list on the side, more to read, the answer still whole.
    assert_eq!(first["complete"], true, "{first}");
    assert!(crate::contract::more_of(&first).iter().any(|m| m.detail.contains("users")), "{first}");
    let mut got = rows(&item["users"]);
    let mut next = item["users_cursor"].as_str().map(str::to_string);
    assert!(next.is_some(), "{first}");
    while let Some(c) = next {
        let page = crate::cursor::more(&c).unwrap();
        assert_eq!(page["list"], "items[0].users", "{page}");
        got.extend(rows(&page["items"]));
        next = page["cursor"].as_str().map(str::to_string);
        assert!(got.len() <= 2 * MAPPERS, "{page}");
    }
    assert_eq!(got, users);
    // A type the bus doesn't carry: callers as before, no `users`.
    let plain = ops::context("InvoiceMapper", 40_000, Default::default(), &scope).unwrap().env;
    assert!(plain["items"][0].get("users").is_none(), "{plain}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A page of a workspace answer's nested list says the repo of the item the list belongs to, not "workspace": its rows
/// name a repo only when it is another, as on the first page.
#[test]
fn a_page_of_a_workspace_answers_nested_list_says_its_items_repo() {
    let cache = test_cache();
    let base = cache.dir("bus-context-page-repo");
    let ledger = repo(&base, "ledgerd", &as_refs(&ledgerd()));
    let mailer = repo(&base, "mailer", &as_refs(&mailer()));
    let scope = Scope { repos: vec![ledger, mailer], no_sync: false };
    let first = ops::context("InvoiceIssued", 360, Default::default(), &scope).unwrap().env;
    assert_eq!(first["repo"], "workspace", "{first}");
    let item = &first["items"][0];
    assert_eq!(item["repo"], "ledgerd", "{first}");
    let c = item["users_cursor"].as_str().unwrap_or_else(|| panic!("{first}"));
    let page = crate::cursor::more(c).unwrap();
    assert_eq!((page["list"].as_str(), page["repo"].as_str()), (Some("items[0].users"), Some("ledgerd")), "{page}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A workspace answer's cursors name every repo it read, so each list the size cap cuts gains a cursor many times the
/// size of a row; the cap measures them as their handles. A message type's bus links (the answer) stay whole while
/// its side lists (callers, members, users) are cut and continue by cursor.
#[test]
fn cutting_a_side_list_of_a_workspace_answer_keeps_the_bus_links() {
    use serde_json::json;
    let link = |i: usize| json!({"confidence": "exact", "file": format!("src/Consumer{i}/Consumers/InvoiceIssuedConsumer.cs"), "line": 20, "repo": format!("Shop.Service{i}"), "role": "consumer", "symbol": "InvoiceIssuedConsumer.Consume"});
    let used = |i: usize| json!({"cite": {"check": "verified", "text": "await _bus.Publish(new InvoiceIssued(_mapper.Map<Ledgerd.Models.Entities.Invoice>(entity)));"}, "confidence": "unique_name", "evidence": "name_unique", "file": format!("src/Service/Billing{i}.cs"), "from": {"name": "Issue"}, "kind": "instantiate", "line": 54});
    let mut env = json!({"complete": true, "repo": "workspace", "items": [{
        "bus": [{"file": "src/Models/Events/InvoiceIssued.cs", "line": 6, "links": (0..8).map(link).collect::<Vec<_>>(), "message": "Ledgerd.Models.Events.InvoiceIssued", "queue": "Billing", "resolution": "exact", "role": "message", "symbol": "InvoiceIssued"}],
        "callers": (0..5).map(used).collect::<Vec<_>>(), "callers_summary": {"by_confidence": {"unique_name": 5}, "total": 5},
        "members": (0..10).map(|i| json!({"file": "src/Models/Events/InvoiceIssued.cs", "kind": "property", "line": 7 + i, "name": format!("Field{i}")})).collect::<Vec<_>>(), "members_total": 10,
        "users": (0..8).map(used).collect::<Vec<_>>(), "users_total": 8,
        "repo": "Ledgerd", "source": "public record InvoiceIssued(Invoice Invoice);", "source_mode": "full",
        "symbol": {"file": "src/Models/Events/InvoiceIssued.cs", "kind": "class", "line": 6, "name": "InvoiceIssued"}}]});
    // A workspace of 300 checkouts, each with a long path: every cursor names them all.
    let repos: Vec<std::path::PathBuf> = (0..300).map(|i| format!("/home/someone/src/shop/checkouts/a-fairly-long-folder-name/Shop.Service{i}").into()).collect();
    let scope = Scope { repos, no_sync: false };
    let lists = ["callers", "users", "members", "bus/0/links"].iter().map(|l| crate::cursor::List::new(format!("/items/0/{l}"), 25, false)).collect();
    let req = crate::cursor::Request::Context { symbol: "InvoiceIssued".into(), budget: 6000, source: "full".into(), limit: None };
    crate::cursor::offer(&mut env, req, &scope, lists, &[]);
    let out = crate::mcp::cap_full(env, 6000);
    let item = &out["items"][0];
    assert_eq!(item["bus"][0]["links"].as_array().map(Vec::len), Some(8), "{out}");
    // The side lists were cut, and continue.
    assert!(item["users"].as_array().is_some_and(|u| u.len() < 8) && item["users_cursor"].is_string(), "{out}");
}

/// Cut to fit, a message type's bus links (its publishers and consumers in other repos) continue by `links_cursor`
/// to exactly the links cut, and `users_note` goes before any of them.
#[test]
fn a_message_types_bus_links_cut_to_fit_continue_by_cursor() {
    const CONSUMERS: usize = 6;
    let cache = test_cache();
    let base = cache.dir("bus-links");
    let l = ledgerd();
    let mut roots = vec![repo(&base, "ledgerd", &as_refs(&l))];
    for k in 0..CONSUMERS {
        let mut m = mailer();
        m[1].1 = m[1].1.replace("namespace Mailer.Consumers", &format!("namespace Mailer{k}.Consumers"));
        roots.push(repo(&base, &format!("mailer{k}"), &as_refs(&m)));
    }
    let scope = Scope { repos: roots, no_sync: false };
    let whole = ops::context("InvoiceIssued", 40_000, Default::default(), &scope).unwrap().env;
    let link_rows = |v: &Value| v.as_array().map(|a| a.iter().map(|r| format!("{}:{}:{}", r["repo"].as_str().unwrap_or(""), r["file"].as_str().unwrap_or(""), r["line"])).collect::<Vec<_>>()).unwrap_or_default();
    let all = link_rows(&whole["items"][0]["bus"][0]["links"]);
    assert_eq!(all.iter().filter(|r| r.starts_with("mailer")).count(), CONSUMERS, "{whole}");
    assert!(whole["items"][0]["users_note"].is_string(), "{whole}");
    // Small enough that the side lists go and the links are cut.
    let max = 1_200;
    let out = crate::mcp::cap_full(whole, max);
    let item = &out["items"][0];
    let shown = link_rows(&item["bus"][0]["links"]);
    assert!(!shown.is_empty() && shown.len() < all.len(), "{out}");
    assert!(item.get("users_note").is_none(), "{out}");
    // Cut, the answer says so, and the rest is one cursor away: exactly the links cut, in order.
    assert_eq!(out["complete"], false, "{out}");
    let mut got = shown;
    let mut next = item["bus"][0]["links_cursor"].as_str().map(str::to_string);
    assert!(next.is_some(), "{out}");
    while let Some(c) = next {
        let page = crate::cursor::more(&c).unwrap();
        assert_eq!(page["list"], "items[0].bus[0].links", "{page}");
        got.extend(link_rows(&page["items"]));
        next = page["cursor"].as_str().map(str::to_string);
        assert!(got.len() <= all.len(), "{page}");
    }
    assert_eq!(got, all);
    let _ = std::fs::remove_dir_all(&base);
}
