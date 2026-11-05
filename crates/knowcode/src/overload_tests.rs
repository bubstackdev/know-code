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

/// Overloads split between an interface and its base interface, implemented by one class: lines are 1-based.
const SHELF: &[(&str, &str)] = &[
    (
        "src/Shelf.cs",
        "using System.Linq.Expressions;\nnamespace Shelf;\n\
         public interface IStore<T>\n{\n\
         \x20   T Fetch(int id);\n\
         \x20   T Fetch(Expression<System.Func<T, bool>> where);\n}\n\
         public interface IBooks : IStore<Book>\n{\n\
         \x20   Book Fetch(string isbn);\n\
         \x20   Book Fetch(int id, bool track);\n\
         \x20   Book Fetch(long id, bool track);\n}\n\
         public class Books : IBooks\n{\n\
         \x20   public Book Fetch(int id) => null;\n\
         \x20   public Book Fetch(Expression<System.Func<Book, bool>> where) => null;\n\
         \x20   public Book Fetch(string isbn) => null;\n\
         \x20   public Book Fetch(int id, bool track) => null;\n\
         \x20   public Book Fetch(long id, bool track) => null;\n}\n\
         public class Book { public int Id { get; set; } }\n",
    ),
    (
        "src/Desk.cs",
        "namespace Shelf;\npublic class Desk\n{\n\
         \x20   private readonly IBooks _books;\n\
         \x20   public Desk(IBooks books) { _books = books; }\n\
         \x20   public void ById(int id) { _books.Fetch(id); }\n\
         \x20   public void ByWhere() { _books.Fetch(b => b.Id == 1); }\n\
         \x20   public void ByIsbn(string isbn) { _books.Fetch(isbn); }\n\
         \x20   public void Tracked(dynamic id) { _books.Fetch(id, true); }\n}\n",
    ),
];

#[test]
fn overloads_across_an_interface_and_its_base_are_told_apart_by_count_and_type() {
    let cache = test_cache();
    let base = cache.dir("overload-hierarchy");
    let root = repo(&base, "shelf", SHELF);
    let names = |v: Vec<(String, &'static str)>| v.into_iter().map(|(n, c)| format!("{n} {c}")).collect::<Vec<_>>();
    // `IStore<T>.Fetch(int)`: the call passing an `int`. Not `IBooks.Fetch(string)`, a same-named overload of a
    // derived interface that implements nothing, nor `Books.Fetch(string)`, which takes as many arguments of another
    // type. (`Books`, an `IStore<Book>` through `IBooks`, isn't looked for among its implementations.)
    let fetch_int = names(callers(&root, "src/Shelf.cs:5"));
    assert!(fetch_int.iter().any(|c| c.starts_with("Desk.ById ")), "{fetch_int:?}");
    assert!(!fetch_int.iter().any(|c| c.starts_with("IBooks.Fetch") || c.starts_with("Desk.ByIsbn") || c.starts_with("Desk.ByWhere")), "{fetch_int:?}");
    assert!(fetch_int.iter().filter(|c| c.starts_with("Books.Fetch")).count() <= 1, "{fetch_int:?}");
    // A lambda takes no `string`: `_books.Fetch(b => …)` calls the base interface's `Expression` overload, not the
    // derived interface's `Fetch(string)`.
    let fetch_where = names(callers(&root, "src/Shelf.cs:6"));
    assert!(fetch_where.iter().any(|c| c.starts_with("Desk.ByWhere ")), "{fetch_where:?}");
    let fetch_isbn = names(callers(&root, "src/Shelf.cs:10"));
    assert!(fetch_isbn.iter().any(|c| c.starts_with("Desk.ByIsbn ")), "{fetch_isbn:?}");
    assert!(!fetch_isbn.iter().any(|c| c.starts_with("Desk.ByWhere")), "{fetch_isbn:?}");
    // `Fetch(id, true)` with `id` of unknown type fits `(int, bool)` and `(long, bool)`: ambiguous, never exact.
    let tracked = names(callers(&root, "src/Shelf.cs:11"));
    assert!(tracked.iter().any(|c| c.starts_with("Desk.Tracked ")) && !tracked.contains(&"Desk.Tracked exact".to_string()), "{tracked:?}");
    let _ = std::fs::remove_dir_all(&base);
}
