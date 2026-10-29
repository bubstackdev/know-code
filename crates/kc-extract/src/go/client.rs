//! Go HTTP client calls: `net/http`'s `Get`, `Head`, `Post`, `PostForm`, `NewRequest` and `NewRequestWithContext`,
//! and the same methods of an `http.Client`, as client routes of the calling function. A request built with
//! `NewRequest` is the call site; the `Do` that sends it adds nothing the request doesn't say.
//!
//! The URL is read as [`urls`] pieces: string literals, `+`, `fmt.Sprintf`'s format with its verbs as values, a
//! package constant as `{=Name}`. A value before any text is a base address (`base + "/orders"`), dropped as the
//! other languages' clients drop theirs.

use tree_sitter::Node;

use super::Cx;
use super::http::Fw;
use crate::HttpRoute;
use crate::swift::{children, line};
use crate::types::TypeExpr;
use crate::urls::{self, Piece};

impl<'a> Cx<'a> {
    /// A client call `q.M(args)`: a request, read as a client route; `false` when the call isn't one.
    pub(super) fn client_call(&mut self, call: Node, operand: Node, method: &str, args: &[Node]) -> bool {
        if !self.is_http_client(operand) {
            return false;
        }
        let (verb, url) = match (method, args) {
            ("Get", [url, ..]) => (Some("GET".to_string()), *url),
            ("Head", [url, ..]) => (Some("HEAD".to_string()), *url),
            ("Post" | "PostForm", [url, ..]) => (Some("POST".to_string()), *url),
            ("NewRequest", [m, url, ..]) | ("NewRequestWithContext", [_, m, url, ..]) => (self.request_method(*m), *url),
            _ => return false,
        };
        let path = urls::encode(&self.url_pieces(url), true).filter(|p| urls::looks_like_path(p)).unwrap_or_default();
        match verb {
            Some(verb) => self.facts.http_routes.push(HttpRoute {
                sym: self.current(),
                side: "client",
                verb,
                path,
                path_ref: None,
                service: None,
                service_ref: None,
                line: line(call),
            }),
            // A method chosen at run time (a wrapper's parameter): the request is seen but can't be linked.
            None => self.unsupported_route(method, "its method isn't a literal", call),
        }
        true
    }

    /// Whether a receiver is `net/http` itself, `http.DefaultClient`, or a value typed `http.Client`.
    fn is_http_client(&self, operand: Node) -> bool {
        let is_http = |q: &str| self.fw_of(q) == Some(Fw::NetHttp);
        match operand.kind() {
            "identifier" if self.local(self.text(operand)).is_none() && is_http(self.text(operand)) => true,
            "selector_expression" => {
                let (Some(o), Some(f)) = (operand.child_by_field_name("operand"), operand.child_by_field_name("field")) else { return false };
                (o.kind() == "identifier" && self.local(self.text(o)).is_none() && is_http(self.text(o)) && self.text(f) == "DefaultClient")
                    || self.is_client_type(operand)
            }
            _ => self.is_client_type(operand),
        }
    }

    fn is_client_type(&self, e: Node) -> bool {
        matches!(self.expr_type(e), Some(TypeExpr::Named(n)) if n.split_once('.').is_some_and(|(q, t)| t == "Client" && self.fw_of(q) == Some(Fw::NetHttp)))
    }

    /// A request's method: a string literal or a `net/http` constant (`http.MethodPost`).
    fn request_method(&self, m: Node) -> Option<String> {
        match m.kind() {
            "interpreted_string_literal" | "raw_string_literal" => urls::verb(self.text(m)),
            "selector_expression" => self.method_constant(m).and_then(|v| urls::verb(&v)),
            _ => None,
        }
    }

    /// A URL expression as pieces.
    pub(super) fn url_pieces(&self, e: Node) -> Vec<Piece> {
        match e.kind() {
            "interpreted_string_literal" | "raw_string_literal" => {
                let t = self.text(e);
                vec![Piece::Text(t[1..t.len().saturating_sub(1)].to_string())]
            }
            "parenthesized_expression" => children(e).find(|c| c.is_named()).map(|c| self.url_pieces(c)).unwrap_or_default(),
            "binary_expression" if e.child_by_field_name("operator").is_some_and(|o| self.text(o) == "+") => {
                let mut out = e.child_by_field_name("left").map(|l| self.url_pieces(l)).unwrap_or_default();
                out.extend(e.child_by_field_name("right").map(|r| self.url_pieces(r)).unwrap_or_default());
                out
            }
            // A package constant: resolved at query time when it is a string (`const apiBase = "/api"`).
            "identifier" if self.local(self.text(e)).is_none() => vec![Piece::Ref(self.text(e).to_string())],
            "call_expression" => self.sprintf_pieces(e).unwrap_or_else(|| vec![Piece::Call]),
            _ => vec![Piece::Unknown],
        }
    }

    /// `fmt.Sprintf(format, args…)`: the format's text, each verb replaced by its argument's pieces when that is a
    /// literal or a constant, and by an unknown value otherwise.
    fn sprintf_pieces(&self, call: Node) -> Option<Vec<Piece>> {
        let f = call.child_by_field_name("function")?;
        let (o, m) = (f.child_by_field_name("operand")?, f.child_by_field_name("field")?);
        if f.kind() != "selector_expression" || self.text(m) != "Sprintf" || !self.is_import(self.text(o)) {
            return None;
        }
        let args: Vec<Node> = call.child_by_field_name("arguments").map(|a| children(a).filter(|c| c.is_named()).collect())?;
        let (format, values) = args.split_first()?;
        if !matches!(format.kind(), "interpreted_string_literal" | "raw_string_literal") {
            return None;
        }
        let t = self.text(*format);
        let mut values = values.iter();
        let mut out = Vec::new();
        let mut text = String::new();
        let mut chars = t[1..t.len().saturating_sub(1)].chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                text.push(c);
                continue;
            }
            if chars.peek() == Some(&'%') {
                chars.next();
                text.push('%');
                continue;
            }
            // Flags, width and precision, then the verb letter.
            while chars.next_if(|c| !c.is_ascii_alphabetic()).is_some() {}
            chars.next();
            out.push(Piece::Text(std::mem::take(&mut text)));
            match values.next() {
                Some(v) if matches!(v.kind(), "interpreted_string_literal" | "raw_string_literal" | "identifier") => out.extend(self.url_pieces(*v)),
                _ => out.push(Piece::Unknown),
            }
        }
        out.push(Piece::Text(text));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    fn clients(src: &str) -> Vec<(String, String)> {
        crate::go::extract("orchard/client.go", src)
            .http_routes
            .iter()
            .filter(|r| r.side == "client")
            .map(|r| (r.verb.clone(), r.path.clone()))
            .collect()
    }

    fn row(verb: &str, path: &str) -> (String, String) {
        (verb.to_string(), path.to_string())
    }

    #[test]
    fn requests_and_their_urls() {
        let got = clients(
            r#"package orchard

import (
	"context"
	"fmt"
	"net/http"
	"strings"
)

const apiBase = "/api/v1"

type Picker struct {
	base   string
	client *http.Client
}

func (p *Picker) Trees(ctx context.Context, id int) {
	http.Get(p.base + "/trees")
	http.Post(fmt.Sprintf("%s/trees/%d/prune", p.base, id), "application/json", nil)
	http.NewRequest(http.MethodDelete, apiBase+"/trees/"+fmt.Sprint(id), nil)
	http.NewRequestWithContext(ctx, "PUT", fmt.Sprintf("%s/trees/%d", apiBase, id), nil)
	client := &http.Client{}
	client.Head("http://orchard.example/health")
	http.DefaultClient.PostForm(p.base+"/login", nil)
	strings.NewReader("/not/a/request")
}

func send(method, url string) {
	http.NewRequest(method, url, nil)
	http.Get(url)
}
"#,
        );
        assert_eq!(
            got,
            vec![
                row("GET", "/trees"),
                row("POST", "/trees/{}/prune"),
                row("DELETE", "{=apiBase}/trees/{}"),
                row("PUT", "{=apiBase}/trees/{}"),
                row("HEAD", "http://orchard.example/health"),
                row("POST", "/login"),
                // A URL known only at run time: kept with no path.
                row("GET", ""),
            ]
        );
    }
}
