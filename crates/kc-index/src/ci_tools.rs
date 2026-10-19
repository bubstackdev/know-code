//! Deploy commands other than `ansible-playbook` in a CI job (see `kc_extract::gitlabci`): what each one deploys
//! and the files in the application repo it is configured by, read at query time from the checkout.
//!
//! - `wrangler deploy|publish|versions upload`: the `wrangler.toml`, `wrangler.json` or `wrangler.jsonc` used
//!   (`--config`, else the one in the working directory), the `main` entry it names, the worker's name (the
//!   environment's `name`, else `<name>-<env>` under `--env`, else the top-level one).
//! - `aws s3 sync|cp` (source, destination), `aws cloudfront create-invalidation|update-distribution`
//!   (distribution), `aws ecs update-service` (cluster, service), `aws lambda update-function-code` (function),
//!   `aws ec2 create-launch-template-version|modify-launch-template` (launch template), `aws autoscaling
//!   start-instance-refresh|set-desired-capacity|update-auto-scaling-group` (group), `aws ssm send-command` (document,
//!   targets), `aws cloudformation deploy` (stack, template), `aws elasticbeanstalk` (application, environment,
//!   version); `aws codeartifact publish-package-version` is a release (repository, package, version).
//! - `sam deploy` (stack, template, `samconfig.toml`), `cdk deploy` (stacks, `cdk.json`).
//! - `terraform`, `tofu`, `gitlab-terraform` and `gitlab-tofu` `init|validate|plan|apply|destroy`: the configuration
//!   directory (`-chdir`, else the working directory; `$TF_ROOT` for `gitlab-terraform`, `$GITLAB_TOFU_ROOT_DIR` for
//!   `gitlab-tofu`), its workspace, and the `source` of
//!   each `module` block in its `.tf` files, linked as [`crate::terraform`] links module calls.
//! - `docker build|push`: the Dockerfile (`-f`, else the context's), the context, the image tags.
//! - `kubectl apply|set|rollout|create|replace` (manifests, namespace, image), `helm upgrade|install` (release,
//!   chart, values files, namespace).
//! - Releases: `npm publish` (also yarn's and pnpm's: the package, registry, dist tag), `upack push` (package,
//!   feed), `release-cli create` (tag).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::ci::Passed;

/// A file a deploy command reads.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FileRef {
    /// `config`, `entry`, `dockerfile`, `context`, `tf_dir`, `template`, `manifest`, `chart`, `values`,
    /// `package_json`.
    pub role: &'static str,
    /// Its path in the application repo (`.` for the root), or as written when it holds a variable nothing sets.
    pub path: String,
    /// The checkout has it.
    pub found: bool,
}

/// A Terraform `module` block's source and the repo it names.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModuleLink {
    pub name: String,
    pub source: String,
    /// Where the block is, in the application repo.
    pub file: String,
    pub line: u32,
    /// The repo holding the module: another repo (by git URL or registry address), or the application repo itself
    /// for a local path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    /// As [`crate::terraform::Terraform::link`] gives it: `exact` (a git source whose URL is the repo's origin, or a
    /// local path), `unique_name` (a registry address, or a git URL by the repo's name), `ambiguous` (several repos),
    /// `external_likely` (nothing indexed: a public registry module, an archive).
    pub confidence: &'static str,
}

/// How a step was reached through a package script: `npm run deploy`, where that runs and where the script is.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Via {
    /// The command running the script, as written (`pnpm run deploy`).
    pub command: String,
    pub repo: PathBuf,
    pub file: String,
    pub line: u32,
}

/// One deploy command of a job.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeployStep {
    /// `wrangler`, `aws`, `terraform` (also for `tofu` and `gitlab-terraform`), `docker`, `kubectl`, `helm`, `sam`,
    /// `cdk`; releases `npm`, `upack`, `release-cli`.
    pub tool: &'static str,
    /// The subcommand: `deploy`, `s3 cp`, `cloudfront update-distribution`, `apply`, `build`, …
    pub action: String,
    /// The command as written, on one line.
    pub command: String,
    /// Where it is written: a CI file, or the package.json holding the script it is in.
    pub repo: PathBuf,
    pub file: String,
    pub line: u32,
    /// The template job supplying it (the job itself when its own).
    #[serde(skip)]
    pub from_job: String,
    /// The template jobs whose `!reference`s splice it into the job, outermost first.
    #[serde(skip)]
    pub through: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileRef>,
    /// What it deploys to, each value as written and as resolved (see [`Passed`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Passed>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<ModuleLink>,
}

/// The repo a module source names and the link's confidence (`(None, "external_likely")` when no indexed repo
/// holds it), or `None` for a local path.
pub(crate) type ModuleRepo<'x> = dyn Fn(&str) -> Option<(Option<PathBuf>, &'static str)> + 'x;

/// What reading a command needs from the pipeline: the application checkout, the working directory, and variables.
pub(crate) struct ToolCtx<'x> {
    pub root: &'x Path,
    pub cwd: String,
    /// A value as written, expanded (`name` is the value's name).
    pub expand: &'x dyn Fn(&str, &str) -> Passed,
    /// Whether a variable is set anywhere the pipeline reads.
    pub has: &'x dyn Fn(&str) -> bool,
    /// The repo a Terraform module source names.
    pub module: &'x ModuleRepo<'x>,
}

impl ToolCtx<'_> {
    /// A path as written, expanded and joined to the working directory; with whether every variable in it is set.
    fn path(&self, written: &str) -> (String, bool) {
        let p = (self.expand)("", written);
        let text = p.value.clone().or(p.partial).unwrap_or_else(|| written.to_string());
        (join(&self.cwd, &text), p.value.is_some())
    }

    fn file(&self, role: &'static str, written: &str) -> FileRef {
        let (path, known) = self.path(written);
        FileRef { role, found: known && exists(self.root, &path), path }
    }
}

/// `dir` joined with `rel` (`.` and `..` read; `$CI_PROJECT_DIR` is `.`), `.` for the root.
pub(crate) fn join(dir: &str, rel: &str) -> String {
    let rel = rel.trim();
    let mut parts: Vec<&str> = if rel.starts_with('/') { Vec::new() } else { dir.split('/').filter(|p| !p.is_empty() && *p != ".").collect() };
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() { ".".into() } else { parts.join("/") }
}

/// The largest file read.
const MAX_READ: u64 = 1 << 20;

pub(crate) fn read(root: &Path, rel: &str) -> Option<String> {
    if crate::privacy::blocks(root, rel) {
        return None;
    }
    let p = root.join(rel);
    let meta = std::fs::metadata(&p).ok()?;
    if !meta.is_file() || meta.len() > MAX_READ {
        return None;
    }
    std::fs::read_to_string(p).ok()
}

fn exists(root: &Path, rel: &str) -> bool {
    !rel.contains('$') && root.join(rel).exists() && !crate::privacy::blocks(root, rel)
}

/// Options and positional arguments, given the options taking a value; `--opt=value` and `-chdir=dir` forms too.
struct Args<'s> {
    opts: Vec<(&'s str, &'s str)>,
    flags: Vec<&'s str>,
    pos: Vec<&'s str>,
}

impl<'s> Args<'s> {
    fn parse(args: &[&'s str], takes: &[&str]) -> Args<'s> {
        let mut out = Args { opts: Vec::new(), flags: Vec::new(), pos: Vec::new() };
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            if a == "--" {
                out.pos.extend(&args[i + 1..]);
                break;
            }
            if a.starts_with('-') && a.len() > 1 {
                if let Some((o, v)) = a.split_once('=') {
                    out.opts.push((o, v));
                } else if takes.contains(&a) && i + 1 < args.len() {
                    out.opts.push((a, args[i + 1]));
                    i += 1;
                } else {
                    out.flags.push(a);
                }
            } else {
                out.pos.push(a);
            }
            i += 1;
        }
        out
    }

    fn get(&self, names: &[&str]) -> Option<&'s str> {
        self.opts.iter().rev().find(|(o, _)| names.contains(o)).map(|(_, v)| *v)
    }

    fn all(&self, names: &[&str]) -> Vec<&'s str> {
        self.opts.iter().filter(|(o, _)| names.contains(o)).map(|(_, v)| *v).collect()
    }
}

fn collapse(s: &str) -> String {
    let mut out = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.len() > 240 {
        let mut cut = 240;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

/// The step a `tool` op gives (the tool as the extractor wrote it, then its arguments), or none for a subcommand
/// this doesn't read. Its location is the caller's to fill.
pub(crate) fn step(tool: &str, args: &[&str], cx: &ToolCtx) -> Option<DeployStep> {
    let command = collapse(&format!("{tool} {}", args.join(" ")));
    let mut s = DeployStep {
        tool: "",
        action: String::new(),
        command,
        repo: PathBuf::new(),
        file: String::new(),
        line: 0,
        from_job: String::new(),
        through: Vec::new(),
        via: None,
        files: Vec::new(),
        values: Vec::new(),
        modules: Vec::new(),
    };
    let val = |s: &mut DeployStep, name: &str, written: &str| s.values.push((cx.expand)(name, written));
    match tool {
        "wrangler" => {
            let a = Args::parse(args, &["--env", "-e", "--config", "-c", "--name", "--compatibility-date", "--route", "--var", "--define", "--outdir", "--assets", "--site", "--triggers", "--project-name", "--branch", "--tag", "--message", "--preview-alias"]);
            let (action, rest) = match a.pos.as_slice() {
                [v @ ("versions" | "pages"), sub, rest @ ..] => (format!("{v} {sub}"), rest),
                [v, rest @ ..] => (v.to_string(), rest),
                [] => return None,
            };
            s.tool = "wrangler";
            if action.starts_with("pages") {
                if let Some(dir) = rest.first() {
                    s.files.push(cx.file("assets", dir));
                }
                if let Some(p) = a.get(&["--project-name"]) {
                    val(&mut s, "project", p);
                }
                s.action = action;
                return Some(s);
            }
            s.action = action;
            let config = match a.get(&["--config", "-c"]) {
                Some(c) => Some(cx.file("config", c)),
                None => ["wrangler.toml", "wrangler.json", "wrangler.jsonc"].iter().map(|f| cx.file("config", f)).find(|f| f.found),
            };
            let env = a.get(&["--env", "-e"]).map(|e| (cx.expand)("env", e));
            let parsed = config.as_ref().filter(|c| c.found).and_then(|c| read(cx.root, &c.path).and_then(|t| wrangler_config(&c.path, &t)));
            let env_name = env.as_ref().and_then(|e| e.value.clone());
            if let Some(c) = config {
                let dir = c.path.rsplit_once('/').map_or(".".to_string(), |(d, _)| d.to_string());
                s.files.push(c);
                if let Some(w) = &parsed {
                    let main = env_name.as_deref().and_then(|e| w.envs.iter().find(|(n, ..)| n == e)).and_then(|(_, _, m)| m.clone()).or_else(|| w.main.clone());
                    // A script given on the command line replaces `main`.
                    let entry = rest.first().map(|e| cx.path(e).0).or_else(|| main.map(|m| join(&dir, &m)));
                    if let Some(e) = entry {
                        s.files.push(FileRef { role: "entry", found: exists(cx.root, &e), path: e });
                    }
                }
            }
            let name = match (a.get(&["--name"]), &parsed) {
                (Some(n), _) => Some(n.to_string()),
                (None, Some(w)) => match &env_name {
                    Some(e) => match w.envs.iter().find(|(n, ..)| n == e) {
                        Some((_, Some(n), _)) => Some(n.clone()),
                        _ => w.name.as_ref().map(|n| format!("{n}-{e}")),
                    },
                    None => w.name.clone(),
                },
                _ => None,
            };
            if let Some(n) = name {
                val(&mut s, "name", &n);
            }
            if let Some(e) = env {
                s.values.push(e);
            }
        }
        "aws" => {
            let a = Args::parse(
                args,
                &[
                    "--region", "--profile", "--output", "--endpoint-url", "--query", "--color", "--ca-bundle", "--cli-read-timeout", "--cli-connect-timeout", "--exclude", "--include", "--acl",
                    "--cache-control", "--content-type", "--content-encoding", "--content-disposition", "--content-language", "--metadata", "--metadata-directive", "--storage-class", "--expires",
                    "--sse", "--sse-kms-key-id", "--grants", "--website-redirect", "--distribution-id", "--id", "--paths", "--if-match", "--distribution-config", "--invalidation-batch",
                    "--cluster", "--service", "--task-definition", "--desired-count", "--function-name", "--zip-file", "--s3-bucket", "--s3-key", "--image-uri", "--publish", "--page-size",
                    "--source-region", "--deployment-configuration", "--network-configuration", "--platform-version", "--force-new-deployment", "--revision-id", "--architectures",
                    "--launch-template-id", "--launch-template-name", "--source-version", "--version-description", "--launch-template-data", "--default-version",
                    "--auto-scaling-group-name", "--preferences", "--desired-configuration", "--desired-capacity", "--min-size", "--max-size", "--document-name",
                    "--targets", "--instance-ids", "--parameters", "--comment", "--stack-name", "--template-file", "--capabilities", "--parameter-overrides",
                    "--environment-name", "--application-name", "--version-label", "--source-bundle", "--domain", "--domain-owner", "--repository", "--format",
                    "--namespace", "--package", "--package-version", "--asset-content", "--asset-name", "--asset-sha256",
                ],
            );
            let [service, sub, rest @ ..] = a.pos.as_slice() else { return None };
            s.tool = "aws";
            s.action = format!("{service} {sub}");
            match (*service, *sub) {
                ("s3", _) => {
                    if let Some(src) = rest.first() {
                        val(&mut s, "source", src);
                    }
                    if let Some(dst) = rest.get(1) {
                        val(&mut s, "destination", dst);
                    }
                }
                ("cloudfront", _) => {
                    if let Some(d) = a.get(&["--distribution-id", "--id"]) {
                        val(&mut s, "distribution", d);
                    }
                }
                ("ecs", _) => {
                    for (name, opt) in [("cluster", "--cluster"), ("service", "--service"), ("task_definition", "--task-definition")] {
                        if let Some(v) = a.get(&[opt]) {
                            val(&mut s, name, v);
                        }
                    }
                }
                ("lambda", _) => {
                    if let Some(f) = a.get(&["--function-name"]) {
                        val(&mut s, "function", f);
                    }
                    if let Some(c) = a.get(&["--zip-file", "--image-uri", "--s3-bucket"]) {
                        val(&mut s, "code", c);
                    }
                }
                _ => {
                    // The rest name what they act on by option.
                    let named: &[(&str, &[&str])] = match (*service, *sub) {
                        ("ec2", _) => &[("launch_template", &["--launch-template-id", "--launch-template-name"]), ("version", &["--default-version"])],
                        ("autoscaling", _) => &[("asg", &["--auto-scaling-group-name"]), ("capacity", &["--desired-capacity"])],
                        ("ssm", _) => &[("document", &["--document-name"]), ("targets", &["--targets", "--instance-ids"])],
                        ("cloudformation", _) => &[("stack", &["--stack-name"])],
                        ("elasticbeanstalk", _) => &[("application", &["--application-name"]), ("environment", &["--environment-name"]), ("version", &["--version-label"])],
                        ("codeartifact", _) => &[("repository", &["--repository"]), ("package", &["--package"]), ("version", &["--package-version"])],
                        _ => &[],
                    };
                    for (name, opts) in named {
                        if let Some(v) = a.get(opts) {
                            val(&mut s, name, v);
                        }
                    }
                    if let Some(t) = a.get(&["--template-file"]) {
                        s.files.push(cx.file("template", t));
                    }
                }
            }
        }
        "sam" | "cdk" => {
            let a = Args::parse(args, &["--stack-name", "--template-file", "-t", "--template", "--config-file", "--config-env", "--s3-bucket", "--region", "--profile", "--parameter-overrides", "--capabilities", "--app", "-a", "--context", "-c", "--require-approval", "--outputs-file"]);
            if a.pos.first() != Some(&"deploy") {
                return None;
            }
            s.action = "deploy".into();
            if tool == "sam" {
                s.tool = "sam";
                let t = a.get(&["--template-file", "-t", "--template"]).map(|t| cx.file("template", t));
                let t = t.or_else(|| ["template.yaml", "template.yml"].iter().map(|f| cx.file("template", f)).find(|f| f.found));
                s.files.extend(t);
                let c = cx.file("config", a.get(&["--config-file"]).unwrap_or("samconfig.toml"));
                if c.found {
                    s.files.push(c);
                }
                if let Some(n) = a.get(&["--stack-name"]) {
                    val(&mut s, "stack", n);
                }
            } else {
                s.tool = "cdk";
                let c = cx.file("config", "cdk.json");
                if c.found {
                    s.files.push(c);
                }
                if a.pos.len() > 1 {
                    val(&mut s, "stacks", &a.pos[1..].join(" "));
                }
            }
        }
        "terraform" | "tofu" | "gitlab-terraform" | "gitlab-tofu" => {
            let chdir = args.iter().take_while(|a| a.starts_with('-')).find_map(|a| a.strip_prefix("-chdir="));
            let a = Args::parse(args, &["-var-file", "-var", "-target", "-out", "-backend-config", "-lock-timeout", "-parallelism", "-state"]);
            let action = a.pos.first()?;
            s.tool = "terraform";
            // `apply -destroy` destroys.
            s.action = if *action == "apply" && a.flags.contains(&"-destroy") { "destroy".to_string() } else { action.to_string() };
            let dir = match (chdir, tool) {
                (Some(d), _) => cx.path(d),
                (None, "gitlab-terraform") if (cx.has)("TF_ROOT") => cx.path("$TF_ROOT"),
                (None, "gitlab-tofu") if (cx.has)("GITLAB_TOFU_ROOT_DIR") => cx.path("$GITLAB_TOFU_ROOT_DIR"),
                _ => (cx.cwd.clone(), true),
            };
            let (dir, known) = dir;
            let found = known && tf_files(cx.root, &dir).next().is_some();
            s.files.push(FileRef { role: "tf_dir", path: dir.clone(), found });
            for f in a.all(&["-var-file"]) {
                s.files.push(cx.file("var_file", &format!("{dir}/{f}")));
            }
            if (cx.has)("TF_WORKSPACE") {
                val(&mut s, "workspace", "$TF_WORKSPACE");
            }
            if found {
                s.modules = modules(cx, &dir);
            }
        }
        "docker" => {
            let takes = &[
                "-f", "--file", "-t", "--tag", "--build-arg", "--target", "--platform", "--network", "--label", "--cache-from", "--cache-to", "--secret", "--ssh", "--progress", "-o",
                "--output", "--iidfile", "--add-host", "--shm-size", "--ulimit", "-m", "--memory", "--builder", "--metadata-file", "-H", "--host", "--context", "--config",
            ];
            let a = Args::parse(args, takes);
            let pos: Vec<&str> = a.pos.iter().copied().skip_while(|p| *p == "buildx" || *p == "image").collect();
            let (action, rest) = pos.split_first()?;
            s.tool = "docker";
            s.action = action.to_string();
            if *action == "build" {
                let context = rest.last().copied().unwrap_or(".");
                let ctx = cx.file("context", context);
                // `-f` is read from the working directory, not the context.
                let dockerfile = match a.get(&["-f", "--file"]) {
                    Some(f) => cx.file("dockerfile", f),
                    None => cx.file("dockerfile", &format!("{context}/Dockerfile")),
                };
                s.files.push(dockerfile);
                s.files.push(ctx);
                let tags = a.all(&["-t", "--tag"]);
                if !tags.is_empty() {
                    val(&mut s, "image", &tags.join(" "));
                }
            } else if let Some(img) = rest.first() {
                val(&mut s, "image", img);
            }
        }
        "kubectl" => {
            let a = Args::parse(args, &["-f", "--filename", "-k", "--kustomize", "-n", "--namespace", "--context", "--kubeconfig", "-l", "--selector", "-o", "--output", "--cluster", "--user", "--server", "-s", "--timeout", "--field-manager"]);
            let (action, rest) = a.pos.split_first()?;
            s.tool = "kubectl";
            s.action = match (*action, rest.first()) {
                ("set" | "rollout" | "create", Some(sub)) => format!("{action} {sub}"),
                _ => action.to_string(),
            };
            for f in a.all(&["-f", "--filename"]) {
                s.files.push(cx.file("manifest", f));
            }
            for k in a.all(&["-k", "--kustomize"]) {
                s.files.push(cx.file("manifest", k));
            }
            if let Some(n) = a.get(&["-n", "--namespace"]) {
                val(&mut s, "namespace", n);
            }
            match (*action, rest) {
                ("set", [_, target, image, ..]) => {
                    val(&mut s, "target", target);
                    val(&mut s, "image", image);
                }
                ("rollout", [_, target, ..]) => val(&mut s, "target", target),
                _ => {}
            }
        }
        "helm" => {
            let a = Args::parse(args, &["-f", "--values", "-n", "--namespace", "--set", "--set-string", "--set-file", "--version", "--timeout", "--kube-context", "--kubeconfig", "--repo", "--post-renderer", "-o", "--output", "--description"]);
            let [action, release, chart, ..] = a.pos.as_slice() else { return None };
            s.tool = "helm";
            s.action = action.to_string();
            val(&mut s, "release", release);
            let c = cx.file("chart", chart);
            if c.found {
                s.files.push(c);
            } else {
                val(&mut s, "chart", chart);
            }
            for f in a.all(&["-f", "--values"]) {
                s.files.push(cx.file("values", f));
            }
            if let Some(n) = a.get(&["-n", "--namespace"]) {
                val(&mut s, "namespace", n);
            }
        }
        "npm" | "yarn" | "pnpm" => {
            let a = Args::parse(args, &["--tag", "--registry", "--access", "--otp", "--workspace", "-w"]);
            if a.pos.first() != Some(&"publish") {
                return None;
            }
            s.tool = "npm";
            s.action = "publish".into();
            // The package published: a tarball or directory given, else the working directory's package.json.
            match a.pos.get(1) {
                Some(p) => val(&mut s, "package", p),
                None => s.files.push(cx.file("package_json", "package.json")),
            }
            if let Some(r) = a.get(&["--registry"]) {
                val(&mut s, "registry", r);
            }
            if let Some(t) = a.get(&["--tag"]) {
                val(&mut s, "tag", t);
            }
        }
        "upack" => {
            let a = Args::parse(args, &["--user", "--source"]);
            let ["push", package, rest @ ..] = a.pos.as_slice() else { return None };
            s.tool = "upack";
            s.action = "push".into();
            val(&mut s, "package", package);
            if let Some(feed) = rest.first().copied().or(a.get(&["--source"])) {
                val(&mut s, "feed", feed);
            }
        }
        "release-cli" => {
            let a = Args::parse(args, &["--name", "--tag-name", "--ref", "--description", "--milestone", "--released-at", "--assets-link"]);
            if a.pos.first() != Some(&"create") {
                return None;
            }
            s.tool = "release-cli";
            s.action = "create".into();
            if let Some(t) = a.get(&["--tag-name"]) {
                val(&mut s, "tag", t);
            }
        }
        _ => return None,
    }
    Some(s)
}

/// Whether a step releases a package or version rather than deploying something.
pub fn is_release(step: &DeployStep) -> bool {
    matches!(step.tool, "npm" | "upack" | "release-cli") || step.action.starts_with("codeartifact ")
}

/// A wrangler config's top-level `name` and `main`, and each environment's (name, `name`, `main`).
#[derive(Debug, Default, PartialEq)]
struct Wrangler {
    name: Option<String>,
    main: Option<String>,
    envs: Vec<(String, Option<String>, Option<String>)>,
}

fn wrangler_config(path: &str, text: &str) -> Option<Wrangler> {
    let doc: serde_json::Value = if path.ends_with(".toml") {
        let t: toml::Value = toml::from_str(text).ok()?;
        serde_json::to_value(t).ok()?
    } else {
        serde_json::from_str(&strip_jsonc(text)).ok()?
    };
    let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let mut w = Wrangler { name: s(&doc, "name"), main: s(&doc, "main"), envs: Vec::new() };
    if let Some(envs) = doc.get("env").and_then(|e| e.as_object()) {
        for (n, v) in envs {
            w.envs.push((n.clone(), s(v, "name"), s(v, "main")));
        }
    }
    Some(w)
}

/// JSON with comments (`//`, `/* */`) and trailing commas, as plain JSON.
fn strip_jsonc(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                out.extend_from_slice(&b[start..i.min(b.len())]);
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b',' => {
                let next = b[i + 1..].iter().find(|c| !c.is_ascii_whitespace());
                if !matches!(next, Some(b'}' | b']')) {
                    out.push(b',');
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The `.tf` files directly in `dir` of the checkout.
fn tf_files(root: &Path, dir: &str) -> impl Iterator<Item = String> {
    let privacy = crate::privacy::Privacy::load(root);
    let listed = if privacy.blocks(dir) { None } else { std::fs::read_dir(root.join(dir)).ok() };
    let mut names: Vec<String> = listed
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".tf"))
        .collect();
    names.sort();
    let dir = dir.to_string();
    names.into_iter().map(move |n| if dir == "." { n } else { format!("{dir}/{n}") }).filter(move |p| !privacy.blocks(p))
}

/// The `module` blocks of the configuration in `dir`, each linked as `cx.module` links its source.
fn modules(cx: &ToolCtx, dir: &str) -> Vec<ModuleLink> {
    let mut out = Vec::new();
    for file in tf_files(cx.root, dir) {
        let Some(text) = read(cx.root, &file) else { continue };
        for (name, source, line) in module_sources(&text) {
            // `None` from the linker is a local path: a directory of the application repo.
            let (repo, confidence) = (cx.module)(&source).unwrap_or((Some(cx.root.to_path_buf()), "exact"));
            out.push(ModuleLink { name, source, file: file.clone(), line, repo, confidence });
        }
    }
    out
}

/// Each `module "name" { … source = "…" … }` of a Terraform file: its name, source and the source's line.
pub(crate) fn module_sources(text: &str) -> Vec<(String, String, u32)> {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((_, l)) = lines.next() {
        let t = l.trim_start();
        let Some(rest) = t.strip_prefix("module") else { continue };
        let rest = rest.trim_start();
        let Some(name) = rest.strip_prefix('"').and_then(|r| r.split_once('"')).map(|(n, _)| n.to_string()) else { continue };
        if !rest.contains('{') {
            continue;
        }
        // The block's own `source`: at depth one, before its closing brace.
        let mut depth = l.matches('{').count() as i32 - l.matches('}').count() as i32;
        for (j, b) in lines.by_ref() {
            let bt = b.trim();
            if depth == 1
                && let Some(v) = bt.strip_prefix("source")
                && let Some(v) = v.trim_start().strip_prefix('=')
                && let Some(s) = v.trim().strip_prefix('"').and_then(|s| s.split_once('"')).map(|(s, _)| s)
            {
                out.push((name.clone(), s.to_string(), j as u32 + 1));
            }
            depth += b.matches('{').count() as i32 - b.matches('}').count() as i32;
            if depth <= 0 {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_destroy_is_a_destroy() {
        let expand = |name: &str, written: &str| Passed { name: name.into(), written: written.into(), value: Some(written.into()), partial: None, from: None, unresolved: Vec::new() };
        let has = |_: &str| false;
        let module = |_: &str| None;
        let cx = ToolCtx { root: Path::new("/nonexistent"), cwd: ".".into(), expand: &expand, has: &has, module: &module };
        let action = |tool: &str, args: &[&str]| step(tool, args, &cx).map(|s| s.action);
        assert_eq!(action("gitlab-tofu", &["apply", "-destroy"]).as_deref(), Some("destroy"));
        assert_eq!(action("terraform", &["-chdir=infra", "apply", "-destroy", "-auto-approve"]).as_deref(), Some("destroy"));
        assert_eq!(action("tofu", &["destroy"]).as_deref(), Some("destroy"));
        assert_eq!(action("terraform", &["apply", "-auto-approve"]).as_deref(), Some("apply"));
    }

    #[test]
    fn parses_configs_and_module_sources() {
        let w = wrangler_config(
            "wrangler.jsonc",
            "{\n // c\n \"name\": \"site\", /* x */\n \"main\": \"src/worker/index.ts\",\n \"env\": {\"staging\": {\"vars\": {\"A\": \"http://x\"},},},\n}",
        )
        .unwrap();
        assert_eq!(w, Wrangler { name: Some("site".into()), main: Some("src/worker/index.ts".into()), envs: vec![("staging".into(), None, None)] });
        let t = wrangler_config("wrangler.toml", "name = \"api\"\nmain = \"src/index.js\"\n[env.prod]\nname = \"api-live\"\n").unwrap();
        assert_eq!(t.envs, [("prod".to_string(), Some("api-live".to_string()), None)]);
        let tf = "module \"spa\" {\n  source  = \"gitlab.example.com/terraform/terraform-aws-site/aws\"\n  version = \"1.0\"\n  tags = { a = 1 }\n}\n\nmodule \"vpc\" {\n  source = \"terraform-aws-modules/vpc/aws\"\n}\nterraform {\n  required_providers {\n    aws = { source = \"hashicorp/aws\" }\n  }\n}\nmodule \"local\" {\n  inputs = {\n    source = \"not this\"\n  }\n  source = \"./modules/x\"\n}\n";
        assert_eq!(
            module_sources(tf),
            [
                ("spa".to_string(), "gitlab.example.com/terraform/terraform-aws-site/aws".to_string(), 2),
                ("vpc".to_string(), "terraform-aws-modules/vpc/aws".to_string(), 8),
                ("local".to_string(), "./modules/x".to_string(), 19),
            ]
        );
        assert_eq!(join("infra", "../app/./x"), "app/x");
        assert_eq!(join("", "."), ".");
    }
}
