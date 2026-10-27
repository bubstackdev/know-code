//! Conventions recognised by name: the attributes, base classes, calls and decorators a code base uses for its
//! message bus, its service gateway and its request contracts, the lifetimes its DI scope values stand for, and the
//! names CI and Ansible linking compare. Libraries KnowCode knows (MassTransit, NServiceBus, ASP.NET, Autofac) are
//! built in; a code base's own conventions are configured (`[conventions.*]` in `config.toml`, read by
//! `kc_index::config`), so a team's in-house names reach the extractors without being written into KnowCode.
//!
//! The extractors read [`current`]: what the config last set (see [`set`]), else the defaults. What a language's
//! extraction depends on is part of its extractor version (see [`stamp`]), so a change to it re-extracts that
//! language's files and nothing else.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock, RwLock};

/// The role of a call's or attribute's argument, by position (see [`Call`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arg {
    /// The message put on the bus.
    Message,
    /// The bus destination (a virtual host): a constant's member name (`Hosts.Orders` → `Orders`) or a literal.
    Host,
    /// A queue on that host: a literal.
    Queue,
    /// The service a request goes to: a literal, `nameof`, or a constant.
    Service,
    /// The request's path.
    Path,
    /// The request's HTTP verb.
    Verb,
    /// Any other argument (`_`).
    Other,
}

impl Arg {
    fn parse(s: &str) -> Option<Arg> {
        Some(match s {
            "message" => Arg::Message,
            "host" => Arg::Host,
            "queue" => Arg::Queue,
            "service" => Arg::Service,
            "path" => Arg::Path,
            "verb" => Arg::Verb,
            "_" => Arg::Other,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Arg::Message => "message",
            Arg::Host => "host",
            Arg::Queue => "queue",
            Arg::Service => "service",
            Arg::Path => "path",
            Arg::Verb => "verb",
            Arg::Other => "_",
        }
    }
}

/// A call (or attribute) and what its arguments are, written `Name(role, role, …)`: `Publish(message)`,
/// `SendToQueue(message, host, queue)`, `CallService(verb, service, path)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Call {
    pub name: String,
    pub args: Vec<Arg>,
}

impl Call {
    /// Parses `Name(role, …)`, requiring each of `required` once and allowing only `allowed` (and `_`).
    pub fn parse(s: &str, required: &[Arg], allowed: &[Arg]) -> Result<Call, String> {
        let s = s.trim();
        let (name, rest) = s.split_once('(').ok_or_else(|| format!("`{s}`: expected `Name({})`", names(required)))?;
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("`{s}`: the name must be an identifier"));
        }
        let inner = rest.strip_suffix(')').ok_or_else(|| format!("`{s}`: missing `)`"))?;
        let mut args = Vec::new();
        for a in inner.split(',').map(str::trim).filter(|a| !a.is_empty()) {
            let arg = Arg::parse(a).filter(|r| *r == Arg::Other || allowed.contains(r) || required.contains(r));
            args.push(arg.ok_or_else(|| format!("`{s}`: `{a}` is not one of {}, _", names(&[required, allowed].concat())))?);
        }
        for r in required {
            if args.iter().filter(|a| *a == r).count() != 1 {
                return Err(format!("`{s}`: needs `{}` exactly once", r.as_str()));
            }
        }
        for r in allowed {
            if args.iter().filter(|a| *a == r).count() > 1 {
                return Err(format!("`{s}`: `{}` more than once", r.as_str()));
            }
        }
        Ok(Call { name: name.to_string(), args })
    }

    /// The position of `role` among the arguments.
    pub fn position(&self, role: Arg) -> Option<usize> {
        self.args.iter().position(|a| *a == role)
    }

    /// As written in the config.
    pub fn render(&self) -> String {
        format!("{}({})", self.name, self.args.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", "))
    }
}

fn names(roles: &[Arg]) -> String {
    roles.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(", ")
}

/// The roles a bus call takes (`message` required).
pub const BUS_ROLES: &[Arg] = &[Arg::Host, Arg::Queue];
/// The roles a request call or attribute takes (all required).
pub const REQUEST_ROLES: &[Arg] = &[Arg::Service, Arg::Path, Arg::Verb];

/// The lifetimes a DI scope value can stand for.
pub const LIFETIMES: &[&str] = &["singleton", "scoped", "transient", "unit_of_work"];

/// C# message bus (`[conventions.csharp.bus]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Bus {
    /// Attributes marking a class as a message type; the first argument, when there is one, is its destination.
    pub message_attributes: Vec<String>,
    /// Generic base types whose first type argument is the message a consumer handles.
    pub consumer_bases: Vec<String>,
    /// Registration calls (`Add…Consumer<T>()`) whose one type argument is a consumer class, not a message.
    pub consumer_registrations: Vec<String>,
    /// Calls putting a message on the bus, when the receiver is a bus, publisher, endpoint or consume context.
    pub publish: Vec<Call>,
}

/// C# requests between services (`[conventions.csharp.http]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CsHttp {
    /// Attributes marking a class as a request sent through a service gateway: `X(service, path, verb)`.
    pub request_attributes: Vec<Call>,
    /// Gateway calls naming the service, path and verb of a request: `X(service, path, verb)`.
    pub gateway_calls: Vec<Call>,
    /// Base classes of a service host (matched as a prefix of the base's name), whose first constructor argument
    /// names the service the repo hosts.
    pub service_host_bases: Vec<String>,
}

/// C# dependency injection (`[conventions.csharp.di]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Di {
    /// `Scoped(Scope.X)` values beyond the built-in ones, with the lifetime each stands for (one of [`LIFETIMES`]).
    pub scope_lifetimes: Vec<(String, &'static str)>,
}

/// TypeScript request contracts (`[conventions.typescript.http]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TsHttp {
    /// Class decorators taking an object with the request's path and verb: the class is a request the app sends.
    pub request_decorators: Vec<String>,
    /// The object's keys holding the path.
    pub path_keys: Vec<String>,
    /// The object's keys holding the verb.
    pub verb_keys: Vec<String>,
}

/// CI (`[conventions.ci]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ci {
    /// Pipeline variables naming the application a deploy installs, first set wins.
    pub app_name_variables: Vec<String>,
}

/// Ansible (`[conventions.ansible]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Ansible {
    /// Infrastructure names, beyond the built-in ones, that a service or image never links to an application repo by.
    pub infra_names: Vec<String>,
    /// Prefixes dropped when names are compared (lower case, letters and digits only).
    pub name_prefixes: Vec<String>,
}

/// Every configurable convention.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Conventions {
    pub bus: Bus,
    pub csharp_http: CsHttp,
    pub di: Di,
    pub ts_http: TsHttp,
    pub ci: Ci,
    pub ansible: Ansible,
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

impl Default for Bus {
    fn default() -> Bus {
        Bus {
            message_attributes: Vec::new(),
            // MassTransit's consumer and NServiceBus's handler.
            consumer_bases: strings(&["IConsumer", "IHandleMessages"]),
            consumer_registrations: strings(&["AddConsumer"]),
            publish: vec![
                Call { name: "Publish".into(), args: vec![Arg::Message] },
                Call { name: "Send".into(), args: vec![Arg::Message] },
            ],
        }
    }
}

impl Default for TsHttp {
    fn default() -> TsHttp {
        TsHttp { request_decorators: Vec::new(), path_keys: strings(&["endpoint", "path", "url"]), verb_keys: strings(&["httpMethod", "method", "verb"]) }
    }
}

impl Default for Ci {
    fn default() -> Ci {
        Ci { app_name_variables: strings(&["PACKAGE_NAME", "APP_NAME"]) }
    }
}

impl Bus {
    /// The publish call named `method`.
    pub fn publish_call(&self, method: &str) -> Option<&Call> {
        self.publish.iter().find(|c| c.name == method)
    }
}

static CURRENT: RwLock<Option<Arc<Conventions>>> = RwLock::new(None);

thread_local! {
    static OVERRIDE: RefCell<Option<Arc<Conventions>>> = const { RefCell::new(None) };
}

fn defaults() -> Arc<Conventions> {
    static DEFAULT: OnceLock<Arc<Conventions>> = OnceLock::new();
    DEFAULT.get_or_init(|| Arc::new(Conventions::default())).clone()
}

/// The conventions in effect: this thread's (see [`scoped`]), else the configured ones, else the defaults.
pub fn current() -> Arc<Conventions> {
    if let Some(c) = OVERRIDE.with(|o| o.borrow().clone()) {
        return c;
    }
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(defaults)
}

/// Sets the configured conventions, for every thread.
pub fn set(c: Conventions) {
    let mut cur = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    if cur.as_deref() != Some(&c) {
        *cur = Some(Arc::new(c));
    }
}

/// Runs `f` with `c` in effect on this thread only (tests, and extraction with conventions other than the config's).
pub fn scoped<T>(c: Conventions, f: impl FnOnce() -> T) -> T {
    let prev = OVERRIDE.with(|o| o.borrow_mut().replace(Arc::new(c)));
    struct Restore(Option<Arc<Conventions>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            OVERRIDE.with(|o| *o.borrow_mut() = prev);
        }
    }
    let _restore = Restore(prev);
    f()
}

/// What `lang`'s extraction depends on here, as a number added to its extractor version (see
/// [`crate::Lang::extractor_version`]): 0 under the defaults, else a hash of its conventions in bits 16–30, so
/// configuring them re-extracts that language's files.
pub fn stamp(lang: crate::Lang) -> u32 {
    use crate::Lang;
    let c = current();
    let d = defaults();
    let text = match lang {
        Lang::CSharp if (&c.bus, &c.csharp_http, &c.di) != (&d.bus, &d.csharp_http, &d.di) => format!("{:?}{:?}{:?}", c.bus, c.csharp_http, c.di),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript if c.ts_http != d.ts_http => format!("{:?}", c.ts_http),
        _ => return 0,
    };
    // FNV-1a: stable across builds and platforms, unlike the standard hasher.
    let mut h: u32 = 0x811c_9dc5;
    for b in text.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    ((h % 0x7fff) + 1) << 16
}

/// Conventions of a made-up code base (`Shop*` names), for tests of what configured conventions do (written as
/// config in `kc_index::config::SYNTHETIC_CONVENTIONS`).
#[doc(hidden)]
pub fn synthetic() -> Conventions {
    let call = |s: &str, required: &[Arg], allowed: &[Arg]| Call::parse(s, required, allowed).unwrap();
    let bus = |s: &str| call(s, &[Arg::Message], BUS_ROLES);
    let req = |s: &str| call(s, REQUEST_ROLES, &[]);
    Conventions {
        bus: Bus {
            message_attributes: strings(&["ShopMessage"]),
            consumer_bases: strings(&["ShopConsumer", "SecuredShopConsumer", "IConsumer", "IHandleMessages"]),
            consumer_registrations: strings(&["AddConsumer", "AddShopConsumer", "AddSecuredShopConsumer"]),
            publish: vec![bus("Publish(message)"), bus("PublishToHost(message, host)"), bus("SendToQueue(message, host, queue)"), bus("Send(message)")],
        },
        csharp_http: CsHttp {
            request_attributes: vec![req("ShopRequest(service, path, verb)")],
            gateway_calls: vec![
                req("CallService(service, path, verb)"),
                req("CallServiceAsync(service, path, verb)"),
                req("SendToApi(verb, service, path)"),
                req("SendToApiAsync(verb, service, path)"),
            ],
            service_host_bases: strings(&["ShopServiceStartup"]),
        },
        di: Di { scope_lifetimes: vec![("PerJob".into(), "unit_of_work")] },
        ts_http: TsHttp { request_decorators: strings(&["RequestContract"]), path_keys: strings(&["endpoint"]), verb_keys: strings(&["httpMethod"]) },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_parse_with_their_roles() {
        let c = Call::parse("SendToQueue(message, host, queue)", &[Arg::Message], BUS_ROLES).unwrap();
        assert_eq!((c.name.as_str(), c.position(Arg::Queue)), ("SendToQueue", Some(2)));
        assert_eq!(c.render(), "SendToQueue(message, host, queue)");
        assert!(Call::parse("Publish()", &[Arg::Message], BUS_ROLES).is_err(), "message is required");
        assert!(Call::parse("Publish(message, path)", &[Arg::Message], BUS_ROLES).is_err(), "not a bus role");
        assert!(Call::parse("Publish(message, host, host)", &[Arg::Message], BUS_ROLES).is_err());
        assert!(Call::parse("Call(verb, service, path, _)", REQUEST_ROLES, &[]).is_ok());
        assert!(Call::parse("Call(service, path)", REQUEST_ROLES, &[]).is_err(), "verb is required");
        assert!(Call::parse("a.b(service, path, verb)", REQUEST_ROLES, &[]).is_err());
    }

    #[test]
    fn only_configured_conventions_move_a_languages_extractor_version() {
        assert_eq!(stamp(crate::Lang::CSharp), 0);
        let mut c = Conventions::default();
        c.ci.app_name_variables.push("APP".into());
        assert_eq!(scoped(c.clone(), || stamp(crate::Lang::CSharp)), 0, "CI conventions are read at query time");
        c.bus.message_attributes.push("Message".into());
        let s = scoped(c.clone(), || stamp(crate::Lang::CSharp));
        assert_ne!(s, 0);
        assert_eq!(s & 0xffff, 0);
        assert_eq!(scoped(c, || stamp(crate::Lang::TypeScript)), 0);
        assert_eq!(stamp(crate::Lang::CSharp), 0, "the override ends with its scope");
    }
}
