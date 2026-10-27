//! Angular across repos: elements and directive attributes a template uses that a library repo declares, and a
//! library component's or directive's uses in the apps that depend on it. Both follow manifest edges (an app's
//! `package.json` dependency on the library).

use std::path::Path;

use anyhow::Result;
use kc_store::query::{self, Angular};

use crate::workspace::{RepoSummary, neighbours};

fn name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Fills in what `query::angular` can't see from one repo: elements and attributes declared by the repos `root`
/// depends on, and uses of the component's selectors in the repos depending on it. Attributes that resolve nowhere
/// are plain inputs or third-party directives, and are left out.
pub fn complete(a: &mut Angular, root: &Path, summaries: &[RepoSummary]) -> Result<()> {
    let (dependencies, dependents) = neighbours(root, summaries);
    // A repo whose checkout is gone declares nothing and uses nothing; where it would have, it is missing.
    let (gone_deps, dependencies): (Vec<&RepoSummary>, Vec<&RepoSummary>) = dependencies.into_iter().partition(|d| d.gone);
    let (gone_dependents, dependents): (Vec<&RepoSummary>, Vec<&RepoSummary>) = dependents.into_iter().partition(|d| d.gone);
    let unresolved: Vec<&query::NgUse> = a.renders.iter().filter(|u| u.component.is_none()).chain(&a.unresolved_attrs).collect();
    crate::workspace::note_missing_if(gone_deps, |d| d.selectors.iter().any(|s| unresolved.iter().any(|u| u.matched_by(&s.part))));
    let resolve = |u: &mut query::NgUse| {
        if let Some((repo, sel)) = dependencies.iter().find_map(|d| d.selectors.iter().find(|s| u.matched_by(&s.part)).map(|s| (d, s))) {
            u.component = Some(sel.component.clone());
            u.repo = Some(name(&repo.root));
        }
    };
    for u in a.renders.iter_mut().filter(|u| u.component.is_none()) {
        resolve(u);
    }
    for mut u in std::mem::take(&mut a.unresolved_attrs) {
        resolve(&mut u);
        if u.component.is_some() {
            a.renders.push(u);
        }
    }
    a.renders.sort_by(|x, y| (&x.file, x.line).cmp(&(&y.file, y.line)));
    let parts = query::selector_parts(a.selector.as_deref().unwrap_or(""));
    if !parts.is_empty() {
        gone_dependents.iter().for_each(|d| crate::workspace::note_missing(&d.root));
        for d in dependents {
            let store = crate::open(&d.root)?;
            for mut u in query::template_uses(store.conn(), &parts)? {
                u.repo = Some(name(&d.root));
                a.rendered_by.push(u);
            }
        }
    }
    Ok(())
}
