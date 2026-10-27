//! A size eviction must not turn a cross-repo answer into a confidently empty result.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn run(base: &Path, args: &[&str]) -> String {
    let out = Command::new(KC).args(args).current_dir(base)
        .env("KNOWCODE_CACHE_DIR", base.join("cache")).env("KNOWCODE_CONFIG_DIR", base.join("config"))
        .env("KNOWCODE_DATA_DIR", base.join("data")).output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn invariant_an_evicted_http_backend_is_restored_without_manual_indexing() {
    let base = std::env::temp_dir().join(format!("knowcode-test-cache-http-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let api = base.join("api");
    let web = base.join("web");
    for root in [&api, &web] {
        fs::create_dir_all(root).unwrap();
        assert!(Command::new("git").args(["init", "-q"]).arg(root).status().unwrap().success());
    }
    write(&api, "src/OrdersController.cs", "public class OrdersController : Controller {\n [HttpPost(\"Orders\")]\n public Task Orders() => null;\n}\n");
    // The web app's request contracts are a made-up code base's convention, which the config names.
    write(&base, "config/config.toml", kc_index::config::SYNTHETIC_CONVENTIONS);
    write(&web, "src/Api.ts", "export class Api { static readonly orders = 'Orders'; }\n");
    write(&web, "src/contracts.ts", "@RequestContract({ endpoint: Api.orders, httpMethod: HttpMethods.post })\nexport class ApiPlaceOrder {}\n");
    write(&web, "src/order.service.ts", "export class OrderService {\n constructor(private http: HttpService) {}\n load(): void { this.http.send(new ApiPlaceOrder()); }\n}\n");
    run(&base, &["index", api.to_str().unwrap(), web.to_str().unwrap(), "--threads", "1"]);
    let query = ["context", "OrderService.load", "--repo", web.to_str().unwrap(), "--budget", "10000", "--json"];
    let before: Value = serde_json::from_str(&run(&base, &query)).unwrap();
    assert_eq!(before["complete"], true, "{before}");
    assert!(before.to_string().contains("OrdersController.Orders"), "{before}");

    // This binary contains one test, so environment setup precedes all in-process index APIs.
    unsafe {
        std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
        std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
        std::env::set_var("KNOWCODE_DATA_DIR", base.join("data"));
    }
    kc_index::config::set("cache.max_size", "1").unwrap();
    let api = kc_index::repo_root(&api).unwrap();
    let web = kc_index::repo_root(&web).unwrap();
    let report = kc_index::cache::maintain(std::slice::from_ref(&web)).unwrap();
    assert!(report.removed.iter().any(|r| r.root == api), "{report:?}");
    assert!(!kc_index::db_path(&api).unwrap().exists());

    let restored: Value = serde_json::from_str(&run(&base, &query)).unwrap();
    assert!(restored.to_string().contains("OrdersController.Orders"), "eviction lost the known backend: {restored}");
    if restored["complete"] == false {
        assert!(restored["incomplete"].as_array().unwrap().iter().any(|i| i["reason"] == "stale"), "{restored}");
    }
    for provenance in restored["provenance"].as_array().unwrap() {
        if let Some(generation) = provenance["generation"].as_str().filter(|g| g.starts_with("summary:")) {
            assert!(!generation.contains('|'), "summary generation must not include internal paths or policy keys: {provenance}");
        }
    }
    assert!(kc_index::db_path(&api).unwrap().exists(), "the referenced backend should be restored automatically");
    let fresh: Value = serde_json::from_str(&run(&base, &query)).unwrap();
    assert_eq!(fresh["complete"], true, "{fresh}");
    assert!(fresh.to_string().contains("OrdersController.Orders"));
    let _ = fs::remove_dir_all(base);
}
