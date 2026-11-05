//! `kc_more` in the format of the answer it continues. A text answer (an impact or trace tree) names its cursors in
//! its hints (see [`crate::compact::text_hints`]); the pages after it are text too, each row as the answer's own
//! tree gives it, so a reader of the first page reads the rest the same way. A cursor a JSON answer handed out
//! still gives JSON pages.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::Value;

/// Cursors handed out in text answers, the most recent last: as many as the server keeps handles for.
const KEPT: usize = 4096;

static TEXT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// What a page's text leaves room for besides its rows: the header and the hints `run_text` adds.
const HEADER_ROOM: usize = 600;

/// Notes that `cursor` (a full cursor) was handed out in a text answer.
pub fn mark(cursor: &str) {
    let mut t = TEXT.lock().unwrap_or_else(|e| e.into_inner());
    if !t.iter().any(|c| c == cursor) {
        t.push_back(cursor.to_string());
        if t.len() > KEPT {
            t.pop_front();
        }
    }
}

/// Whether the full cursor `cursor` was handed out in a text answer.
pub fn continues_text(cursor: &str) -> bool {
    TEXT.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|c| c == cursor)
}

/// The page the full cursor `cursor` continues to, as an envelope and its text, cut to fit `max` characters: rows
/// that don't fit are left for the next page, whose cursor starts at the first of them, so none is skipped.
pub fn page(cursor: &str, max: usize) -> anyhow::Result<(Value, String)> {
    let mut env = crate::cursor::more(cursor)?;
    let mut resume = crate::cursor::take(&mut env);
    loop {
        let text = crate::render::page_text(&env);
        let rows = env["items"].as_array().map_or(0, Vec::len);
        if text.len() + HEADER_ROOM <= max || rows <= 1 {
            return Ok((env, text));
        }
        if let Some(items) = env["items"].as_array_mut() {
            items.pop();
        }
        match resume.as_mut() {
            Some(r) => {
                crate::cursor::cut(&mut env, r, "/items");
            }
            None => env["truncated"] = Value::Bool(true),
        }
        crate::cursor::seal(&mut env);
    }
}

#[cfg(test)]
mod tests {
    use crate::ops::tests::{repo, test_cache};
    use crate::ops::{self, ImpactParams, Scope};
    use serde_json::Value;

    /// The next page's cursor a text page names, as its hint does once `run_text` has made it a handle.
    fn next(env: &Value) -> Option<String> {
        let mut env = env.clone();
        crate::contract::finish(&mut env);
        crate::compact::text_hints(&mut env);
        let hints: Vec<&str> = env["incomplete"].as_array()?.iter().filter_map(|i| i["hint"].as_str()).collect();
        let handle = hints.iter().find_map(|h| h.split("kc_more {cursor: \"").nth(1)?.split('"').next())?;
        Some(crate::compact::resolve(handle).unwrap())
    }

    /// A text impact cut at its limit continues as text, each row as the tree gives it, until every row is listed
    /// once: pages cut to fit the size cap leave their last rows to the next page, never skip them.
    #[test]
    fn a_text_answers_pages_are_text_and_list_every_row_once() {
        let cache = test_cache();
        let base = cache.dir("more-text");
        let mut files = vec![("lib.py".to_string(), "def target():\n    pass\n".to_string())];
        for i in 0..40 {
            files.push((format!("callers/c{i:02}.py"), format!("from lib import target\ndef caller_with_a_long_name_{i:02}():\n    target()\n")));
        }
        let all: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let root = repo(&base, "app", &all);
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let p = ImpactParams { symbol: Some("target".into()), depth: 1, limit: 5, ..Default::default() };
        let out = ops::impact(&p, &scope).unwrap();
        let first: Vec<String> = out.data[0].impact.affected.iter().map(|a| a.qualified.clone()).collect();
        let mut env = out.env.clone();
        crate::contract::finish(&mut env);
        let mut cursor = next(&env).expect("the first page names its continuation");
        assert!(super::continues_text(&cursor), "a cursor a text answer hands out continues as text");
        let mut listed = first;
        let mut pages = 0;
        loop {
            // Small enough that a page of the 35 rows left is cut to fit.
            let (env, text) = super::page(&cursor, 1_500).unwrap();
            pages += 1;
            assert!(text.contains("called by function caller_with_a_long_name_"), "{text}");
            listed.extend(env["items"].as_array().unwrap().iter().map(|r| r["qualified"].as_str().unwrap().to_string()));
            match next(&env) {
                Some(c) => {
                    assert!(super::continues_text(&c), "the next page stays text");
                    cursor = c;
                }
                None => break,
            }
            assert!(pages < 20, "pages end");
        }
        assert!(pages > 1, "the size cap cut a page");
        let mut sorted = listed.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 40, "{listed:?}");
        assert_eq!(listed.len(), 40, "no row listed twice: {listed:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
