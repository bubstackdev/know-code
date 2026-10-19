//! Angular templates (`.html`): the custom elements and directive attributes they use, scanned without a parser.

use crate::{FileFacts, NgFact};

pub const VERSION: u32 = 3;

/// HTML and SVG attributes, which never select a directive of the code base, matched in lower case.
const STANDARD_ATTRS: &[&str] = &[
    "accept", "action", "alt", "autocomplete", "autofocus", "charset", "checked", "class", "clip-rule", "cols",
    "colspan", "content", "cx", "cy", "d", "dir", "disabled", "download", "fill", "fill-rule", "for", "form", "height",
    "hidden", "href", "id", "innerhtml", "innertext", "label", "lang", "list", "loading", "max", "maxlength", "method",
    "min", "minlength", "multiple", "name", "novalidate", "opacity", "pattern", "placeholder", "points", "r", "readonly",
    "rel", "required", "role", "rows", "rowspan", "rx", "ry", "scope", "selected", "size", "src", "srcset", "step",
    "stroke", "stroke-linecap", "stroke-linejoin", "stroke-width", "style", "tabindex", "target", "textcontent",
    "title", "transform", "type", "value", "viewbox", "width", "wrap", "x", "x1", "x2", "xmlns", "y", "y1", "y2",
];

/// Angular's own directives (common, forms, router), which no indexed repo declares.
const ANGULAR_ATTRS: &[&str] = &[
    "formArrayName", "formControl", "formControlName", "formGroup", "formGroupName", "ngClass", "ngFor", "ngForOf",
    "ngForTrackBy", "ngIf", "ngIfElse", "ngModel", "ngModelOptions", "ngNonBindable", "ngPlural", "ngPluralCase",
    "ngProjectAs", "ngSrc", "ngStyle", "ngSwitch", "ngSwitchCase", "ngSwitchDefault", "ngTemplateOutlet",
    "ngTemplateOutletContext", "queryParams", "routerLink", "routerLinkActive", "routerLinkActiveOptions",
];

/// An opening tag: its name (lower case), the attribute names a selector can match (`[nbBadge]="x"` and
/// `*uiIf="y"` give `nbBadge` and `uiIf`; event bindings, template references and `[attr.x]`-style bindings are
/// left out), each with its line, and the tag's line.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, u32)>,
    pub line: u32,
}

pub fn extract(source: &str) -> FileFacts {
    FileFacts { ng: template_facts(source, None, 0), ..Default::default() }
}

/// The `renders` facts of a template: one per custom element (`value` the tag), and one per attribute that may select
/// a directive (`value` the attribute, as written). `extra` is the element's tag and attributes, space-separated,
/// for selectors that name both (`ui-paginator[withCursors]`). `line_base` is added to every line.
pub fn template_facts(source: &str, sym: Option<usize>, line_base: u32) -> Vec<NgFact> {
    let mut out = Vec::new();
    for e in elements(source) {
        let extra = std::iter::once(e.tag.as_str()).chain(e.attrs.iter().map(|(a, _)| a.as_str())).collect::<Vec<_>>().join(" ");
        if is_custom(&e.tag) {
            out.push(NgFact { kind: "renders", sym, value: e.tag.clone(), extra: Some(extra.clone()), line: line_base + e.line });
        }
        for (a, l) in e.attrs.iter().filter(|(a, _)| may_select(a)) {
            out.push(NgFact { kind: "attr", sym, value: a.clone(), extra: Some(extra.clone()), line: line_base + l });
        }
    }
    out
}

/// A custom element: a name with a `-`, other than Angular's `ng-template`, `ng-container` and `ng-content`.
pub fn is_custom(tag: &str) -> bool {
    tag.contains('-') && !matches!(tag, "ng-template" | "ng-container" | "ng-content")
}

fn may_select(attr: &str) -> bool {
    let lower = attr.to_ascii_lowercase();
    !STANDARD_ATTRS.contains(&lower.as_str())
        && !lower.starts_with("aria-")
        && !lower.starts_with("data-")
        && !ANGULAR_ATTRS.contains(&attr)
}

/// Opening tags custom elements or not, with 1-based lines, skipping comments.
pub fn elements(src: &str) -> Vec<Element> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1u32);
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => line += 1,
            b'<' if src[i..].starts_with("<!--") => {
                let end = src[i..].find("-->").map_or(bytes.len(), |e| i + e + 3);
                line += src[i..end].matches('\n').count() as u32;
                i = end;
                continue;
            }
            b'<' => {
                let name: String =
                    src[i + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
                if name.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    let tag_line = line;
                    let (attrs, end, end_line) = attributes(src, i + 1 + name.len(), line);
                    out.push(Element { tag: name.to_ascii_lowercase(), attrs, line: tag_line });
                    i = end;
                    line = end_line;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The attributes of a tag from `i` (just after its name) to its `>`: names a selector can match, the byte offset
/// after the tag, and the line there.
fn attributes(src: &str, mut i: usize, mut line: u32) -> (Vec<(String, u32)>, usize, u32) {
    let b = src.as_bytes();
    let mut out = Vec::new();
    while i < b.len() {
        match b[i] {
            b'>' => return (out, i + 1, line),
            b'\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_ascii_whitespace() || c == b'/' => i += 1,
            // A stray `<` (text like `a < b`, read as a tag): stop here and let the scan see it.
            b'<' => return (out, i, line),
            _ => {
                let start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'<') {
                    i += 1;
                }
                let name_line = line;
                if let Some(n) = attr_name(&src[start..i]) {
                    out.push((n, name_line));
                }
                let mut j = i;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < b.len() && b[j] == b'=' {
                    line += src[i..j].matches('\n').count() as u32;
                    j += 1;
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        line += (b[j] == b'\n') as u32;
                        j += 1;
                    }
                    if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                        let q = b[j];
                        let end = src[j + 1..].find(q as char).map_or(b.len(), |e| j + 1 + e + 1);
                        line += src[j..end].matches('\n').count() as u32;
                        j = end;
                    } else {
                        while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                            j += 1;
                        }
                    }
                    i = j;
                }
            }
        }
    }
    (out, i, line)
}

/// The name a selector matches for an attribute as written: `[x]`, `[(x)]`, `*x`, `bind-x`, `bindon-x` and `x` give
/// `x`; outputs (`(x)`, `on-x`), references (`#x`, `ref-x`), `let-x`, animations, i18n markers and dotted bindings
/// (`[attr.x]`, `[class.x]`) give none.
fn attr_name(raw: &str) -> Option<String> {
    if raw.starts_with(['(', '#', '@']) || raw.starts_with("[@") || raw.starts_with("on-") || raw.starts_with("ref-") {
        return None;
    }
    if raw.starts_with("let-") || raw.starts_with("i18n") {
        return None;
    }
    let n = raw.trim_start_matches("[(").trim_end_matches(")]").trim_start_matches('[').trim_end_matches(']');
    let n = n.trim_start_matches('*');
    let n = n.strip_prefix("bindon-").or_else(|| n.strip_prefix("bind-")).unwrap_or(n);
    let ok = !n.is_empty()
        && n.starts_with(|c: char| c.is_ascii_alphabetic())
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_elements_with_lines() {
        let html = "<div class=\"x\">\n  <nb-mx-records [records]=\"r\"></nb-mx-records>\n  <!-- <nb-old></nb-old>\n  -->\n  <kendo-grid></kendo-grid><span></span>\n<ng-template><ng-container></ng-container></ng-template>\n</div>\n";
        let custom: Vec<(String, u32)> = elements(html).into_iter().filter(|e| is_custom(&e.tag)).map(|e| (e.tag, e.line)).collect();
        assert_eq!(custom, [("nb-mx-records".to_string(), 2), ("kendo-grid".to_string(), 5)]);
    }

    #[test]
    fn attributes_that_may_select_directives() {
        let html = "<button uiButton class=\"a\" (click)=\"go()\" #b\n  [nbBadge]=\"n > 1\" *uiIf=\"x\" [attr.aria-label]=\"l\"\n  [(ngModel)]=\"m\" aria-hidden=\"true\">Go</button>\n<ui-paginator [withCursors]='true' [ngClass]=\"c\" />";
        let els = elements(html);
        assert_eq!(els[0].tag, "button");
        let names: Vec<(&str, u32)> = els[0].attrs.iter().map(|(a, l)| (a.as_str(), *l)).collect();
        assert_eq!(names, [("uiButton", 1), ("class", 1), ("nbBadge", 2), ("uiIf", 2), ("ngModel", 3), ("aria-hidden", 3)]);
        assert_eq!(els[1].tag, "ui-paginator");
        assert_eq!(els[1].line, 4);

        let facts = template_facts(html, None, 10);
        let got: Vec<(&str, &str, Option<&str>, u32)> = facts.iter().map(|f| (f.kind, f.value.as_str(), f.extra.as_deref(), f.line)).collect();
        let button = "button uiButton class nbBadge uiIf ngModel aria-hidden";
        let pager = "ui-paginator withCursors ngClass";
        assert_eq!(
            got,
            [
                ("attr", "uiButton", Some(button), 11),
                ("attr", "nbBadge", Some(button), 12),
                ("attr", "uiIf", Some(button), 12),
                ("renders", "ui-paginator", Some(pager), 14),
                ("attr", "withCursors", Some(pager), 14),
            ]
        );
    }
}
