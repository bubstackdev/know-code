//! react-query's `data`: what the query function returns (TanStack's `TData`). A fetcher returning a package's value
//! makes `data` the package's, so its members aren't the repo's same-named ones; a fetcher casting to the repo's own
//! model keeps them linked, though the component reading `data` never imports that model.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn index(root: &Path) -> Store {
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    store
}

/// `refs NAME` from `file`: each reference's line, confidence and targets.
fn refs_in(store: &Store, name: &str, file: &str) -> Vec<(u32, String, Vec<String>)> {
    let (hits, _) = query::refs(store.conn(), name, &["member"], 50).unwrap();
    hits.iter()
        .filter(|h| h.file == file)
        .map(|h| (h.line, h.confidence.to_string(), h.targets.iter().map(|t| t.qualified.clone()).collect()))
        .collect()
}

#[test]
fn query_data_is_what_the_query_function_returns() {
    let root = scratch("ts-query-data");
    write(&root, "package.json", r#"{ "dependencies": { "@tanstack/react-query": "5.0.0", "@acme/sdk": "1.0.0", "react": "19.0.0" } }"#);
    write(&root, "tsconfig.json", r#"{ "compilerOptions": { "baseUrl": "src" } }"#);
    write(&root, "src/types/shelf-page.ts", "export interface ShelfPage {\n    Total: number;\n}\n");
    write(&root, "src/types/library.ts", "export interface Library {\n    Ratings: string[];\n    Shelves: string[];\n}\n");
    write(
        &root,
        "src/hooks/useShelves.ts",
        "import { useQuery } from '@tanstack/react-query';
import { getShelfApi } from '@acme/sdk/shelf-api';
import type { ShelfPage } from 'types/shelf-page';

const fetchFilters = async (api: Api) => {
    const response = await getShelfApi(api).getFilters();
    return response.data;
};

const fetchPage = async (api: Api) => {
    const response = await getShelfApi(api).getPage();
    return response.data as ShelfPage;
};

export const useFilters = (api: Api) => {
    return useQuery({
        queryKey: ['filters'],
        queryFn: () => fetchFilters(api)
    });
};

export const usePage = (api: Api) => useQuery({ queryKey: ['page'], queryFn: () => fetchPage(api) });
",
    );
    write(
        &root,
        "src/views/Shelves.tsx",
        "import { useMemo } from 'react';
import { useFilters, usePage } from 'hooks/useShelves';
import type { Library } from 'types/library';

export function Shelves({ api }: Props) {
    const { data: filters } = useFilters(api);
    const { data: page } = usePage(api);
    const library = useMemo<Library>(() => load(), []);
    return [filters?.Ratings, page?.Total, library.Shelves];
}
",
    );
    let store = index(&root);
    let file = "src/views/Shelves.tsx";
    // The package's filters: not the repo's `Library.Ratings`.
    assert_eq!(refs_in(&store, "Ratings", file), vec![(9, "unresolved".to_string(), vec![])]);
    // The repo's model, cast to in another file.
    assert_eq!(refs_in(&store, "Total", file), vec![(9, "exact".to_string(), vec!["ShelfPage.Total".to_string()])]);
    // A package's generic function given the repo's type hands it back: still linked.
    let shelves = refs_in(&store, "Shelves", file);
    assert!(shelves.iter().any(|(l, _, t)| *l == 9 && t == &vec!["Library.Shelves".to_string()]), "{shelves:?}");
    let _ = std::fs::remove_dir_all(&root);
}
