//! A SQL table's context in a workspace reads as many of the repos naming it as a page lists (fix 5(b)): its
//! `used_by` says when it read only some, and its cursor reads on, reaching every repo once.

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, Scope};
use serde_json::Value;

const USERS: usize = 7;

fn user(i: usize) -> (String, String) {
    (
        "src/Ledger.cs".to_string(),
        format!(
            "namespace Svc{i};\npublic class Order {{ }}\npublic class ShopContext : DbContext\n{{\n    public DbSet<Order> Orders {{ get; set; }}\n}}\n\
public class OrderStore{i}\n{{\n    private readonly ShopContext _db;\n    public void Save(Order o)\n    {{\n        _db.Orders.Add(o);\n        _db.SaveChanges();\n    }}\n}}\n"
        ),
    )
}

fn repos_of(rows: &[Value]) -> Vec<String> {
    rows.iter().filter_map(|r| r["repo"].as_str().map(str::to_string)).collect()
}

#[test]
fn a_workspace_tables_users_continue_to_every_repo_once() {
    let cache = test_cache();
    let base = cache.dir("table-context");
    let schema = repo(&base, "schema", &[("src/shop.sql", "CREATE TABLE `orders` (\n  `id` int NOT NULL\n);\n")]);
    let mut names = Vec::new();
    for i in 0..USERS {
        let (path, text) = user(i);
        repo(&base, &format!("svc{i}"), &[(path.as_str(), text.as_str())]);
        names.push(format!("svc{i}"));
    }
    let scope = Scope { repos: vec![schema.clone()], no_sync: false };
    // Every repo read: as before, nothing cut.
    let all = ops::context("orders", 40_000, Default::default(), &scope).unwrap().env;
    let sql = &all["items"][0]["sql"];
    let mut every = repos_of(sql["used_by"].as_array().unwrap_or_else(|| panic!("{all}")));
    every.sort();
    assert_eq!(every, names, "{all}");
    assert!(sql.get("used_by_truncated").is_none() && sql.get("used_by_note").is_none(), "{all}");
    // A budget listing 3 callers reads 3 repos, says so, and continues.
    let first = ops::context("orders", 360, Default::default(), &scope).unwrap().env;
    let sql = &first["items"][0]["sql"];
    assert_eq!(sql["used_by"].as_array().map(Vec::len), Some(3), "{first}");
    assert_eq!(sql["used_by_truncated"], true, "{first}");
    assert!(sql["used_by_note"].as_str().is_some_and(|n| n.starts_with("read 3 of the 7 repos naming orders")), "{first}");
    assert_eq!(first["complete"], false, "{first}");
    let mut got = repos_of(sql["used_by"].as_array().unwrap());
    let mut next = sql["used_by_cursor"].as_str().map(str::to_string);
    assert!(next.is_some(), "{first}");
    let mut pages = 0;
    while let Some(c) = next {
        let page = crate::cursor::more(&c).unwrap();
        assert_eq!(page["list"], "items[0].sql.used_by", "{page}");
        got.extend(repos_of(page["items"].as_array().unwrap_or_else(|| panic!("{page}"))));
        next = page["cursor"].as_str().map(str::to_string);
        pages += 1;
        assert!(pages < 10, "cursors never end");
    }
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(sorted, names, "every repo once: {got:?}");
    let _ = std::fs::remove_dir_all(&base);
}
