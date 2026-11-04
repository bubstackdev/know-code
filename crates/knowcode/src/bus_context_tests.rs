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
