//! C# overloads told apart by the number of arguments a call passes, in the form it is called: an extension method
//! called on a value takes no argument for its receiver.

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, ImpactParams, Scope};

const FILES: &[(&str, &str)] = &[(
    "src/Shelving.cs",
    "namespace Shelving;\npublic class Rack { }\npublic static class RackExtensions\n{\n\
     \x20   public static Rack Stack<TItem, TBox>(this Rack rack) => rack;\n\
     \x20   public static Rack Stack<TItem>(this Rack rack, System.Func<TItem, TItem> pick) => rack;\n}\n\
     public class Loader\n{\n\
     \x20   public void Load(Rack rack) { rack.Stack<int>(x => x); }\n\
     \x20   public void LoadTwo(Rack rack) { rack.Stack<int, string>(); }\n\
     \x20   public void LoadStatic(Rack rack) { RackExtensions.Stack<int, string>(rack); }\n}\n",
)];

fn callers(root: &std::path::Path, at: &str) -> Vec<(String, &'static str)> {
    let scope = Scope { repos: vec![root.to_path_buf()], no_sync: false };
    let p = ImpactParams { symbol: Some(at.into()), depth: 1, limit: 100, ..Default::default() };
    let out = ops::impact(&p, &scope).unwrap();
    let mut got: Vec<(String, &'static str)> = out.data[0].impact.affected.iter().map(|a| (a.qualified.clone(), a.confidence)).collect();
    got.sort();
    got
}

#[test]
fn an_extension_overload_taking_fewer_arguments_than_a_call_on_a_value_passes_is_not_its_caller() {
    let cache = test_cache();
    let base = cache.dir("overload-arity");
    let root = repo(&base, "shelving", FILES);
    // `rack.Stack<int>(x => x)` passes one argument besides its receiver: only the one-parameter overload takes it.
    // `RackExtensions.Stack<int, string>(rack)` passes the receiver as its argument: the parameterless one's.
    assert_eq!(
        callers(&root, "src/Shelving.cs:5"),
        [("Loader.LoadStatic".to_string(), "exact"), ("Loader.LoadTwo".to_string(), "exact")],
        "the first overload's callers"
    );
    assert_eq!(callers(&root, "src/Shelving.cs:6"), [("Loader.Load".to_string(), "exact")], "the second overload's callers");
    let _ = std::fs::remove_dir_all(&base);
}
