//! Maven and Gradle modules, read from `pom.xml` and Gradle scripts as text: each project is a declared module at its
//! directory (a Gradle settings script's `include`s name them too), and each package carries the coordinates other
//! builds depend on it by.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;

fn repo(name: &str, files: &[(&str, &str)]) -> (PathBuf, Store) {
    let dir = std::env::temp_dir().join(format!("knowcode-test-jvm-modules-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let mut store = Store::open(&dir.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&dir, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    (dir, store)
}

fn modules(dir: &Path, store: &Store) -> Vec<(String, String, String)> {
    let areas = kc_index::areas::areas(dir, store).unwrap();
    let mut out: Vec<(String, String, String)> =
        kc_index::areas::declared(store, &areas).unwrap().modules.into_iter().map(|m| (m.name, m.kind, m.dir)).collect();
    out.sort();
    out
}

fn class(name: &str) -> String {
    format!("package org.example.shop;\n\npublic class {name} {{\n  public int size() {{ return 0; }}\n}}\n")
}

#[test]
fn a_gradle_build_declares_a_module_per_project() {
    let (dir, store) = repo(
        "gradle",
        &[
            ("settings.gradle.kts", "rootProject.name = \"orchard\"\ninclude(\":core\", \":web:admin\", \":docs\")\n"),
            ("build.gradle.kts", "plugins { java }\n"),
            ("core/build.gradle.kts", "group = \"org.example\"\ndependencies {\n  api(\"com.squareup.okio:okio:3.9.0\")\n}\n"),
            ("core/src/main/java/org/example/shop/Cart.java", &class("Cart")),
            ("web/admin/build.gradle", "dependencies {\n  implementation project(':core')\n}\n"),
            ("web/admin/src/main/kotlin/Admin.kt", "package org.example.shop\n\nclass Admin\n"),
            // A project with no build script of its own, configured from the root.
            ("docs/src/main/java/org/example/shop/Docs.java", &class("Docs")),
            // A build script the settings script doesn't include: Gradle never builds it.
            ("old/build.gradle.kts", "plugins { java }\n"),
            ("old/src/main/java/org/example/shop/Old.java", &class("Old")),
            // A build of its own (a plugin's test fixture), with a settings script beside it.
            ("plugin/src/test/resources/fixture/settings.gradle.kts", "rootProject.name = \"fixture\"\n"),
            ("plugin/src/test/resources/fixture/build.gradle.kts", "plugins { java }\n"),
            ("plugin/src/test/resources/fixture/src/main/java/Fixture.java", "class Fixture {}\n"),
        ],
    );
    assert_eq!(
        modules(&dir, &store),
        [
            // Named by its build script, as Gradle names a project (here with the `group` it sets)...
            ("admin".to_string(), "gradle".to_string(), "web/admin".to_string()),
            // ...or, with none, by the settings script's `include`.
            ("docs".to_string(), "gradle".to_string(), "docs".to_string()),
            ("fixture".to_string(), "gradle".to_string(), "plugin/src/test/resources/fixture".to_string()),
            ("org.example:core".to_string(), "gradle".to_string(), "core".to_string()),
        ]
    );
    let packages = kc_store::manifests::packages(store.conn()).unwrap();
    let core = packages.iter().find(|p| p.file == "core/build.gradle.kts").unwrap();
    assert_eq!(core.name, "org.example:core");
    assert_eq!(core.deps.iter().map(|d| (d.name.as_str(), d.source.as_str())).collect::<Vec<_>>(), [("com.squareup.okio:okio", "registry")]);
    let admin = packages.iter().find(|p| p.file == "web/admin/build.gradle").unwrap();
    assert_eq!(admin.deps.iter().map(|d| (d.name.as_str(), d.source.as_str())).collect::<Vec<_>>(), [(":core", "workspace")]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_maven_build_declares_a_module_per_pom() {
    let pom = |artifact: &str, modules: &str, deps: &str| {
        format!(
            "<project>\n  <parent><groupId>org.example.shop</groupId><artifactId>shop</artifactId><version>1</version></parent>\n  <artifactId>{artifact}</artifactId>\n  {modules}\n  <dependencies>{deps}</dependencies>\n</project>\n"
        )
    };
    let (dir, store) = repo(
        "maven",
        &[
            ("pom.xml", &pom("shop", "<modules><module>shop-model</module><module>shop-web</module></modules>", "")),
            ("shop-model/pom.xml", &pom("shop-model", "", "")),
            ("shop-model/src/main/java/org/example/shop/Cart.java", &class("Cart")),
            (
                "shop-web/pom.xml",
                &pom("shop-web", "", "<dependency><groupId>org.example.shop</groupId><artifactId>shop-model</artifactId></dependency>"),
            ),
            ("shop-web/src/main/java/org/example/shop/Page.java", &class("Page")),
        ],
    );
    assert_eq!(
        modules(&dir, &store),
        [
            ("org.example.shop:shop".to_string(), "maven".to_string(), String::new()),
            ("org.example.shop:shop-model".to_string(), "maven".to_string(), "shop-model".to_string()),
            ("org.example.shop:shop-web".to_string(), "maven".to_string(), "shop-web".to_string()),
        ]
    );
    let packages = kc_store::manifests::packages(store.conn()).unwrap();
    let web = packages.iter().find(|p| p.file == "shop-web/pom.xml").unwrap();
    assert_eq!(web.deps.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["org.example.shop:shop-model"]);
    let _ = std::fs::remove_dir_all(&dir);
}
