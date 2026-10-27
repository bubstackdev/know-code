//! Request paths built in code: a URL argument to an HTTP client call, or a gateway call's path, read as a sequence
//! of pieces and encoded for [`crate::HttpRoute::path`].
//!
//! The encoding keeps literal text as written, writes `{=Name}` for a constant to resolve at query time (`Api.x`,
//! `ServiceNames.Path`), and `{}` for any other interpolated value (a route parameter, to the linker). An unresolved
//! constant at the start of the path is taken for a base address and dropped (see `kc_store::query::client_path`).

/// One piece of a URL expression: `$"{baseUrl}/orders/{id}"` is `[Unknown, Text("/orders/"), Unknown]`.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    /// A name that may be a string constant: `Api.orders`, `ServiceNames.Path`, `API`.
    Ref(String),
    /// Any other value: at the start of a URL, its base address.
    Unknown,
    /// A value computed by a call (`${base(account)}/health`): it may build part of the path, so it stands for a
    /// parameter even at the start.
    Call,
}

/// Encodes a URL's pieces. With `base`, the URL may start with a base address: unknown values before the first text
/// are dropped, and a URL made only of values keeps the last one when it is a constant (`${base}${sep}${Api.x}`).
/// `None` when nothing in it names a path.
pub fn encode(pieces: &[Piece], base: bool) -> Option<String> {
    let first_text = pieces.iter().position(|p| matches!(p, Piece::Text(t) if !t.is_empty()));
    let kept: &[Piece] = match (base, first_text) {
        (true, None) => match pieces.last() {
            Some(last @ Piece::Ref(_)) => std::slice::from_ref(last),
            _ => return None,
        },
        _ => pieces,
    };
    let mut out = String::new();
    let mut leading = base;
    for p in kept {
        match p {
            Piece::Text(t) => {
                leading = leading && t.is_empty();
                out.push_str(t);
            }
            Piece::Ref(r) => {
                out.push_str("{=");
                out.push_str(r);
                out.push('}');
            }
            Piece::Unknown if leading => {}
            Piece::Unknown => out.push_str("{}"),
            Piece::Call => {
                leading = false;
                out.push_str("{}");
            }
        }
    }
    let named = out.split(['/', '?']).any(|s| !s.is_empty() && s != "{}");
    named.then_some(out)
}

/// Whether an encoded path plausibly names an HTTP path rather than some other string (a cache key): it has a `/`
/// or a constant in it.
pub fn looks_like_path(encoded: &str) -> bool {
    encoded.contains('/') || encoded.contains("{=")
}

/// The host of an absolute URL (`https://user@Orders.Example.com:443/api` → `orders.example.com`), lower-cased, with
/// its user info, path and a default port (80, 443) dropped. A wildcard listening address (`0.0.0.0`, `*`, `+`,
/// `[::]`) is `localhost`. `None` for a relative URL, or one whose host is a placeholder (`{base}`, `#host#`).
pub fn host_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default().to_ascii_lowercase();
    let (host, port) = match authority.find(']') {
        Some(end) => (&authority[..=end], authority[end + 1..].strip_prefix(':')),
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority.as_str(), None),
        },
    };
    let port = match port.map(str::parse::<u16>) {
        None | Some(Ok(80 | 443)) => None,
        Some(Ok(n)) => Some(n),
        Some(Err(_)) => return None,
    };
    let host = match host {
        "0.0.0.0" | "*" | "+" | "[::]" | "127.0.0.1" => "localhost".to_string(),
        _ => host.to_string(),
    };
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '[' | ']' | ':')) {
        return None;
    }
    Some(match port {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

/// `HttpMethod.Get`, `HttpMethods.Delete`, `'GET'`, `"post"`: an upper-case HTTP verb.
pub fn verb(name: &str) -> Option<String> {
    let v = name.trim_matches(['"', '\'', '`']).to_ascii_uppercase();
    matches!(v.as_str(), "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD" | "OPTIONS").then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Piece::*;

    fn t(s: &str) -> Piece {
        Text(s.to_string())
    }

    #[test]
    fn encodes_bases_constants_and_parameters() {
        assert_eq!(encode(&[Unknown, t("/orders/"), Unknown, t("/lines")], true).as_deref(), Some("/orders/{}/lines"));
        assert_eq!(encode(&[Ref("API".into()), t("/projects")], true).as_deref(), Some("{=API}/projects"));
        assert_eq!(encode(&[Unknown, Unknown, Ref("Api.x".into())], true).as_deref(), Some("{=Api.x}"));
        assert_eq!(encode(&[t("https://h.example/api/x?id="), Unknown], true).as_deref(), Some("https://h.example/api/x?id={}"));
        assert_eq!(encode(&[Call, t("/health")], true).as_deref(), Some("{}/health"));
        // Without a base, a leading value is a path parameter.
        assert_eq!(encode(&[Unknown, t("/details")], false).as_deref(), Some("{}/details"));
        // Nothing named: no path.
        assert_eq!(encode(&[Unknown, t("/"), Unknown], true), None);
        assert_eq!(encode(&[Unknown], true), None);
        assert!(looks_like_path("{=Api.x}") && looks_like_path("a/b") && !looks_like_path("cache-key"));
    }

    #[test]
    fn hosts_keep_name_and_port_only() {
        assert_eq!(host_of("https://user:pw@Orders.Example.com:443/api?x=1").as_deref(), Some("orders.example.com"));
        assert_eq!(host_of("http://localhost:5222/").as_deref(), Some("localhost:5222"));
        assert_eq!(host_of("https+http://catalog-api").as_deref(), Some("catalog-api"));
        assert_eq!(host_of("http://0.0.0.0:8080").as_deref(), Some("localhost:8080"));
        assert_eq!(host_of("http://*:5000").as_deref(), Some("localhost:5000"));
        assert_eq!(host_of("/api/orders"), None);
        assert_eq!(host_of("https://{base}/x"), None);
        assert_eq!(host_of("https://#host#/x"), None);
    }
}
