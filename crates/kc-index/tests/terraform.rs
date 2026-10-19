//! Terraform: addresses resolve within their configuration directory, so per-environment and per-region copies of one
//! configuration stay apart, and module calls reach their local module's variables and outputs.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn index(root: &Path) -> Store {
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    store
}

const ENV: &str = r#"
variable "region" {}

locals {
  name = "app-${var.region}"
}

resource "aws_s3_bucket" "logs" {
  bucket = local.name
}

resource "aws_s3_bucket_policy" "logs" {
  bucket = aws_s3_bucket.logs.id
  policy = templatefile("${path.module}/policy.tpl", { arn = aws_s3_bucket.logs.arn })
}

module "net" {
  source = "../../../modules/net"
  cidr   = "10.0.0.0/16"
  name   = local.name
}

resource "aws_instance" "web" {
  count      = 2
  subnet_id  = module.net.subnet_id
  depends_on = [aws_s3_bucket.logs]
}
"#;

const MODULE: &str = r#"
variable "cidr" {
  description = "The VPC's range"
}

variable "name" {}

resource "aws_vpc" "this" {
  cidr_block = var.cidr
  tags       = { Name = var.name }
}

output "subnet_id" {
  value = aws_vpc.this.id
}
"#;

/// The callers of the definition at `file#name`: (file, line, the block or local referring).
fn callers(store: &Store, root: &Path, at: &str) -> Vec<(String, u32, String)> {
    let ctx = query::context(store.conn(), root, at, &query::ContextLimits::from_budget(20_000)).unwrap().expect(at);
    let mut out: Vec<_> = ctx.callers.iter().map(|h| (h.file.clone(), h.line, h.from.as_ref().map_or(String::new(), |f| f.name.clone()))).collect();
    out.sort();
    out
}

#[test]
fn addresses_resolve_within_their_directory() {
    let root = scratch("terraform");
    for env in ["envs/prod/us-east-1", "envs/prod/us-west-2", "envs/staging/us-east-1"] {
        write(&root, &format!("{env}/main.tf"), ENV);
    }
    write(&root, "modules/net/main.tf", MODULE);
    let store = index(&root);
    let conn = store.conn();

    // Three buckets named alike, each used only in its own directory.
    let defs = query::definitions(conn, "aws_s3_bucket.logs").unwrap();
    assert_eq!(defs.len(), 3);
    let west = callers(&store, &root, "envs/prod/us-west-2/main.tf#aws_s3_bucket.logs");
    let f = "envs/prod/us-west-2/main.tf".to_string();
    assert_eq!(
        west,
        [
            (f.clone(), 13, "aws_s3_bucket_policy.logs".into()),
            (f.clone(), 14, "aws_s3_bucket_policy.logs".into()),
            (f.clone(), 26, "aws_instance.web".into()),
        ]
    );
    let (hits, _) = query::refs(conn, "aws_s3_bucket.logs", &[], usize::MAX).unwrap();
    assert_eq!(hits.len(), 9);
    for h in &hits {
        assert_eq!(h.confidence, "exact");
        assert_eq!(h.targets.len(), 1, "{h:?}");
        assert_eq!(Path::new(&h.targets[0].file).parent(), Path::new(&h.file).parent(), "{h:?}");
    }
    let depends: Vec<&query::RefHit> = hits.iter().filter(|h| h.kind == "depends_on").collect();
    assert_eq!(depends.len(), 3);

    // The local module: a call's arguments set its variables, and `module.net.subnet_id` reads its output, from every
    // environment.
    let cidr = callers(&store, &root, "var.cidr");
    // Set by each environment's call; used by the module's VPC.
    assert_eq!(cidr.len(), 4, "{cidr:?}");
    assert!(cidr[..3].iter().all(|(_, line, from)| *line == 19 && from == "module.net"), "{cidr:?}");
    assert_eq!(cidr[3], ("modules/net/main.tf".into(), 9, "aws_vpc.this".into()));
    let output = callers(&store, &root, "output.subnet_id");
    assert_eq!(output.iter().map(|c| c.2.as_str()).collect::<Vec<_>>(), ["aws_instance.web"; 3]);
    // Inside the module, `var.name` is the module's own variable, not an environment's local.
    let name = callers(&store, &root, "modules/net/main.tf#var.name");
    assert!(name.iter().any(|c| c.0 == "modules/net/main.tf" && c.2 == "aws_vpc.this"), "{name:?}");

    // Impact from the module's VPC crosses its output into every environment's instances.
    let vpc = query::definitions(conn, "aws_vpc.this").unwrap();
    let impact = kc_store::trace::impact(conn, &[vpc[0].id], 3, 100).unwrap();
    let mut reached: Vec<(String, String)> = impact.affected.iter().map(|a| (a.qualified.clone(), a.file.clone())).collect();
    reached.sort();
    assert!(reached.contains(&("output.subnet_id".into(), "modules/net/main.tf".into())), "{reached:?}");
    for env in ["envs/prod/us-east-1", "envs/prod/us-west-2", "envs/staging/us-east-1"] {
        assert!(reached.contains(&("aws_instance.web".into(), format!("{env}/main.tf"))), "{reached:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
