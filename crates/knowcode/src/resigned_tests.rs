//! A diff that changes a C# method's parameters still reaches the code calling it as it was: a call written for the
//! old signature no longer binds to the method in the changed tree, so the working tree's graph alone has no edge
//! from it. The base commit says which signature the method had, and its callers are matched by name, receiver and
//! argument count, as code naming a removed definition is.

use crate::ops::tests::{commit, repo, test_cache};
use crate::ops::{self, ImpactParams, Scope};
use crate::review::{ReviewParams, review};

const CASING: &str = "namespace Fmt;\npublic static class Casing\n{\n\
    \x20   public static string Format(string value, string format) { return value; }\n}\n";
const WRITER: &str = "namespace Fmt;\npublic class Writer\n{\n\
    \x20   public string Render(string v) { return Casing.Format(v, \"u\"); }\n\
    \x20   public string Plain(string v) { return v; }\n}\n";
const WRITER_TESTS: &str = "namespace Fmt.Tests;\npublic class WriterTests\n{\n\
    \x20   public void Renders() { new Writer().Render(\"a\"); }\n}\n";

fn diff_impact(root: &std::path::Path) -> ops::ImpactRepo {
    let p = ImpactParams { diff: Some("working".into()), depth: 3, limit: 100, ..Default::default() };
    ops::impact(&p, &Scope { repos: vec![root.to_path_buf()], no_sync: false }).unwrap().data.remove(0)
}

/// `(qualified, via, via_removed, confidence)` of the hop-1 affected symbols outside the changed file, sorted.
fn hop1(r: &ops::ImpactRepo, changed: &str) -> Vec<(String, String, bool, &'static str)> {
    let mut v: Vec<_> = r
        .impact
        .affected
        .iter()
        .filter(|a| a.hop == 1 && a.file != changed)
        .map(|a| (a.qualified.clone(), a.via.clone(), a.via_removed, a.confidence))
        .collect();
    v.sort();
    v
}

/// The serilog case review missed: a parameter added without a default. The two-argument call no longer fits, so it is
/// a dependent through the old signature (`via_removed`), named by its type: `unique_name`, never `exact`. The method
/// stays a changed symbol, and is listed with the signature it had, not as removed.
#[test]
fn a_call_written_for_the_old_signature_is_a_dependent_of_the_change() {
    let cache = test_cache();
    let base = cache.dir("resigned-added-param");
    let app = repo(&base, "fmt", &[("src/Casing.cs", CASING), ("src/Writer.cs", WRITER), ("tests/WriterTests.cs", WRITER_TESTS)]);
    commit(&app, "init");
    std::fs::write(app.join("src/Casing.cs"), CASING.replace("string format)", "string format, int width)")).unwrap();

    let r = diff_impact(&app);
    assert_eq!(hop1(&r, "src/Casing.cs"), [("Writer.Render".to_string(), "Casing.Format".to_string(), true, "unique_name")], "{:?}", r.impact.affected);
    let g = r.removed.iter().find(|g| g.qualified == "Casing.Format").expect("Casing.Format's old signature listed");
    assert_eq!(g.old_signature.as_deref(), Some("public static string Format(string value, string format)"));
    assert_eq!(g.named_by.iter().map(|n| (n.qualified.as_str(), n.site.as_str())).collect::<Vec<_>>(), [("Writer.Render", "src/Writer.cs:4")]);
    assert!(r.impact.scope.iter().any(|s| s.qualified == "Casing.Format"), "still a changed symbol");
    let text = crate::render::impact_text(std::slice::from_ref(&r), false);
    assert!(text.contains("changed signature of method Casing.Format"), "{text}");
    assert!(text.contains("(calls Casing.Format as it was before the change)"), "{text}");

    // Review lists the caller's file as a dependent, the test reaching it as a test to run, and the method as
    // changed (not removed).
    let out = review(&ReviewParams::new("working".into()), &Scope { repos: vec![app.clone()], no_sync: false }).unwrap();
    let files = |k: &str| out.env[k].as_array().unwrap().iter().map(|v| v["file"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(files("dependents"), ["src/Writer.cs"], "{}", out.env);
    assert!(files("tests").contains(&"tests/WriterTests.cs".to_string()), "{}", out.env);
    let change = &out.env["changes"][0];
    assert_eq!(change["file"], "src/Casing.cs");
    assert!(change["symbols"].as_array().unwrap().iter().all(|s| s.get("change").is_none()), "{change}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A parameter added with a default leaves the old call fitting the method: the graph binds it as before (a static call
/// through the type's name, `unique_name`), and it is no dependent through an old signature.
#[test]
fn a_call_that_still_fits_the_new_signature_is_an_ordinary_caller() {
    let cache = test_cache();
    let base = cache.dir("resigned-optional-param");
    let app = repo(&base, "fmt", &[("src/Casing.cs", CASING), ("src/Writer.cs", WRITER)]);
    commit(&app, "init");
    std::fs::write(app.join("src/Casing.cs"), CASING.replace("string format)", "string format, int width = 0)")).unwrap();
    let r = diff_impact(&app);
    assert_eq!(hop1(&r, "src/Casing.cs"), [("Writer.Render".to_string(), "Casing.Format".to_string(), false, "unique_name")], "{:?}", r.impact.affected);
    assert!(r.removed.is_empty(), "{:?}", r.removed);
    let _ = std::fs::remove_dir_all(&base);
}

/// The `Padding.Apply` variant: of two overloads, the changed one's old calls now fit the other by count. Which one
/// such a call meant before can't be told by count alone, so it is a dependent through the old signature, `ambiguous`,
/// and never an `exact` caller of either.
#[test]
fn an_old_call_that_now_fits_another_overload_is_ambiguous() {
    let cache = test_cache();
    let base = cache.dir("resigned-overload");
    let padding = "namespace Fmt;\npublic static class Padding\n{\n\
        \x20   public static string Apply(string value, int width) { return value; }\n\
        \x20   public static string Apply(object value, int width) { return value.ToString(); }\n}\n";
    let user = "namespace Fmt;\npublic class Table\n{\n\
        \x20   public string Cell(string v) { return Padding.Apply(v, 4); }\n}\n";
    let app = repo(&base, "fmt", &[("src/Padding.cs", padding), ("src/Table.cs", user)]);
    commit(&app, "init");
    std::fs::write(app.join("src/Padding.cs"), padding.replace("Apply(string value, int width)", "Apply(string value, int width, char fill)")).unwrap();
    let r = diff_impact(&app);
    let cell: Vec<_> = r.impact.affected.iter().filter(|a| a.qualified == "Table.Cell").map(|a| (a.via_removed, a.confidence)).collect();
    assert_eq!(cell, [(true, "ambiguous")], "{:?}", r.impact.affected);
    let _ = std::fs::remove_dir_all(&base);
}
