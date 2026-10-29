//! Go server routes: the registrations gin, echo, chi and `net/http` read, as server routes of the function
//! registering them (eval/routes-v1/README.md measures them against each framework's own listing).
//!
//! A router is a value the syntax places: a framework's constructor (`gin.Default()`, `echo.New()`,
//! `chi.NewRouter()`, `http.NewServeMux()`), a group of one (`r.Group("/v1")`, `r.With(mw)`), the router chi hands a
//! `Route` or `Group` closure, or a parameter of a router type. Each carries the prefix of its routes, encoded as the
//! store resolves route groups (`kc_store::query::http_routes`): a router a function receives as parameter `i` starts
//! with `{@F#i}`, the prefixes callers pass it (a `group` row each); one a function builds and returns starts with
//! `{@F#ret}`, the prefixes it is mounted under. A route whose path isn't a literal or a constant is `unsupported`.

use tree_sitter::Node;

use super::Cx;
use crate::swift::{children, line};
use crate::{HttpRoute, StringConst, Unsupported};

/// The frameworks whose routes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fw {
    Gin,
    Echo,
    Chi,
    NetHttp,
}

/// A router value: its framework and the encoded prefix of the routes registered on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Router {
    pub fw: Fw,
    pub prefix: String,
}

/// The framework an import path is, if one read here.
pub(super) fn framework(path: &str) -> Option<Fw> {
    match path {
        "github.com/gin-gonic/gin" => Some(Fw::Gin),
        "github.com/labstack/echo" | "github.com/labstack/echo/v4" | "github.com/labstack/echo/v5" => Some(Fw::Echo),
        "github.com/go-chi/chi" | "github.com/go-chi/chi/v5" => Some(Fw::Chi),
        "net/http" => Some(Fw::NetHttp),
        _ => None,
    }
}

const GIN_ECHO_VERBS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT", "TRACE"];
const CHI_VERBS: &[&str] = &["Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "Connect", "Trace"];

/// `prefix` and `path` joined as a stored route path: segments, no leading or trailing `/`; a prefix ending in a
/// group key (`{@F#0}`) is followed directly.
fn join(prefix: &str, path: &str) -> String {
    let path = path.trim_matches('/');
    if prefix.is_empty() {
        return path.to_string();
    }
    if path.is_empty() {
        return prefix.trim_end_matches('/').to_string();
    }
    if prefix.starts_with("{@") && prefix.find('}') == Some(prefix.len() - 1) {
        format!("{prefix}{path}")
    } else {
        format!("{}/{path}", prefix.trim_end_matches('/'))
    }
}

/// A `net/http` pattern's method and path: `"GET /items/{id}"` is `GET` and `/items/{id}`; no method answers every
/// one. `{$}`, which anchors the path's end, isn't part of it.
fn pattern(p: &str) -> (String, String) {
    let (verb, rest) = match p.split_once(' ') {
        Some((v, r)) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_uppercase()) => (v.to_string(), r.trim_start()),
        _ => ("*".to_string(), p),
    };
    // A host before the path (`example.com/x`) is kept as written: the route is the host's.
    (verb, rest.replace("{$}", ""))
}

impl<'a> Cx<'a> {
    /// The framework an import qualifier (`gin`, `chi`, an alias) names in this file.
    fn fw_of(&self, q: &str) -> Option<Fw> {
        self.frameworks.iter().find(|(name, _)| name == q).map(|(_, fw)| *fw)
    }

    pub(super) fn router_named(&self, name: &str) -> Option<Router> {
        self.scopes.iter().rev().find_map(|s| s.routers.get(name).cloned()).or_else(|| {
            (self.local(name).is_none()).then(|| self.package_routers.get(name).cloned()).flatten()
        })
    }

    /// The framework a file importing gin or echo registers with, for a receiver the syntax doesn't place (a router
    /// in a field, or an application's own wrapper around one): their upper-case verb methods (`GET`, `POST`) with a
    /// literal path are registrations whatever the receiver is.
    fn verb_framework(&self) -> Option<Fw> {
        self.frameworks.iter().map(|(_, fw)| *fw).find(|fw| matches!(fw, Fw::Gin | Fw::Echo))
    }

    /// The router a type names: `*gin.Engine`, `gin.IRouter`, `*echo.Group`, `chi.Router`, `*http.ServeMux`, …
    pub(super) fn router_type(&self, t: Node) -> Option<Fw> {
        let text: String = self.text(t).trim_start_matches('*').split_whitespace().collect();
        let (q, name) = text.split_once('.')?;
        let fw = self.fw_of(q)?;
        let is = match fw {
            Fw::Gin => matches!(name, "Engine" | "RouterGroup" | "IRouter" | "IRoutes"),
            Fw::Echo => matches!(name, "Echo" | "Group"),
            Fw::Chi => matches!(name, "Router" | "Mux"),
            Fw::NetHttp => name == "ServeMux",
        };
        is.then_some(fw)
    }

    /// The router an expression is, when the syntax says: a router variable, a constructor, or a group of one.
    pub(super) fn router_of(&self, e: Node) -> Option<Router> {
        match e.kind() {
            "identifier" => self.router_named(self.text(e)),
            "parenthesized_expression" | "unary_expression" => children(e).filter(|c| c.is_named()).last().and_then(|c| self.router_of(c)),
            "call_expression" => {
                let f = e.child_by_field_name("function")?;
                if f.kind() != "selector_expression" {
                    return None;
                }
                let operand = f.child_by_field_name("operand")?;
                let method = self.text(f.child_by_field_name("field")?);
                let args: Vec<Node> = e.child_by_field_name("arguments").map(|a| children(a).filter(|c| c.is_named()).collect()).unwrap_or_default();
                if operand.kind() == "identifier"
                    && self.local(self.text(operand)).is_none()
                    && let Some(fw) = self.fw_of(self.text(operand))
                {
                    let made = match fw {
                        Fw::Gin => matches!(method, "Default" | "New"),
                        Fw::Echo => method == "New",
                        Fw::Chi => matches!(method, "NewRouter" | "NewMux"),
                        Fw::NetHttp => method == "NewServeMux",
                    };
                    return made.then(|| Router { fw, prefix: String::new() });
                }
                let base = match self.router_of(operand) {
                    Some(b) => b,
                    // A group of a router the syntax doesn't place, in a file registering with gin or echo.
                    None if matches!(method, "Group" | "SubGroup") && args.first().is_some_and(|a| self.literal(*a).is_some()) => {
                        Router { fw: self.verb_framework()?, prefix: String::new() }
                    }
                    None => return None,
                };
                match (base.fw, method) {
                    (Fw::Gin | Fw::Echo, "Group" | "SubGroup") => {
                        let p = self.literal(*args.first()?)?;
                        Some(Router { fw: base.fw, prefix: join(&base.prefix, &p) })
                    }
                    (Fw::Chi, "With") | (Fw::Gin | Fw::Echo, "Use") => Some(base),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// A route path written as a string literal, or `{=Name}` for a constant of this package.
    fn literal(&self, e: Node) -> Option<String> {
        match e.kind() {
            "interpreted_string_literal" | "raw_string_literal" => {
                let t = self.text(e);
                Some(t[1..t.len().saturating_sub(1)].to_string())
            }
            "identifier" if self.local(self.text(e)).is_none() => Some(format!("{{={}}}", self.text(e))),
            _ => None,
        }
    }

    fn push_route(&mut self, verb: &str, path: String, at: Node) {
        self.facts.http_routes.push(HttpRoute {
            sym: self.current(),
            side: "server",
            verb: verb.to_string(),
            path,
            path_ref: None,
            service: None,
            service_ref: None,
            line: line(at),
        });
    }

    fn push_group(&mut self, key: String, prefix: Result<String, String>, at: Node) {
        let (verb, path) = match prefix {
            Ok(p) => (String::new(), p),
            Err(why) => ("?".to_string(), why),
        };
        self.facts.http_routes.push(HttpRoute {
            sym: self.current(),
            side: "group",
            verb,
            path,
            path_ref: None,
            service: Some(key),
            service_ref: None,
            line: line(at),
        });
    }

    fn unsupported_route(&mut self, style: &str, why: &str, at: Node) {
        self.facts.unsupported.push(Unsupported { area: "http", construct: format!("{style}: {why}"), line: line(at) });
    }

    /// The key a function's router parameters and result are known by, as callers name it: `pkg.F`, `pkg.T.M`.
    pub(super) fn func_key(&self, recv: Option<&str>, name: &str) -> String {
        let pkg = self.facts.package.clone().unwrap_or_default();
        match recv {
            Some(r) => format!("{pkg}.{r}.{name}"),
            None => format!("{pkg}.{name}"),
        }
    }

    /// The key of the function a call calls, when the syntax names it: `F(…)`, `q.F(…)`, `x.M(…)` with `x` typed.
    fn callee_key(&self, f: Node) -> Option<String> {
        match f.kind() {
            "identifier" => Some(self.func_key(None, self.text(f))),
            "selector_expression" => {
                let operand = f.child_by_field_name("operand")?;
                let name = self.text(f.child_by_field_name("field")?);
                if operand.kind() == "identifier" && self.local(self.text(operand)).is_none() && self.is_import(self.text(operand)) {
                    let q = self.text(operand);
                    if self.fw_of(q).is_some() {
                        return None;
                    }
                    let binding = self.facts.bindings.iter().find(|b| b.local == q || (b.local.is_empty() && crate::go::package_name_guess(&b.module) == q));
                    // The standard library has no helpers of the repository's.
                    if binding.is_some_and(|b| !b.module.split('/').next().unwrap_or("").contains('.')) {
                        return None;
                    }
                    let pkg = binding.map_or(q.to_string(), |b| crate::go::package_name_guess(&b.module).to_string());
                    return Some(format!("{pkg}.{name}"));
                }
                match self.expr_type(operand)? {
                    crate::types::TypeExpr::Named(t) if !t.contains('.') => Some(self.func_key(Some(&t), name)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Reads a call for routes: a registration on a router, a group or mount, or a router passed to a helper.
    /// Doesn't visit the call; the caller does, as for any call.
    pub(super) fn route_call(&mut self, call: Node, f: Node) {
        let args: Vec<Node> = call.child_by_field_name("arguments").map(|a| children(a).filter(|c| c.is_named()).collect()).unwrap_or_default();
        // A router passed to a helper: the helper's parameter gets this router's prefix.
        if let Some(key) = self.callee_key(f) {
            for (i, a) in args.iter().enumerate() {
                if let Some(r) = self.router_of(*a) {
                    self.push_group(format!("{key}#{i}"), Ok(r.prefix), *a);
                }
            }
        }
        if f.kind() != "selector_expression" {
            return;
        }
        let (Some(operand), Some(field)) = (f.child_by_field_name("operand"), f.child_by_field_name("field")) else { return };
        let method = self.text(field);
        // `http.HandleFunc(…)`: the default ServeMux.
        let router = if operand.kind() == "identifier"
            && self.local(self.text(operand)).is_none()
            && self.fw_of(self.text(operand)) == Some(Fw::NetHttp)
            && matches!(method, "Handle" | "HandleFunc")
        {
            Some(Router { fw: Fw::NetHttp, prefix: String::new() })
        } else {
            self.router_of(operand).or_else(|| {
                let literal_path = args.first().and_then(|a| self.literal(*a)).is_some_and(|p| p.starts_with('/'));
                (args.len() >= 2 && literal_path && (GIN_ECHO_VERBS.contains(&method) || matches!(method, "Any" | "Static" | "StaticFile")))
                    .then(|| self.verb_framework().map(|fw| Router { fw, prefix: String::new() }))
                    .flatten()
            })
        };
        let Some(router) = router else { return };
        let path_at = |i: usize| args.get(i).copied();
        match (router.fw, method) {
            (Fw::Gin | Fw::Echo, m) if GIN_ECHO_VERBS.contains(&m) => self.route(&router, m, path_at(0), call),
            (Fw::Gin | Fw::Echo, "Any") => self.route(&router, "*", path_at(0), call),
            (Fw::Gin, "Handle") | (Fw::Echo, "Add") => {
                let verb = path_at(0).and_then(|v| self.literal(v)).filter(|v| !v.starts_with("{="));
                match verb {
                    Some(v) => self.route(&router, &v.to_uppercase(), path_at(1), call),
                    None => self.unsupported_route(method, "its method isn't a literal", call),
                }
            }
            (Fw::Gin | Fw::Echo, "Match") => {
                let verbs: Vec<String> = path_at(0)
                    .map(|l| {
                        let mut out = Vec::new();
                        let mut stack = vec![l];
                        while let Some(n) = stack.pop() {
                            if let Some(v) = (n.kind() == "interpreted_string_literal").then(|| self.literal(n)).flatten() {
                                out.push(v.to_uppercase());
                            } else if n.kind() == "selector_expression"
                                && let Some(v) = self.method_constant(n)
                            {
                                out.push(v);
                                continue;
                            }
                            let mut kids: Vec<Node> = children(n).collect();
                            kids.reverse();
                            stack.extend(kids);
                        }
                        out
                    })
                    .unwrap_or_default();
                if verbs.is_empty() {
                    self.unsupported_route(method, "its methods aren't literals", call);
                }
                for v in verbs {
                    self.route(&router, &v, path_at(1), call);
                }
            }
            (Fw::Gin, "Static" | "StaticFS") => self.route(&router, "*", path_at(0).map(|_| call), call),
            (Fw::Gin, "StaticFile" | "StaticFileFS") => {
                self.route(&router, "GET", path_at(0), call);
                self.route(&router, "HEAD", path_at(0), call);
            }
            // echo serves a directory at the prefix followed directly by `*` (`/avatar*`).
            (Fw::Echo, "Static") => match path_at(0).and_then(|p| self.literal(p)) {
                Some(p) => self.push_route("GET", join(&router.prefix, &format!("{}*", p.trim_end_matches('/'))), call),
                None => self.unsupported_route("Static", "its prefix isn't a literal", call),
            },
            (Fw::Echo, "File") => self.route(&router, "GET", path_at(0), call),
            (Fw::Chi, m) if CHI_VERBS.contains(&m) => self.route(&router, &m.to_uppercase(), path_at(0), call),
            (Fw::Chi, "Handle" | "HandleFunc") => self.route(&router, "*", path_at(0), call),
            (Fw::Chi, "Method" | "MethodFunc") => {
                let verb = path_at(0).and_then(|v| self.literal(v)).filter(|v| !v.starts_with("{="));
                match verb {
                    Some(v) => self.route(&router, &v.to_uppercase(), path_at(1), call),
                    None => self.unsupported_route(method, "its method isn't a literal", call),
                }
            }
            (Fw::Chi, "Mount") => {
                let Some(p) = path_at(0).and_then(|p| self.literal(p)) else {
                    return self.unsupported_route("Mount", "its path isn't a literal", call);
                };
                let at = join(&router.prefix, &p);
                // A router built and returned by a function: its routes go under this prefix.
                // A router a function of this package builds (`adminRouter()`, `todos{}.Routes()`); a call into another
                // package (`promhttp.Handler()`) is a handler chi can't walk into.
                let mounted = path_at(1).and_then(|h| match h.kind() {
                    "call_expression" => h
                        .child_by_field_name("function")
                        .filter(|f| !self.qualified_by_import(*f))
                        .and_then(|f| self.callee_key(f))
                        .map(|k| format!("{k}#ret")),
                    "identifier" => self.router_named(self.text(h)).and_then(|r| r.prefix.strip_prefix("{@").and_then(|k| k.strip_suffix('}')).map(str::to_string)),
                    _ => None,
                });
                match mounted {
                    Some(key) => self.push_group(key, Ok(at), call),
                    // A handler chi can't walk into: everything under the prefix, any method.
                    None => self.push_route("*", join(&at, "*"), call),
                }
            }
            (Fw::Chi, "Route") => {
                if let Some(p) = path_at(0).and_then(|p| self.literal(p)) {
                    self.pending_router = Some(Router { fw: Fw::Chi, prefix: join(&router.prefix, &p) });
                } else {
                    self.unsupported_route("Route", "its path isn't a literal", call);
                }
            }
            (Fw::Chi, "Group") => self.pending_router = Some(router.clone()),
            (Fw::NetHttp, "Handle" | "HandleFunc") => match path_at(0).and_then(|p| self.literal(p)) {
                Some(p) if !p.starts_with("{=") => {
                    let (verb, path) = pattern(&p);
                    let path = join(&router.prefix, &path);
                    self.push_route(&verb, path, call);
                }
                _ => self.unsupported_route(method, "its pattern isn't a literal", call),
            },
            _ => {}
        }
    }

    /// The method a `net/http` constant names: `http.MethodGet` is `GET`.
    fn method_constant(&self, n: Node) -> Option<String> {
        let operand = n.child_by_field_name("operand")?;
        let field = self.text(n.child_by_field_name("field")?);
        (self.fw_of(self.text(operand)) == Some(Fw::NetHttp)).then(|| field.strip_prefix("Method").map(str::to_uppercase)).flatten()
    }

    /// Whether a callee is a function of an imported package (`q.F`).
    fn qualified_by_import(&self, f: Node) -> bool {
        f.kind() == "selector_expression"
            && f.child_by_field_name("operand").is_some_and(|o| o.kind() == "identifier" && self.local(self.text(o)).is_none() && self.is_import(self.text(o)))
    }

    fn route(&mut self, router: &Router, verb: &str, path: Option<Node>, call: Node) {
        if path.map(|p| p.id()) == Some(call.id()) {
            // gin's Static and StaticFS: the directory's files, under the path as `path/*filepath`.
            if let Some(p) = children(call.child_by_field_name("arguments").unwrap_or(call)).find(|c| c.is_named()).and_then(|a| self.literal(a)) {
                let path = join(&router.prefix, &join(&p, "*filepath"));
                self.push_route("GET", path.clone(), call);
                self.push_route("HEAD", path, call);
            }
            return;
        }
        match path.and_then(|p| self.literal(p)) {
            Some(p) => {
                let path = join(&router.prefix, &p);
                self.push_route(verb, path, call);
            }
            None => self.unsupported_route(verb, "its path isn't a literal or a constant", call),
        }
    }

    /// A package-level string constant, for route paths naming it (`{=Name}`).
    pub(super) fn string_const(&mut self, name: &str, value: Node, at: Node) {
        if let Some(v) = self.literal(value).filter(|v| !v.starts_with("{=")) {
            self.facts.constants.push(StringConst { qualified: name.to_string(), value: v, line: line(at) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes(src: &str) -> Vec<(String, String, String)> {
        crate::go::extract("orchard/routes.go", src)
            .http_routes
            .iter()
            .map(|r| (r.side.to_string(), r.verb.clone(), r.service.clone().map_or(r.path.clone(), |k| format!("{k} <- {}", r.path))))
            .collect()
    }

    fn row(side: &str, verb: &str, path: &str) -> (String, String, String) {
        (side.to_string(), verb.to_string(), path.to_string())
    }

    #[test]
    fn gin_groups_and_helpers() {
        let got = routes(
            "package orchard\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc main() {\n\tr := gin.Default()\n\tr.GET(\"/ping\", ping)\n\tv1 := r.Group(\"/v1\")\n\t{\n\t\tv1.POST(\"/crates\", create)\n\t\tv1.Any(\"/any\", any)\n\t}\n\taddApples(v1)\n\tr.Run()\n}\n\nfunc addApples(rg *gin.RouterGroup) {\n\tapples := rg.Group(\"/apples\")\n\tapples.GET(\"/:id\", get)\n}\n",
        );
        assert_eq!(
            got,
            [
                row("server", "GET", "ping"),
                row("server", "POST", "v1/crates"),
                row("server", "*", "v1/any"),
                row("group", "", "orchard.addApples#0 <- v1"),
                row("server", "GET", "{@orchard.addApples#0}apples/:id"),
            ]
        );
    }

    #[test]
    fn chi_routes_groups_and_mounts() {
        let got = routes(
            "package orchard\n\nimport (\n\t\"net/http\"\n\n\t\"github.com/go-chi/chi/v5\"\n)\n\nfunc main() {\n\tr := chi.NewRouter()\n\tr.Get(\"/\", home)\n\tr.Route(\"/crates\", func(r chi.Router) {\n\t\tr.With(auth).Post(\"/\", create)\n\t\tr.Group(func(r chi.Router) {\n\t\t\tr.Get(\"/{id}\", get)\n\t\t})\n\t})\n\tr.Mount(\"/admin\", adminRouter())\n\tr.Mount(\"/static\", http.FileServer(nil))\n\thttp.ListenAndServe(\":3333\", r)\n}\n\nfunc adminRouter() chi.Router {\n\tr := chi.NewRouter()\n\tr.Get(\"/accounts\", accounts)\n\treturn r\n}\n",
        );
        assert_eq!(
            got,
            [
                row("server", "GET", ""),
                row("server", "POST", "crates"),
                row("server", "GET", "crates/{id}"),
                row("group", "", "orchard.adminRouter#ret <- admin"),
                row("server", "*", "static/*"),
                row("server", "GET", "{@orchard.adminRouter#ret}accounts"),
            ]
        );
    }

    #[test]
    fn echo_and_net_http() {
        let got = routes(
            "package orchard\n\nimport (\n\t\"net/http\"\n\n\t\"github.com/labstack/echo/v4\"\n)\n\nconst apiPrefix = \"/api\"\n\nfunc main() {\n\te := echo.New()\n\tg := e.Group(apiPrefix)\n\tg.PUT(\"/crates/:id\", update)\n\te.Match([]string{\"GET\", \"HEAD\"}, \"/health\", health)\n\tmux := http.NewServeMux()\n\tmux.HandleFunc(\"GET /items/{id}\", item)\n\tmux.HandleFunc(\"/{$}\", index)\n\thttp.HandleFunc(\"POST /hook\", hook)\n\tmux.HandleFunc(path, other)\n}\n",
        );
        assert_eq!(
            got,
            [
                row("server", "PUT", "{=apiPrefix}/crates/:id"),
                row("server", "GET", "health"),
                row("server", "HEAD", "health"),
                row("server", "GET", "items/{id}"),
                row("server", "*", ""),
                row("server", "POST", "hook"),
            ]
        );
        let f = crate::go::extract("orchard/routes.go", "package orchard\n\nimport \"net/http\"\n\nfunc main() {\n\tmux := http.NewServeMux()\n\tmux.HandleFunc(path, other)\n}\n");
        assert_eq!(f.unsupported.iter().map(|u| u.area).collect::<Vec<_>>(), ["http"]);
    }

    #[test]
    fn joining_and_patterns() {
        assert_eq!(join("", "/v1/"), "v1");
        assert_eq!(join("v1", "/users"), "v1/users");
        assert_eq!(join("{@shop.routes#0}", "/users"), "{@shop.routes#0}users");
        assert_eq!(join("{@shop.routes#0}v1", "users"), "{@shop.routes#0}v1/users");
        assert_eq!(join("api", "/"), "api");
        assert_eq!(pattern("GET /items/{id}"), ("GET".to_string(), "/items/{id}".to_string()));
        assert_eq!(pattern("/{$}"), ("*".to_string(), "/".to_string()));
    }
}
