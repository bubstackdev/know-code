//! A symbol's source as a skeleton: its signature and its members' signatures, with bodies elided, plus the lines
//! that matter for the question `context` answers: the lines making the references it makes into the indexed code
//! (calls, instantiations, table reads and writes that resolve, and member uses resolving to one member such as a
//! DbSet; framework calls such as LINQ's `Select` don't count),
//! each from the start of its statement, and its own `return`, `throw` and `yield` lines.
//!
//! A signature runs from the line naming a symbol (after the first line of each decorator or attribute) to the line
//! opening its body (`{`, `=>`, or a Python `:`); a symbol of three lines or fewer (a field, a property, a one-line
//! method) is kept whole, and members keep their doc comments (`///`, `/** */`). Spans come from the index, so no
//! file is parsed again.

use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::Connection;

use crate::query::SymbolRow;

/// How `context` shows a symbol's source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceMode {
    /// The whole span, cut at the budget.
    Full,
    /// Signatures and key lines, bodies elided.
    Skeleton,
    /// No source.
    None,
    /// A skeleton when the full span would take more than half the source budget (a fifth of the whole), else the
    /// full span.
    #[default]
    Auto,
}

impl std::str::FromStr for SourceMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "full" => Ok(SourceMode::Full),
            "skeleton" => Ok(SourceMode::Skeleton),
            "none" => Ok(SourceMode::None),
            "auto" => Ok(SourceMode::Auto),
            other => anyhow::bail!("source must be full, skeleton, none or auto, not {other}"),
        }
    }
}

/// A skeleton: the text, and the file lines it keeps as compact ranges (`10-14,20,33-35`).
#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    pub text: String,
    pub lines: String,
    /// Lines of the span left out.
    pub elided: usize,
}

/// Lines a symbol of this many lines or fewer keeps whole.
const SHORT: u32 = 3;
/// Lines a signature may run to.
const MAX_SIGNATURE: u32 = 12;

/// The lines of `hits` (a symbol's own references, resolved) that a skeleton keeps: calls, instantiations, reads and
/// writes resolving into the indexed code, and member uses resolving to one indexed member.
pub fn reference_lines(hits: &[crate::query::RefHit]) -> Vec<u32> {
    let mut lines: Vec<u32> = hits
        .iter()
        .filter(|h| match h.kind.as_str() {
            "call" | "instantiate" => matches!(h.confidence, "exact" | "unique_name" | "ambiguous"),
            // A table read or written, even one no indexed schema defines.
            "read" | "write" => true,
            // A member use resolving to one indexed member: a DbSet read (`context.Orders`), a property the code
            // depends on. Ambiguous member uses (`item.Name`) are too common to keep.
            "member" => matches!(h.confidence, "exact" | "unique_name"),
            _ => false,
        })
        .map(|h| h.line)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Lines a statement may run back to from a kept line.
const MAX_CONTINUATION: u32 = 4;

/// The first line of the statement `n` is in: back over fluent continuations (`.Where(…)`) and lines left open
/// (ending in `(`, `,`, `=`, an operator).
fn statement_start<'a>(n: u32, first: u32, line: &impl Fn(u32) -> &'a str) -> u32 {
    let mut at = n;
    while at > first && n - at < MAX_CONTINUATION {
        let here = line(at).trim_start();
        let before = line(at - 1).trim_end();
        let continues = ["." , "?.", "&&", "||", "+", "?", ":"].iter().any(|p| here.starts_with(p))
            || ["(", ",", "=", "=>", "&&", "||", "+", "?"].iter().any(|p| before.ends_with(p));
        if !continues {
            break;
        }
        at -= 1;
    }
    at
}

/// The skeleton of `sym`, whose file's text is `text` (the whole file), keeping `key_lines` (see
/// [`reference_lines`]) besides signatures and exits.
pub fn skeleton(conn: &Connection, sym: &SymbolRow, text: &str, key_lines: &[u32]) -> Result<Skeleton> {
    let lines: Vec<&str> = text.lines().collect();
    let (first, last) = (sym.line.max(1), sym.end_line.min(lines.len() as u32).max(sym.line.max(1)));
    let line = |n: u32| lines.get(n as usize - 1).copied().unwrap_or("");
    let mut keep: BTreeSet<u32> = BTreeSet::new();
    keep_signature(&mut keep, first, last, &sym.name, &line);
    keep.insert(last);

    // Members: signatures kept, bodies elided (short members kept whole).
    let mut stmt = conn.prepare_cached("SELECT start_line, end_line, name FROM symbols WHERE parent_id = ?1 ORDER BY start_line")?;
    let children: Vec<(u32, u32, String)> = stmt.query_map([sym.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    let children: Vec<(u32, u32, String)> =
        children.into_iter().filter(|(a, b, _)| *a >= first && *b <= last && (*a, *b) != (first, last)).collect();
    for (a, b, name) in &children {
        keep_signature(&mut keep, *a, *b, name, &line);
        keep.insert(*b);
        // The member's doc comment, just above it.
        let mut k = *a;
        while k > first && is_doc(line(k - 1)) {
            k -= 1;
            keep.insert(k);
        }
    }
    let in_child = |n: u32| children.iter().any(|&(a, b, _)| n > a && n < b);

    // The lines making the symbol's own references, and its exits, each from its statement's start.
    let exits = (first..=last).filter(|n| !in_child(*n) && is_exit(line(*n)));
    for n in key_lines.iter().copied().filter(|n| (first..=last).contains(n)).chain(exits) {
        keep.extend(statement_start(n, first, &line)..=n);
    }

    let mut out = String::new();
    let mut prev: Option<u32> = None;
    let mut elided = 0;
    let kept: Vec<u32> = keep.iter().copied().filter(|n| (first..=last).contains(n)).collect();
    for (i, &n) in kept.iter().enumerate() {
        if let Some(p) = prev
            && n > p + 1
        {
            elided += (n - p - 1) as usize;
            let next = line(n);
            // Only blank lines between: nothing to mark.
            if (p + 1..n).all(|k| line(k).trim().is_empty()) {
                out.push('\n');
                out.push_str(line(n).trim_end());
                prev = Some(n);
                continue;
            }
            // A body left out whole: `sig {` … `}` on one line.
            if out.ends_with('{') && next.trim() == "}" {
                out.push_str(" … }");
                prev = Some(n);
                continue;
            }
            out.push('\n');
            let indent: String = next.chars().take_while(|c| c.is_whitespace()).collect();
            let indent = if next.trim_start().starts_with('}') { format!("{indent}    ") } else { indent };
            out.push_str(&indent);
            out.push_str("…\n");
        } else if i > 0 {
            out.push('\n');
        }
        out.push_str(line(n).trim_end());
        prev = Some(n);
    }
    Ok(Skeleton { text: out.trim_end().to_string(), lines: ranges(&kept), elided })
}

/// Keeps a symbol's signature: the line declaring its name (past any decorators, whose arguments are elided)
/// through the line opening its body, or all of it when short.
fn keep_signature<'a>(keep: &mut BTreeSet<u32>, first: u32, last: u32, name: &str, line: &impl Fn(u32) -> &'a str) {
    if last - first < SHORT {
        keep.extend(first..=last);
        return;
    }
    let end = (first + MAX_SIGNATURE).min(last);
    // Decorators and attributes before the name: their first lines only.
    let named = (first..=end).find(|n| has_word(line(*n), name)).unwrap_or(first);
    for n in first..named {
        let t = line(n).trim_start();
        if t.starts_with('@') || t.starts_with('[') {
            keep.insert(n);
        }
    }
    for n in named..=end {
        keep.insert(n);
        let t = line(n).trim_end();
        if t.contains('{') || t.ends_with("=>") || t.ends_with(':') || t.ends_with(';') {
            return;
        }
    }
}

/// Whether `name` appears in `text` as a whole word.
fn has_word(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(word) && !text[i + name.len()..].chars().next().is_some_and(word)
    })
}

/// A doc comment line: `///`, or `/** … */` and its `*` lines.
fn is_doc(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with("///") || t.starts_with("/**") || t.starts_with("* ") || t == "*" || t.starts_with("*/")
}

/// A line leaving the function: `return`, `throw`, `yield`, `raise`.
fn is_exit(l: &str) -> bool {
    let t = l.trim_start();
    ["return", "throw", "yield", "raise"].iter().any(|k| t.strip_prefix(k).is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', ';', '('])))
}

/// `10-14,20,33-35`.
fn ranges(lines: &[u32]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        while j + 1 < lines.len() && lines[j + 1] == lines[j] + 1 {
            j += 1;
        }
        out.push(if i == j { lines[i].to_string() } else { format!("{}-{}", lines[i], lines[j]) });
        i = j + 1;
    }
    out.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ranges_compact() {
        assert_eq!(ranges(&[1, 2, 3, 7, 9, 10]), "1-3,7,9-10");
        assert_eq!(ranges(&[]), "");
    }

    #[test]
    fn names_are_whole_words() {
        assert!(has_word("export class AppComponent implements OnInit {", "AppComponent"));
        assert!(!has_word("export class AppComponentBase {", "AppComponent"));
    }

    #[test]
    fn doc_comment_lines() {
        assert!(is_doc("    /// Saves it."));
        assert!(is_doc("   * @param x"));
        assert!(is_doc("   */"));
        assert!(!is_doc("    x = a * b;"));
    }

    #[test]
    fn exits_are_whole_words() {
        assert!(is_exit("    return x;"));
        assert!(is_exit("throw new Error()"));
        assert!(is_exit("return;"));
        assert!(!is_exit("returnValue = 1;"));
    }
}
