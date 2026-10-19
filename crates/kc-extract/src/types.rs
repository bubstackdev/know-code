//! Syntactic type expressions: what the extractor can say about a value's type from one file, to be finished at
//! query time against the whole index.
//!
//! `let e = makeEngine()` gives `Call { name: "makeEngine" }`, resolved later through the function's declared return
//! type. `game.factory.tiles[c]` gives a `Path` from the caller's `game` property through `factory` and `tiles` to an
//! element. Stored encoded: a plain nominal type as its bare name, anything else as JSON.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeExpr {
    /// A nominal type: `Factory`, `String`.
    Named(String),
    /// `[T]`, `Array<T>`, `Set<T>` and other sequences of T.
    Array(Box<TypeExpr>),
    /// `[K: V]`.
    Dict(Box<TypeExpr>, Box<TypeExpr>),
    /// The declared return type of `name` (a free function or a member of the caller's type), or of `Owner.name`.
    Call { owner: Option<String>, name: String },
    /// The element type of a sequence (a `for` loop variable, a closure's `$0`).
    Elem(Box<TypeExpr>),
    /// A member chain from a root.
    Path { root: PathRoot, segs: Vec<Seg> },
    /// Parameter `param` of a lambda passed as argument `arg` (of `args`) to method `name`, called on a value of type
    /// `recv`, or with no receiver on the caller's type: an input type of the delegate that method's parameter takes.
    LambdaParam { recv: Option<Box<TypeExpr>>, name: String, args: u32, arg: u32, param: u32 },
    /// A delegate (`Func<…, T>`): calling it, or its `Invoke`, gives a `T`.
    Delegate(Box<TypeExpr>),
    /// A generic type with its type arguments (`PagedList<Order>`, `Lazy<IFoo>`), in order, `None` where not known.
    /// A member of the repo's generic type has its type parameters replaced by the arguments; a library's
    /// (`Lazy<T>.Value`, `IOptions<T>.Value`, `ConsumeContext<T>.Message`) hands out its one argument through the
    /// usual accessors.
    Generic { name: String, args: Vec<Option<TypeExpr>> },
    /// A tuple (`(string Number, decimal Amount)`): its elements' names (`Item1`, … when unnamed) and types, when known.
    /// Its members are the elements, never a type's of the same name.
    Tuple(Vec<(String, Option<TypeExpr>)>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathRoot {
    /// `self`, or an unqualified member of the caller's type.
    SelfType,
    /// An identifier that isn't a known local: a property of the caller's type (or an enclosing type).
    Prop(String),
    /// `Type.member`: static access.
    Static(String),
    /// Any other typed expression.
    Expr(Box<TypeExpr>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seg {
    /// `.name` property access.
    Member(String),
    /// `.name(...)` method call.
    Call(String),
    /// `[...]` subscript of the current value.
    Index,
}

/// What resolution needs to know about a C# type declaration, kept as its symbol's `type_name`: its namespace, its
/// type parameters, the type arguments it gives its generic bases (`class Repo : BaseRepo<ShopContext>`), and the
/// file's `using` aliases (which the index's imports don't tell from namespaces imported).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TypeDecl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ns: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<String>,
    /// Generic bases: the base's name and its arguments, encoded type expressions (`None` when not known).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<(String, Vec<Option<String>>)>,
    /// `using Alias = Target;` in the file, as (alias, target).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<(String, String)>,
}

impl TypeDecl {
    /// `None` when there is nothing to say.
    pub fn encode(&self) -> Option<String> {
        (*self != TypeDecl::default()).then(|| serde_json::to_string(self).expect("TypeDecl serializes"))
    }

    /// The declaration info in a type symbol's `type_name`; empty for anything else.
    pub fn decode(s: Option<&str>) -> TypeDecl {
        s.filter(|s| ["{\"ns\"", "{\"params\"", "{\"bases\"", "{\"aliases\""].iter().any(|k| s.starts_with(k)))
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    }
}

impl TypeExpr {
    pub fn encode(&self) -> String {
        match self {
            TypeExpr::Named(n) => n.clone(),
            other => serde_json::to_string(other).expect("TypeExpr serializes"),
        }
    }

    pub fn decode(s: &str) -> Option<TypeExpr> {
        if s.starts_with('{') || s.starts_with('"') {
            serde_json::from_str(s).ok()
        } else if s.is_empty() {
            None
        } else {
            Some(TypeExpr::Named(s.to_string()))
        }
    }

    /// Appends a segment, turning this expression into (or extending) a path.
    pub fn then(self, seg: Seg) -> TypeExpr {
        match self {
            TypeExpr::Path { root, mut segs } => {
                segs.push(seg);
                TypeExpr::Path { root, segs }
            }
            other => TypeExpr::Path { root: PathRoot::Expr(Box::new(other)), segs: vec![seg] },
        }
    }

    pub fn root(root: PathRoot) -> TypeExpr {
        TypeExpr::Path { root, segs: Vec::new() }
    }

    /// This expression with every type name and static root (`Name`, `A.B.Name`) passed through `f`.
    pub fn map_names(&self, f: &impl Fn(&str) -> String) -> TypeExpr {
        let m = |t: &TypeExpr| Box::new(t.map_names(f));
        match self {
            TypeExpr::Named(n) => TypeExpr::Named(f(n)),
            TypeExpr::Array(e) => TypeExpr::Array(m(e)),
            TypeExpr::Dict(k, v) => TypeExpr::Dict(m(k), m(v)),
            TypeExpr::Elem(e) => TypeExpr::Elem(m(e)),
            TypeExpr::Delegate(e) => TypeExpr::Delegate(m(e)),
            TypeExpr::Generic { name, args } => TypeExpr::Generic {
                name: f(name),
                args: args.iter().map(|a| a.as_ref().map(|a| a.map_names(f))).collect(),
            },
            TypeExpr::Tuple(es) => TypeExpr::Tuple(es.iter().map(|(n, e)| (n.clone(), e.as_ref().map(|e| e.map_names(f)))).collect()),
            TypeExpr::Path { root, segs } => {
                let root = match root {
                    PathRoot::Static(n) => PathRoot::Static(f(n)),
                    PathRoot::Expr(e) => PathRoot::Expr(m(e)),
                    other => other.clone(),
                };
                TypeExpr::Path { root, segs: segs.clone() }
            }
            TypeExpr::LambdaParam { recv, name, args, arg, param } => TypeExpr::LambdaParam {
                recv: recv.as_ref().map(|r| m(r)),
                name: name.clone(),
                args: *args,
                arg: *arg,
                param: *param,
            },
            TypeExpr::Call { .. } => self.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let named = TypeExpr::Named("Factory".into());
        assert_eq!(named.encode(), "Factory");
        let path = TypeExpr::root(PathRoot::Prop("game".into())).then(Seg::Member("factory".into())).then(Seg::Index);
        let enc = path.encode();
        assert!(enc.starts_with('{'));
        assert_eq!(TypeExpr::decode(&enc), Some(path));
        assert_eq!(TypeExpr::decode("Factory"), Some(named));
    }
}
