//! Message-bus facts of one repo: message types (`[ShopMessage]`), the code publishing or sending messages,
//! and the consumers handling them. Linking publishers to consumers across repos is `kc_index::bus`.

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::is_test_path;

/// The `via` of a MassTransit state machine's consume facts: it handles its events in itself, not in a `Consume`.
pub const SAGA: &str = "MassTransitStateMachine";

/// `A.B.Name` → `Name`.
pub fn bare_name(message: &str) -> &str {
    message.rsplit('.').next().unwrap_or(message)
}

/// One bus fact, with its symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusRow {
    /// `message`, `publish` or `consume`.
    pub kind: String,
    /// Qualified name of the message type, the publishing method, or the consumer class.
    pub symbol: String,
    pub sym_kind: String,
    pub file: String,
    /// Where the fact is: the type, the publish call, the consumer's base type.
    pub line: u32,
    pub start_line: u32,
    pub end_line: u32,
    /// For `message`, the type's dotted name in its namespace; otherwise the type as written, `None` for a publish
    /// whose type isn't known.
    pub message: Option<String>,
    /// For `message`, its namespace; otherwise the namespaces the written name may live in, nearest first.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Virtual host or queue.
    #[serde(default)]
    pub queue: Option<String>,
    /// The call (`Publish`, `Send`, …) or the consumer's base (`ShopConsumer`, `IConsumer`).
    #[serde(default)]
    pub via: Option<String>,
    /// In test code: listed, never linked.
    #[serde(default)]
    pub test: bool,
}

impl BusRow {
    /// The message's bare name.
    pub fn name(&self) -> Option<&str> {
        self.message.as_deref().map(bare_name)
    }

    /// A message type's full name (`Ns.Outer.Name`).
    pub fn full_name(&self) -> Option<String> {
        let m = self.message.as_deref()?;
        Some(match self.scopes.first() {
            Some(ns) if self.kind == "message" && !ns.is_empty() => format!("{ns}.{m}"),
            _ => m.to_string(),
        })
    }

    /// A consumer registered where a generic consumer is closed over the message (`AddMailConsumer<X>()`): it says
    /// the message is consumed, but the code running is the generic consumer's, not the registering method.
    pub fn registration(&self) -> bool {
        self.kind == "consume" && matches!(self.sym_kind.as_str(), "method" | "function" | "init" | "property")
    }

    /// The code that runs for a message: a consumer class's `Consume` method; otherwise the symbol itself (a state
    /// machine, a method registering a generic consumer, a publisher).
    pub fn handler(&self) -> String {
        if self.kind == "consume" && !self.registration() && self.via.as_deref() != Some(SAGA) {
            format!("{}.Consume", self.symbol)
        } else {
            self.symbol.clone()
        }
    }
}

/// Every bus fact in the repo, by file and line.
pub fn rows(conn: &Connection) -> Result<Vec<BusRow>> {
    let mut stmt = conn.prepare(
        "SELECT b.kind, s.name, s.container, s.kind, f.path, b.line, s.start_line, s.end_line, b.message, b.scopes, b.queue,
                b.via
         FROM bus_facts b JOIN symbols s ON s.id = b.symbol_id JOIN files f ON f.id = b.file_id
         ORDER BY f.path, b.line",
    )?;
    let rows = stmt.query_map([], |r| {
        let name: String = r.get(1)?;
        let container: Option<String> = r.get(2)?;
        let file: String = r.get(4)?;
        let scopes: String = r.get(9)?;
        Ok(BusRow {
            kind: r.get(0)?,
            symbol: container.map_or_else(|| name.clone(), |c| format!("{c}.{name}")),
            sym_kind: r.get(3)?,
            test: is_test_path(&file),
            file,
            line: r.get(5)?,
            start_line: r.get(6)?,
            end_line: r.get(7)?,
            message: r.get(8)?,
            scopes: scopes.split('|').filter(|s| !s.is_empty()).map(str::to_string).collect(),
            queue: r.get(10)?,
            via: r.get(11)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Whether a change to the symbol `symbol_id` can reach a message's consumers: it is a message type, it publishes,
/// or it is a type that the repo's code publishes or consumes by name. A cheap check before reading other repos.
pub fn involved(conn: &Connection, symbol_id: i64) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM bus_facts WHERE symbol_id = ?1 AND kind IN ('message', 'publish'))
             OR EXISTS(SELECT 1 FROM symbols s JOIN bus_facts b ON b.message_name = s.name
                       WHERE s.id = ?1 AND s.kind IN ('class', 'struct', 'protocol') AND b.kind IN ('publish', 'consume'))",
    )?;
    Ok(stmt.query_row([symbol_id], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, symbol: &str, message: &str, scopes: &[&str]) -> BusRow {
        BusRow {
            kind: kind.into(),
            symbol: symbol.into(),
            sym_kind: "class".into(),
            file: "src/X.cs".into(),
            line: 1,
            start_line: 1,
            end_line: 1,
            message: Some(message.into()),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            queue: None,
            via: None,
            test: false,
        }
    }

    #[test]
    fn test_files() {
        assert!(is_test_path("tests/Api.Tests/Consumers/PlacedConsumer.cs"));
        assert!(is_test_path("src/Api/PlacedConsumerTests.cs"));
        assert!(!is_test_path("src/Api/Consumers/MonitorTestFailedConsumer.cs"));
        assert!(!is_test_path("PlacedConsumer.cs"));
    }

    #[test]
    fn names_and_handlers() {
        let m = row("message", "Outer.Placed", "Outer.Placed", &["Shop.Events"]);
        assert_eq!((m.name(), m.full_name().as_deref()), (Some("Placed"), Some("Shop.Events.Outer.Placed")));
        let c = row("consume", "PlacedConsumer", "Events.Placed", &["Shop.Mail"]);
        assert_eq!((c.name(), c.full_name().as_deref(), c.handler().as_str()), (Some("Placed"), Some("Events.Placed"), "PlacedConsumer.Consume"));
    }
}
