//! A React component rendered only through JSX has dependents: its tags are calls (kc-extract's TypeScript
//! extractor), so `impact` and `tests` reach the components and tests that render it, a function component and a
//! class component alike.

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, ImpactParams, Scope, TestsParams};

const FILES: &[(&str, &str)] = &[
    ("package.json", r#"{ "name": "shelf-app", "dependencies": { "react": "18.3.1" } }"#),
    ("src/Panel.tsx", "export function Panel({ children }: { children?: unknown }) {\n    return <section>{children}</section>;\n}\n"),
    (
        "src/Board.tsx",
        "import { Component } from 'react';\n\nexport class Board extends Component {\n    render() {\n        return <div />;\n    }\n}\n",
    ),
    (
        "src/Home.tsx",
        "import { Panel } from './Panel';\nimport { Board } from './Board';\n\nexport function Home() {\n    return (\n        <Panel>\n            <Board />\n        </Panel>\n    );\n}\n",
    ),
    ("src/App.tsx", "import { Home } from './Home';\n\nexport function App() {\n    return <Home />;\n}\n"),
    (
        "src/__tests__/layout.test.tsx",
        "import { Panel } from '../Panel';\n\nexport function rendersAnEmptyShelf() {\n    return <Panel />;\n}\n",
    ),
];

fn affected(root: &std::path::Path, target: &str) -> Vec<(String, u8)> {
    let scope = Scope { repos: vec![root.to_path_buf()], no_sync: false };
    let p = ImpactParams { symbol: Some(target.into()), depth: 2, limit: 100, ..Default::default() };
    let out = ops::impact(&p, &scope).unwrap();
    let mut got: Vec<(String, u8)> = out.data[0].impact.affected.iter().map(|a| (a.qualified.clone(), a.hop)).collect();
    got.sort();
    got
}

#[test]
fn a_component_rendered_through_jsx_has_dependents() {
    let cache = test_cache();
    let base = cache.dir("jsx-impact");
    let root = repo(&base, "shelf", FILES);
    // A function component: the component rendering it, then that one's renderer, and the test rendering it.
    let panel = affected(&root, "src/Panel.tsx:1");
    for want in [("Home", 1), ("App", 2), ("rendersAnEmptyShelf", 1)] {
        assert!(panel.iter().any(|(q, h)| q.ends_with(want.0) && *h == want.1), "{want:?} in {panel:?}");
    }
    // A class component, reached the same way.
    let board = affected(&root, "src/Board.tsx:3");
    for want in [("Home", 1), ("App", 2)] {
        assert!(board.iter().any(|(q, h)| q.ends_with(want.0) && *h == want.1), "{want:?} in {board:?}");
    }
    // `tests` finds the test that renders the component, by its reverse edge, not only its name.
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    let t = TestsParams { targets: vec!["src/Panel.tsx".into()], diff: None, limit: 10, evidence: String::new() };
    let sel = &ops::tests(&t, &scope).unwrap().data[0].selection;
    let pick = sel.selected.iter().chain(&sel.left_out).find(|p| p.file == "src/__tests__/layout.test.tsx");
    assert!(pick.is_some_and(|p| p.signals.hop == Some(1)), "{sel:?}");
    let _ = std::fs::remove_dir_all(&base);
}
