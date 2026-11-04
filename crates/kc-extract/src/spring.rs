//! Spring MVC and WebFlux request mappings, shared by the Java and Kotlin extractors: each handler method's routes,
//! from `@RequestMapping` and its shortcuts (`@GetMapping`, …) on the method, joined with its type's
//! `@RequestMapping` as Spring joins them. The extractors read the annotations' arguments in their own syntax into
//! [`Mapping`]s; this module decides which types hold handlers and what routes a method serves.
//!
//! A path is stored as the stored-route encoding reads it (`kc_store::query::http_routes`): segments without leading
//! or trailing `/`, a constant as `{=Name}` (resolved at query time against the repo's string constants), and
//! Spring's own `{id}` and `{id:[0-9]+}` variables as written.

/// What a mapping annotation says, read from its arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mapping {
    /// Its paths (`value` or `path`), encoded; empty when it names none, which is the type's path alone.
    pub paths: Vec<String>,
    /// Its methods (`method = RequestMethod.GET`), upper case; empty for every method.
    pub verbs: Vec<String>,
}

/// The verb a mapping annotation fixes: `Some(None)` for `@RequestMapping` (its `method` argument says), `Some(verb)`
/// for a shortcut, `None` for any other annotation.
pub fn annotation_verb(name: &str) -> Option<Option<&'static str>> {
    Some(match name {
        "RequestMapping" => None,
        "GetMapping" => Some("GET"),
        "PostMapping" => Some("POST"),
        "PutMapping" => Some("PUT"),
        "DeleteMapping" => Some("DELETE"),
        "PatchMapping" => Some("PATCH"),
        _ => return None,
    })
}

/// The verb a `method` argument's value names: `RequestMethod.GET`, or `GET` statically imported.
pub fn request_method(written: &str) -> Option<String> {
    let verb = written.rsplit('.').next()?.trim();
    matches!(verb, "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "TRACE").then(|| verb.to_string())
}

/// Whether the types annotated `annotations` are a client's, whose mappings describe requests sent, not served: a
/// `@FeignClient` interface, or a `@HttpExchange` one (Spring 6's declarative client).
pub fn is_client(annotations: &[String]) -> bool {
    annotations.iter().any(|a| matches!(a.as_str(), "FeignClient" | "HttpExchange"))
}

/// Whether a type's handler methods are served: a `@Controller` or `@RestController`, a type mapped by
/// `@RequestMapping` (Spring before 6 detected handlers by it alone), or an interface or abstract class whose
/// mappings a controller implementing it inherits. A client's interface ([`is_client`]) is never.
pub fn serves(annotations: &[String], abstract_type: bool) -> bool {
    !is_client(annotations) && (abstract_type || annotations.iter().any(|a| matches!(a.as_str(), "Controller" | "RestController" | "RequestMapping")))
}

/// `path` as stored: its segments, no leading or trailing `/`.
fn trim(path: &str) -> &str {
    path.trim_matches('/')
}

/// The routes a handler method serves, as (verb, path): each of its type's paths joined with each of its own, for
/// each of its methods. Spring combines a type's and a method's `method` conditions by union, and a mapping with none
/// answers every method (`*`). A path written absolute on the method is still under the type's path, as in Spring.
pub fn routes(ty: Option<&Mapping>, method: &Mapping, fixed: Option<&str>) -> Vec<(String, String)> {
    let empty = Mapping::default();
    let ty = ty.unwrap_or(&empty);
    let mut verbs: Vec<String> = fixed.map(str::to_string).into_iter().chain(method.verbs.iter().cloned()).collect();
    for v in &ty.verbs {
        if !verbs.contains(v) && !verbs.is_empty() {
            verbs.push(v.clone());
        }
    }
    if verbs.is_empty() {
        verbs = if ty.verbs.is_empty() { vec!["*".to_string()] } else { ty.verbs.clone() };
    }
    let one = [String::new()];
    let prefixes = if ty.paths.is_empty() { &one[..] } else { &ty.paths[..] };
    let paths = if method.paths.is_empty() { &one[..] } else { &method.paths[..] };
    let mut out = Vec::new();
    for prefix in prefixes {
        for path in paths {
            let joined = [trim(prefix), trim(path)].iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("/");
            for verb in &verbs {
                let route = (verb.clone(), joined.clone());
                if !out.contains(&route) {
                    out.push(route);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(paths: &[&str], verbs: &[&str]) -> Mapping {
        Mapping { paths: paths.iter().map(|s| s.to_string()).collect(), verbs: verbs.iter().map(|s| s.to_string()).collect() }
    }

    fn r(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn a_method_mapping_is_joined_under_each_type_path() {
        assert_eq!(routes(Some(&m(&["/api/orders"], &[])), &m(&["/{id}"], &[]), Some("GET")), r(&[("GET", "api/orders/{id}")]));
        assert_eq!(routes(Some(&m(&["/api/orders/"], &[])), &m(&[], &[]), Some("POST")), r(&[("POST", "api/orders")]));
        assert_eq!(
            routes(Some(&m(&["a", "/b"], &[])), &m(&["x", "y/"], &[]), Some("PUT")),
            r(&[("PUT", "a/x"), ("PUT", "a/y"), ("PUT", "b/x"), ("PUT", "b/y")])
        );
        assert_eq!(routes(None, &m(&["/"], &[]), Some("GET")), r(&[("GET", "")]));
    }

    #[test]
    fn request_mapping_methods_combine_and_default_to_every_method() {
        assert_eq!(routes(None, &m(&["/x"], &[]), None), r(&[("*", "x")]));
        assert_eq!(routes(None, &m(&["/x"], &["GET", "POST"]), None), r(&[("GET", "x"), ("POST", "x")]));
        assert_eq!(routes(Some(&m(&[], &["GET"])), &m(&["/x"], &[]), None), r(&[("GET", "x")]));
        assert_eq!(routes(Some(&m(&[], &["GET"])), &m(&["/x"], &[]), Some("POST")), r(&[("POST", "x"), ("GET", "x")]));
    }

    #[test]
    fn handlers_are_served_by_controllers_never_by_clients() {
        let a = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(serves(&a(&["RestController"]), false));
        assert!(serves(&a(&["RequestMapping"]), false));
        assert!(serves(&a(&[]), true));
        assert!(!serves(&a(&["Service"]), false));
        assert!(!serves(&a(&["FeignClient"]), true));
        assert_eq!(request_method("RequestMethod.DELETE").as_deref(), Some("DELETE"));
        assert_eq!(request_method("FOO"), None);
    }
}
