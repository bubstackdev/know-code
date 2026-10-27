//! Host evidence from configuration: which hosts a repo serves, and which its clients send to (see
//! [`HostFact`]). Read from ASP.NET `appsettings*.json` and `launchSettings.json`, .NET `web.config` and
//! `app.config`, Docker Compose files, Kubernetes ingress manifests, and HAProxy and nginx configs.
//!
//! Only hosts are kept: a URL value gives its host (see [`urls::host_of`]) and nothing else of it, so a credential
//! or token in a value never reaches the index. Each fact names where it was read (its line), and for a proxy the
//! backend service it routes the host to.

use crate::urls::host_of;
use crate::yaml::{self, Node, Val};
use crate::{FileFacts, HostFact};

pub const VERSION: u32 = 1;

/// Whether `path` is a file this module reads.
pub fn is_host_config(path: &str) -> bool {
    kind_of(path).is_some()
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    AppSettings,
    LaunchSettings,
    XmlConfig,
    Compose,
    Ingress,
    HaProxy,
    Nginx,
}

fn kind_of(path: &str) -> Option<Kind> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let lower = file.to_ascii_lowercase();
    let dirs: Vec<String> = path.split('/').rev().skip(1).map(str::to_ascii_lowercase).collect();
    let yaml = lower.ends_with(".yml") || lower.ends_with(".yaml");
    Some(if lower.starts_with("appsettings") && lower.ends_with(".json") {
        Kind::AppSettings
    } else if lower == "launchsettings.json" {
        Kind::LaunchSettings
    } else if (lower.starts_with("web.") || lower.starts_with("app.")) && lower.ends_with(".config") {
        Kind::XmlConfig
    } else if yaml && (lower.starts_with("docker-compose") || lower.starts_with("compose")) {
        Kind::Compose
    } else if yaml && lower.contains("ingress") {
        Kind::Ingress
    } else if lower.ends_with(".cfg") && (lower.contains("haproxy") || dirs.iter().any(|d| d.contains("haproxy"))) {
        Kind::HaProxy
    } else if lower == "nginx.conf" || (lower.ends_with(".conf") && dirs.iter().any(|d| d.contains("nginx") || d.starts_with("sites-"))) {
        Kind::Nginx
    } else {
        return None;
    })
}

pub fn extract(path: &str, source: &str) -> FileFacts {
    let mut facts = FileFacts::default();
    let Some(kind) = kind_of(path) else { return facts };
    let out = &mut facts.hosts;
    match kind {
        Kind::AppSettings => {
            for (key, value, line) in json_strings(source) {
                let lower = key.to_ascii_lowercase();
                // The app's own listening addresses: `Kestrel:Endpoints:Http:Url`, `Urls`.
                let listens = (lower.starts_with("kestrel:") && lower.ends_with(":url")) || lower == "urls";
                for v in value.split(';') {
                    if let Some(host) = host_of(v) {
                        out.push(if listens {
                            serves("kestrel", host, None, line)
                        } else {
                            HostFact { role: "configures", kind: "config", host, target: Some(key.clone()), line }
                        });
                    }
                }
            }
        }
        Kind::LaunchSettings => {
            for (key, value, line) in json_strings(source) {
                if key.ends_with("applicationUrl") {
                    for v in value.split(';') {
                        if let Some(host) = host_of(v) {
                            out.push(serves("launch_settings", host, None, line));
                        }
                    }
                }
            }
        }
        Kind::XmlConfig => xml_config(source, out),
        Kind::Compose => compose(source, out),
        Kind::Ingress => ingress(source, out),
        Kind::HaProxy => haproxy(source, out),
        Kind::Nginx => nginx(source, out),
    }
    out.dedup();
    facts
}

fn serves(kind: &'static str, host: String, target: Option<String>, line: u32) -> HostFact {
    HostFact { role: "serves", kind, host, target, line }
}

/// Every string value of a JSON (or JSON with comments) document with its key path (`A:B:C`, the way .NET
/// configuration names keys; array elements by index) and line. Malformed input yields what was read before.
fn json_strings(src: &str) -> Vec<(String, String, u32)> {
    enum Frame {
        Obj(Option<String>),
        Arr(usize),
    }
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    // Path parts of the open containers, and the key awaiting its value in the innermost object.
    let mut path: Vec<String> = Vec::new();
    let mut line = 1u32;
    let mut i = 0;
    let key_of = |stack: &[Frame]| -> Option<String> {
        match stack.last() {
            Some(Frame::Obj(k)) => k.clone(),
            Some(Frame::Arr(n)) => Some(n.to_string()),
            None => None,
        }
    };
    while i < b.len() {
        match b[i] {
            b'\n' => line += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
                continue;
            }
            c @ (b'{' | b'[') => {
                let part = key_of(&stack);
                if let Some(p) = part {
                    path.push(p);
                } else if !stack.is_empty() {
                    path.push(String::new());
                }
                stack.push(if c == b'{' { Frame::Obj(None) } else { Frame::Arr(0) });
            }
            b'}' | b']' => {
                stack.pop();
                if !stack.is_empty() {
                    path.pop();
                }
                // The container was its parent's value: the parent's key is used up.
                if let Some(Frame::Obj(k)) = stack.last_mut() {
                    *k = None;
                }
            }
            b',' => {
                if let Some(Frame::Arr(n)) = stack.last_mut() {
                    *n += 1;
                }
            }
            b'"' => {
                let start_line = line;
                let mut s = String::new();
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                        s.push(match b[i] {
                            b'n' => '\n',
                            b't' => '\t',
                            c => c as char,
                        });
                    } else {
                        if b[i] == b'\n' {
                            line += 1;
                        }
                        // Bytes of a UTF-8 sequence are pushed one by one below; only ASCII matters for hosts.
                        s.push(if b[i].is_ascii() { b[i] as char } else { '\u{fffd}' });
                    }
                    i += 1;
                }
                // A key is followed by `:`.
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                match stack.last_mut() {
                    Some(Frame::Obj(k)) if k.is_none() && b.get(j) == Some(&b':') => *k = Some(s),
                    Some(Frame::Obj(k)) => {
                        let key = k.take().unwrap_or_default();
                        let full = path.iter().chain(std::iter::once(&key)).filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(":");
                        out.push((full, s, start_line));
                    }
                    Some(Frame::Arr(n)) => {
                        let full = path.iter().cloned().chain(std::iter::once(n.to_string())).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(":");
                        out.push((full, s, start_line));
                    }
                    None => {}
                }
            }
            _ => {
                // A non-string scalar ends the pending key's value.
                if b[i].is_ascii_alphanumeric() || b[i] == b'-' {
                    while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'-' | b'+' | b'.')) {
                        i += 1;
                    }
                    if let Some(Frame::Obj(k)) = stack.last_mut() {
                        *k = None;
                    }
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// `<add key="OrdersApi" value="https://orders.example.com/" />`, `<endpoint address="…">`, `baseAddress="…"`:
/// URL attribute values, keyed by the element's `key` or `name` attribute.
fn xml_config(src: &str, out: &mut Vec<HostFact>) {
    for (n, text) in src.lines().enumerate() {
        let line = n as u32 + 1;
        let attr = |name: &str| -> Option<String> {
            let at = text.find(&format!("{name}=\""))? + name.len() + 2;
            Some(text[at..].split('"').next()?.to_string())
        };
        let key = attr("key").or_else(|| attr("name"));
        for name in ["value", "address", "baseAddress", "url", "connectionString"] {
            if let Some(host) = attr(name).as_deref().and_then(host_of) {
                out.push(HostFact { role: "configures", kind: "config", host, target: key.clone(), line });
            }
        }
    }
}

/// Docker Compose: a service built from the repo (`build:`) answers to its service name, its `hostname`, its
/// `container_name` and its published ports on `localhost`; URLs in such a service's environment are where its
/// clients send.
fn compose(src: &str, out: &mut Vec<HostFact>) {
    let (docs, _) = yaml::parse(src);
    for doc in &docs {
        let Some(services) = doc.get("services") else { continue };
        for (name, line, svc) in services.map() {
            if svc.get("build").is_none() {
                continue;
            }
            out.push(serves("compose", name.to_ascii_lowercase(), None, *line));
            for k in ["hostname", "container_name"] {
                if let Some(h) = svc.get(k).and_then(Node::str) {
                    out.push(serves("compose", h.to_ascii_lowercase(), None, svc.get(k).map_or(*line, |n| n.line)));
                }
            }
            for (p, l) in svc.get("ports").map(Node::strings).unwrap_or_default() {
                // `"5000:80"`, `"127.0.0.1:5000:80"`: the host port is the second last.
                let parts: Vec<&str> = p.split(':').collect();
                if parts.len() >= 2
                    && let Ok(port) = parts[parts.len() - 2].parse::<u16>()
                {
                    out.push(serves("compose", format!("localhost:{port}"), None, l));
                }
            }
            if let Some(env) = svc.get("environment") {
                let vars: Vec<(String, String, u32)> = match &env.v {
                    Val::Map(m) => m.iter().filter_map(|(k, l, v)| Some((k.clone(), v.str()?.to_string(), *l))).collect(),
                    Val::Seq(items) => items
                        .iter()
                        .filter_map(|i| i.str().and_then(|s| s.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string(), i.line)))
                        .collect(),
                    _ => Vec::new(),
                };
                for (k, v, l) in vars {
                    if let Some(host) = host_of(&v) {
                        out.push(HostFact { role: "configures", kind: "config", host, target: Some(format!("{name}:{k}")), line: l });
                    }
                }
            }
        }
    }
}

/// Kubernetes `Ingress`: each rule's host is served by the service its paths send to.
fn ingress(src: &str, out: &mut Vec<HostFact>) {
    let (docs, _) = yaml::parse(src);
    for doc in docs.iter().filter(|d| d.get("kind").and_then(Node::str) == Some("Ingress")) {
        let rules = doc.get("spec").and_then(|s| s.get("rules")).map(Node::seq).unwrap_or_default();
        for rule in rules {
            let Some(host) = rule.get("host").and_then(Node::str) else { continue };
            if host.contains("{{") {
                continue;
            }
            let paths = rule.get("http").and_then(|h| h.get("paths")).map(Node::seq).unwrap_or_default();
            let services: Vec<String> = paths
                .iter()
                .filter_map(|p| {
                    let b = p.get("backend")?;
                    b.get("service").and_then(|s| s.get("name")).or_else(|| b.get("serviceName")).and_then(Node::str).map(str::to_string)
                })
                .collect();
            let line = rule.get("host").map_or(rule.line, |h| h.line);
            let host = host.to_ascii_lowercase();
            if services.is_empty() {
                out.push(serves("ingress", host.clone(), None, line));
            }
            for s in services {
                out.push(serves("ingress", host.clone(), Some(s), line));
            }
        }
    }
}

/// HAProxy: `acl <name> hdr(host) -i <host>…` and `use_backend <backend> if <acl>…` in a frontend or listen section,
/// and the backend's servers. A backend's service is a Consul name its servers resolve (`_orders._tcp.service.consul`,
/// `orders.service.consul`) or a host name they name, else the backend's own name.
fn haproxy(src: &str, out: &mut Vec<HostFact>) {
    let mut acls: Vec<(String, Vec<String>)> = Vec::new();
    let mut uses: Vec<(String, Vec<String>, u32)> = Vec::new();
    let mut defaults: Vec<(String, u32)> = Vec::new();
    let mut backends: Vec<(String, Option<String>)> = Vec::new();
    let mut section: (String, String) = (String::new(), String::new());
    for (n, raw) in src.lines().enumerate() {
        let line = n as u32 + 1;
        let text = raw.split('#').next().unwrap_or_default();
        let words: Vec<&str> = text.split_whitespace().collect();
        let Some(first) = words.first() else { continue };
        if matches!(*first, "frontend" | "backend" | "listen" | "defaults" | "global") && !raw.starts_with([' ', '\t']) {
            section = (first.to_string(), words.get(1).unwrap_or(&"").to_string());
            if matches!(*first, "backend" | "listen") {
                backends.push((section.1.clone(), None));
            }
            continue;
        }
        match *first {
            "acl" if words.len() >= 4 && words[2].contains("(host)") => {
                let hosts: Vec<String> = words[3..].iter().filter(|w| !w.starts_with('-')).map(|w| w.to_ascii_lowercase()).collect();
                acls.push((words[1].to_string(), hosts));
            }
            "use_backend" if words.len() >= 2 && !words[1].contains('%') => {
                let conds: Vec<String> =
                    words[2..].iter().filter(|w| !matches!(**w, "if" | "unless" | "||" | "or" | "!")).map(|w| w.trim_start_matches('!').to_string()).collect();
                uses.push((words[1].to_string(), conds, line));
            }
            "default_backend" if words.len() >= 2 => defaults.push((words[1].to_string(), line)),
            "server" | "server-template" if matches!(section.0.as_str(), "backend" | "listen") => {
                let addr = if *first == "server" { words.get(2) } else { words.get(3) };
                if let (Some(addr), Some(b)) = (addr, backends.last_mut())
                    && b.1.is_none()
                {
                    b.1 = backend_service(addr);
                }
            }
            _ => {}
        }
    }
    let service = |b: &str| backends.iter().find(|(n, _)| n == b).map(|(n, s)| s.clone().unwrap_or_else(|| n.clone())).unwrap_or_else(|| b.to_string());
    for (backend, conds, line) in &uses {
        for c in conds {
            for (_, hosts) in acls.iter().filter(|(a, _)| a == c) {
                for h in hosts {
                    if host_like(h) {
                        out.push(serves("proxy", h.clone(), Some(service(backend)), *line));
                    }
                }
            }
        }
    }
}

/// The service a proxy backend's server address names: a Consul service, or a host name (not an address).
fn backend_service(addr: &str) -> Option<String> {
    let host = addr.rsplit_once(':').map_or(addr, |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { addr });
    if let Some(rest) = host.strip_suffix(".service.consul") {
        let name = rest.trim_start_matches('_');
        return Some(name.split("._").next().unwrap_or(name).to_string());
    }
    let ip = host.chars().all(|c| c.is_ascii_digit() || c == '.') || host.contains(':') || host.starts_with('[');
    (!ip && host_like(host)).then(|| host.to_ascii_lowercase())
}

fn host_like(h: &str) -> bool {
    !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':'))
}

/// nginx: a `server` block's `server_name`s are served by what its `proxy_pass` sends to (an `upstream`'s first
/// server, or the URL's host), or by the repo itself when it proxies nowhere.
fn nginx(src: &str, out: &mut Vec<HostFact>) {
    struct Block {
        kind: String,
        names: Vec<(String, u32)>,
        pass: Option<String>,
        servers: Vec<String>,
        name: String,
    }
    let mut stack: Vec<Block> = Vec::new();
    let mut done: Vec<Block> = Vec::new();
    for (n, raw) in src.lines().enumerate() {
        let line = n as u32 + 1;
        let text = raw.split('#').next().unwrap_or_default().trim();
        for stmt in text.split_inclusive([';', '{', '}']) {
            let s = stmt.trim();
            let words: Vec<&str> = s.trim_end_matches([';', '{', '}']).split_whitespace().collect();
            if s.ends_with('{') {
                stack.push(Block {
                    kind: words.first().unwrap_or(&"").to_string(),
                    name: words.get(1).unwrap_or(&"").to_string(),
                    names: Vec::new(),
                    pass: None,
                    servers: Vec::new(),
                });
                continue;
            }
            if s.ends_with('}') {
                if let Some(b) = stack.pop() {
                    done.push(b);
                }
                continue;
            }
            let Some(b) = stack.iter_mut().rev().find(|b| matches!(b.kind.as_str(), "server" | "upstream")) else { continue };
            match words.first().copied() {
                Some("server_name") if b.kind == "server" => {
                    b.names.extend(words[1..].iter().filter(|w| host_like(w) && **w != "_").map(|w| (w.to_ascii_lowercase(), line)))
                }
                Some("server") if b.kind == "upstream" => b.servers.extend(words.get(1).map(|w| w.to_string())),
                _ => {}
            }
            if words.first() == Some(&"proxy_pass")
                && let Some(server) = stack.iter_mut().rev().find(|b| b.kind == "server")
            {
                server.pass = server.pass.take().or_else(|| words.get(1).map(|w| w.to_string()));
            }
        }
    }
    let upstreams: Vec<(String, Option<String>)> =
        done.iter().filter(|b| b.kind == "upstream").map(|b| (b.name.clone(), b.servers.first().and_then(|s| backend_service(s)))).collect();
    for b in done.iter().filter(|b| b.kind == "server") {
        let target = b.pass.as_deref().map(|p| {
            let host = p.split_once("://").map_or(p, |(_, r)| r).split(['/', ':']).next().unwrap_or_default().to_string();
            upstreams.iter().find(|(n, _)| *n == host).map_or(Some(host.to_ascii_lowercase()), |(n, s)| Some(s.clone().unwrap_or_else(|| n.clone())))
        });
        for (name, line) in &b.names {
            out.push(serves("proxy", name.clone(), target.clone().flatten(), *line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(path: &str, src: &str) -> Vec<(&'static str, &'static str, String, Option<String>, u32)> {
        extract(path, src).hosts.into_iter().map(|h| (h.role, h.kind, h.host, h.target, h.line)).collect()
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn appsettings_give_client_hosts_by_key_and_kestrel_its_own() {
        let src = "{\n  // comment\n  \"Services\": {\n    \"Orders\": { \"BaseUrl\": \"https://orders.example.test/api\" },\n    \"Retries\": 3,\n    \"Secret\": \"not-a-url\"\n  },\n  \"Kestrel\": { \"Endpoints\": { \"Http\": { \"Url\": \"http://0.0.0.0:5080\" } } },\n  \"Hosts\": [\"https://a.example.test\", \"https://b.example.test\"]\n}\n";
        assert_eq!(
            facts("src/Web/appsettings.Production.json", src),
            [
                ("configures", "config", "orders.example.test".to_string(), s("Services:Orders:BaseUrl"), 4),
                ("serves", "kestrel", "localhost:5080".to_string(), None, 8),
                ("configures", "config", "a.example.test".to_string(), s("Hosts:0"), 9),
                ("configures", "config", "b.example.test".to_string(), s("Hosts:1"), 9),
            ]
        );
    }

    #[test]
    fn launch_profiles_serve_their_application_urls() {
        let src = "{\n  \"profiles\": {\n    \"http\": {\n      \"applicationUrl\": \"http://localhost:5222;https://localhost:7222\",\n      \"launchUrl\": \"http://localhost:9999\"\n    }\n  }\n}\n";
        assert_eq!(
            facts("src/Api/Properties/launchSettings.json", src),
            [("serves", "launch_settings", "localhost:5222".to_string(), None, 4), ("serves", "launch_settings", "localhost:7222".to_string(), None, 4)]
        );
    }

    #[test]
    fn xml_config_values_are_client_hosts() {
        let src = "<configuration>\n  <appSettings>\n    <add key=\"OrdersUrl\" value=\"https://orders.example.test/\" />\n    <add key=\"Mode\" value=\"fast\" />\n  </appSettings>\n</configuration>\n";
        assert_eq!(facts("Web.config", src), [("configures", "config", "orders.example.test".to_string(), s("OrdersUrl"), 3)]);
    }

    #[test]
    fn compose_services_built_here_serve_their_names() {
        let src = "services:\n  orders-api:\n    build: ./src/Orders\n    ports:\n      - \"5100:80\"\n    environment:\n      - CatalogUrl=http://catalog-api\n  db:\n    image: postgres\n";
        assert_eq!(
            facts("docker-compose.yml", src),
            [
                ("serves", "compose", "orders-api".to_string(), None, 2),
                ("serves", "compose", "localhost:5100".to_string(), None, 5),
                ("configures", "config", "catalog-api".to_string(), s("orders-api:CatalogUrl"), 7),
            ]
        );
    }

    #[test]
    fn ingress_rules_serve_hosts_through_their_service() {
        let src = "apiVersion: networking.k8s.io/v1\nkind: Ingress\nspec:\n  rules:\n    - host: shop.example.test\n      http:\n        paths:\n          - path: /\n            backend:\n              service:\n                name: web\n";
        assert_eq!(facts("deploy/k8s/ingress.yaml", src), [("serves", "ingress", "shop.example.test".to_string(), s("web"), 5)]);
    }

    #[test]
    fn haproxy_host_rules_route_to_backend_services() {
        let src = "frontend public\n    acl host_shop hdr(host) -i shop.example.test www.example.test\n    acl host_admin hdr(host) -i admin.example.test\n    use_backend Shop if host_shop\n    use_backend Admin if host_admin || other\n\nbackend Shop\n    server-template Shop 4 _shopapi._tcp.service.consul check\n\nbackend Admin\n    server a1 10.0.0.5:8080 check\n";
        assert_eq!(
            facts("cfgs/lb/prd/haproxy.cfg", src),
            [
                ("serves", "proxy", "shop.example.test".to_string(), s("shopapi"), 4),
                ("serves", "proxy", "www.example.test".to_string(), s("shopapi"), 4),
                ("serves", "proxy", "admin.example.test".to_string(), s("Admin"), 5),
            ]
        );
    }

    #[test]
    fn nginx_server_names_route_to_their_upstream() {
        let src = "upstream api { server orders-api:8080; }\nserver {\n  listen 80;\n  server_name shop.example.test;\n  location /api/ { proxy_pass http://api; }\n}\nserver {\n  server_name static.example.test;\n  root /var/www;\n}\n";
        assert_eq!(
            facts("deploy/nginx/site.conf", src),
            [("serves", "proxy", "shop.example.test".to_string(), s("orders-api"), 4), ("serves", "proxy", "static.example.test".to_string(), None, 8)]
        );
    }

    #[test]
    fn only_config_files_are_read() {
        for p in ["appsettings.json", "src/A/appsettings.Development.json", "Properties/launchSettings.json", "web.config", "App.Release.config", "docker-compose.override.yml", "compose.yaml", "k8s/ingress.yml", "haproxy/haproxy.cfg", "cfgs/haproxy.cfg", "nginx.conf", "nginx/conf.d/a.conf"] {
            assert!(is_host_config(p), "{p}");
        }
        for p in ["package.json", "settings.json", "packages.config", "values.yaml", "setup.cfg", "app.conf", "roles/web/tasks/main.yml"] {
            assert!(!is_host_config(p), "{p}");
        }
    }
}
