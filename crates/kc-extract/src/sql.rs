//! SQL files (MySQL and MariaDB dumps, migrations and routines; T-SQL scripts), read by a statement-level scanner.
//!
//! No tree-sitter grammar parses these dialects well enough: dumps wrap statements in `/*!40101 … */` comments,
//! change the delimiter (`DELIMITER ;;`) around routines, and use MySQL-only syntax throughout, and a maintained SQL
//! grammar left most `CREATE` statements in such files inside error nodes. The scanner instead tokenizes (strings,
//! comments, quoted identifiers, the current delimiter), splits statements, and reads:
//!
//! - `CREATE TABLE`, `VIEW`, `PROCEDURE`, `FUNCTION`, `TRIGGER` and `EVENT` as symbols of those kinds, contained in
//!   their schema: the one written (`db.t`, `[dbo].[t]`), else the one a `USE db` or the dump's header names, else
//!   the one a dump part's file name gives (`shop.Views.sql`, `shop.Procs.sql`: see [`file_schema`]);
//! - not the stand-ins `mysqldump` writes for each view before the real one (`CREATE VIEW v AS SELECT 1 AS a`, or an
//!   older dump's `CREATE TABLE v (a tinyint)` under a "structure for view" comment), and not temporary tables;
//! - in a view, routine or trigger body, the tables it writes (`write`: `INSERT`, `REPLACE`, `UPDATE`, `DELETE`,
//!   `MERGE`, `TRUNCATE`, `DROP TABLE`, `RENAME TABLE`) and reads (`read`: `FROM`, `JOIN`, `MERGE … USING`), and the routines it calls (`call`:
//!   `CALL`, T-SQL `EXEC`, and calls of functions that aren't builtins);
//! - `ALTER TABLE`, `CREATE INDEX … ON`, foreign keys, `CREATE TABLE … LIKE` and a trigger's table as `type_ref`s.
//!
//! Reference names and schemas are lower-cased, since SQL matches names without case. Statements built in strings
//! (dynamic SQL) are not read. Columns aren't stored: [`columns`] reads them from a table's or view's text on demand.

use crate::swift::collapse;
use crate::{FileFacts, Ref, RefKind, Symbol, SymbolKind, Unsupported};

pub const VERSION: u32 = 2;

const SIGNATURE_CAP: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum T {
    Word,
    /// A quoted identifier: `` `x` ``, `[x]` (T-SQL), `"x"` (T-SQL).
    Quoted,
    Str,
    Num,
    /// `@x`, `@@x`.
    Var,
    Punct,
    /// The statement delimiter: `;` by default, what `DELIMITER` sets, or a T-SQL `GO` line.
    Delim,
}

/// 16 bytes: a dump of a few MB lexes to about a million tokens.
#[derive(Debug, Clone, Copy)]
struct Tok {
    t: T,
    /// The whole token's bytes, quotes included.
    os: u32,
    oe: u32,
    line: u32,
    /// First token on its line.
    bol: bool,
    /// Lexed while the delimiter was not `;` (inside `DELIMITER ;;`), or in a T-SQL file (where `GO` delimits).
    custom: bool,
}

struct Lexer<'a> {
    src: &'a str,
    b: &'a [u8],
    i: usize,
    line: u32,
    bol: bool,
    delim: String,
    tsql: bool,
    /// Open `/*!…` version comments, whose content is code.
    versioned: u32,
    toks: Vec<Tok>,
    errors: u32,
}

fn word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

impl Tok {
    /// The token's bytes; for a quoted identifier, without the quotes.
    fn span(&self) -> (usize, usize) {
        let (s, e) = self.outer();
        if self.t == T::Quoted { (s + 1, e.saturating_sub(1).max(s + 1)) } else { (s, e) }
    }

    fn outer(&self) -> (usize, usize) {
        (self.os as usize, self.oe as usize)
    }
}

impl Lexer<'_> {
    fn push(&mut self, t: T, outer: (usize, usize), line: u32) {
        let custom = self.tsql || self.delim != ";";
        self.toks.push(Tok { t, os: outer.0 as u32, oe: outer.1 as u32, line, bol: self.bol, custom });
        self.bol = false;
    }

    fn rest_of_line(&self) -> usize {
        self.b[self.i..].iter().position(|&c| c == b'\n').map_or(self.b.len(), |p| self.i + p)
    }

    fn run(&mut self) {
        while self.i < self.b.len() {
            let c = self.b[self.i];
            let next = self.b.get(self.i + 1).copied().unwrap_or(0);
            if c == b'\n' {
                self.line += 1;
                self.bol = true;
                self.i += 1;
                continue;
            }
            if c.is_ascii_whitespace() {
                self.i += 1;
                continue;
            }
            if self.bol && (c == b'D' || c == b'd') {
                let end = self.rest_of_line();
                let line = self.src[self.i..end].trim();
                // A short run of symbols (`;;`, `$$`, `//`): a line starting `delimiter VARCHAR(10))` is a parameter.
                let new = line.get(10..).unwrap_or("").trim();
                if line.get(..10).is_some_and(|h| h.eq_ignore_ascii_case("DELIMITER "))
                    && new.len() <= 4
                    && new.bytes().all(|c| c.is_ascii_punctuation() && !matches!(c, b'(' | b')' | b'`' | b'\'' | b'"'))
                {
                    self.delim = new.to_string();
                    self.i = end;
                    continue;
                }
            }
            if self.tsql && self.bol && (c == b'G' || c == b'g') {
                let end = self.rest_of_line();
                let line = self.src[self.i..end].trim().trim_end_matches(';');
                let mut parts = line.split_whitespace();
                if parts.next().is_some_and(|w| w.eq_ignore_ascii_case("GO")) && parts.all(|p| p.bytes().all(|d| d.is_ascii_digit())) {
                    let (s, line) = (self.i, self.line);
                    self.push(T::Delim, (s, s + 2), line);
                    self.i = end;
                    continue;
                }
            }
            // A dumped view's text keeps its line breaks as `\n`.
            if c == b'\\' && matches!(next, b'n' | b'r' | b't') {
                self.i += 2;
                continue;
            }
            if c == b'-' && next == b'-' || (c == b'#' && !self.tsql) {
                self.i = self.rest_of_line();
                continue;
            }
            if c == b'/' && next == b'*' {
                if self.b.get(self.i + 2) == Some(&b'!') {
                    self.i += 3;
                    while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                    self.versioned += 1;
                    continue;
                }
                let end = self.src[self.i + 2..].find("*/").map_or(self.b.len(), |p| self.i + 2 + p + 2);
                self.line += self.src[self.i..end].bytes().filter(|&c| c == b'\n').count() as u32;
                self.i = end;
                continue;
            }
            if c == b'*' && next == b'/' && self.versioned > 0 {
                self.versioned -= 1;
                self.i += 2;
                continue;
            }
            if !self.delim.is_empty() && self.src[self.i..].starts_with(self.delim.as_str()) && !self.tsql {
                let (s, e, line) = (self.i, self.i + self.delim.len(), self.line);
                self.push(T::Delim, (s, e), line);
                self.i = e;
                continue;
            }
            let (s, line) = (self.i, self.line);
            match c {
                b'\'' => {
                    let e = self.string(b'\'', !self.tsql);
                    self.push(T::Str, (s, e), line);
                }
                b'"' if !self.tsql => {
                    let e = self.string(b'"', true);
                    self.push(T::Str, (s, e), line);
                }
                b'"' | b'`' => {
                    let e = self.quoted(c);
                    self.push(T::Quoted, (s, e), line);
                }
                b'[' if self.tsql => {
                    let e = self.quoted(b']');
                    self.push(T::Quoted, (s, e), line);
                }
                b'@' => {
                    self.i += 1;
                    if self.b.get(self.i) == Some(&b'@') {
                        self.i += 1;
                    }
                    match self.b.get(self.i) {
                        Some(&q @ (b'`' | b'\'' | b'"')) => {
                            self.quoted(q);
                        }
                        _ => {
                            while self.i < self.b.len() && (word_byte(self.b[self.i]) || self.b[self.i] == b'.') {
                                self.i += 1;
                            }
                        }
                    }
                    let e = self.i;
                    self.push(T::Var, (s, e), line);
                }
                b'0'..=b'9' => {
                    while self.i < self.b.len() && (self.b[self.i].is_ascii_alphanumeric() || self.b[self.i] == b'.') {
                        self.i += 1;
                    }
                    let e = self.i;
                    self.push(T::Num, (s, e), line);
                }
                _ if word_byte(c) || (c == b'#' && self.tsql) => {
                    self.i += 1;
                    while self.i < self.b.len() && word_byte(self.b[self.i]) {
                        self.i += 1;
                    }
                    let e = self.i;
                    self.push(T::Word, (s, e), line);
                }
                _ => {
                    self.i += 1;
                    let e = self.i;
                    self.push(T::Punct, (s, e), line);
                }
            }
        }
    }

    /// Past a string opened at `self.i` by `q`, with doubled quotes (and backslashes, if `escapes`) escaping.
    fn string(&mut self, q: u8, escapes: bool) -> usize {
        self.i += 1;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c == b'\n' {
                self.line += 1;
            }
            if escapes && c == b'\\' {
                self.i += 2;
                continue;
            }
            self.i += 1;
            if c == q {
                if self.b.get(self.i) == Some(&q) {
                    self.i += 1;
                    continue;
                }
                return self.i;
            }
        }
        self.errors += 1;
        self.b.len()
    }

    /// Past an identifier quoted by `close` (opened at `self.i`). An identifier never spans lines: an unclosed quote
    /// ends at the line's end (a malformed dump), so it can't swallow the statements after it.
    fn quoted(&mut self, close: u8) -> usize {
        self.i += 1;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c == b'\n' {
                self.errors += 1;
                return self.i;
            }
            self.i += 1;
            if c == close {
                if self.b.get(self.i) == Some(&close) && close != b']' {
                    self.i += 1;
                    continue;
                }
                return self.i;
            }
        }
        self.errors += 1;
        self.b.len()
    }
}

/// The tokens, the number of unclosed quotes, and whether the file is T-SQL.
fn tokenize(src: &str) -> (Vec<Tok>, u32, bool) {
    tokenize_as(src, is_tsql(src))
}

fn tokenize_as(src: &str, tsql: bool) -> (Vec<Tok>, u32, bool) {
    let mut lx = Lexer { src, b: src.as_bytes(), i: 0, line: 1, bol: true, delim: ";".into(), tsql, versioned: 0, toks: Vec::new(), errors: 0 };
    lx.run();
    (lx.toks, lx.errors, tsql)
}

/// A T-SQL script: batches separated by `GO` lines, or `[dbo]`-style names.
fn is_tsql(src: &str) -> bool {
    src.lines().any(|l| l.trim().eq_ignore_ascii_case("GO")) || src.contains("[dbo]") || src.contains("[DBO]")
}

/// Keywords that never name a table where a table name may appear.
const NOT_NAMES: &[&str] = &[
    "SELECT", "WHERE", "SET", "VALUES", "VALUE", "LATERAL", "DUAL", "ON", "USING", "AS", "JOIN", "INNER", "LEFT", "RIGHT",
    "OUTER", "CROSS", "FULL", "NATURAL", "STRAIGHT_JOIN", "GROUP", "ORDER", "LIMIT", "HAVING", "UNION", "WITH", "FOR",
    "INTO", "FROM", "IGNORE", "LOW_PRIORITY", "HIGH_PRIORITY", "DELAYED", "QUICK", "TABLE", "IF", "NOT", "EXISTS", "AND",
    "OR", "CASE", "WHEN", "THEN", "ELSE", "END", "BEGIN", "DECLARE", "RETURN", "WINDOW", "PARTITION", "USE", "FORCE",
    "OFFSET", "FETCH", "OUTPUT", "TOP", "DEFAULT", "NULL", "INTERVAL",
];

/// Words ending a table list: the next clause.
const CLAUSES: &[&str] = &[
    "WHERE", "SET", "GROUP", "ORDER", "LIMIT", "HAVING", "UNION", "EXCEPT", "INTERSECT", "WINDOW", "INTO", "VALUES",
    "SELECT", "FOR", "LOCK", "PROCEDURE", "OPTION", "WHEN", "THEN", "ELSE", "END", "RETURNING", "OUTPUT", "OFFSET",
    "FETCH", "USING", "ON", "DO", "IF", "BEGIN", "DECLARE", "INSERT", "UPDATE", "DELETE", "REPLACE", "CALL", "EXEC",
    "EXECUTE", "OPEN", "CLOSE", "LEAVE", "ITERATE", "WHILE", "LOOP", "REPEAT", "UNTIL", "RETURN", "SIGNAL", "PREPARE",
    "DEALLOCATE", "COMMIT", "ROLLBACK", "START", "TRUNCATE", "MERGE", "WITH", "CREATE", "DROP", "ALTER", "ELSEIF",
];

const JOIN_WORDS: &[&str] = &["JOIN", "INNER", "LEFT", "RIGHT", "OUTER", "CROSS", "FULL", "NATURAL", "STRAIGHT_JOIN", "APPLY"];

/// Functions, types and keywords written before `(` that aren't calls of a routine the code base defines.
const BUILTINS: &[&str] = &[
    // Keywords and clauses.
    "ALL", "AND", "ANY", "AS", "AGAINST", "BETWEEN", "BY", "CHECK", "CONSTRAINT", "DEFAULT", "DISTINCT", "ELSE",
    "ELSEIF", "EXISTS", "FOREIGN", "FROM", "HAVING", "IF", "IN", "INDEX", "INTO", "IS", "JOIN", "KEY", "LIKE", "LIMIT",
    "MATCH", "NOT", "ON", "OR", "OVER", "PARTITION", "PRIMARY", "REFERENCES", "RETURN", "RETURNS", "ROW", "ROWS",
    "SELECT", "SET", "SOME", "THEN", "UNION", "UNIQUE", "USING", "VALUES", "VALUE", "WHEN", "WHERE", "WHILE", "WITH",
    "UNTIL", "XOR", "CASE", "INTERVAL", "SIGNAL", "TABLE", "FULLTEXT", "SPATIAL", "PROCEDURE", "FUNCTION", "ESCAPE",
    "OUTPUT", "TOP", "CONTAINS", "FREETEXT", "OPENJSON", "OPENROWSET", "OPENQUERY", "GENERATED", "ALWAYS", "STORED",
    "VIRTUAL", "PERIOD", "SYSTEM_TIME", "FOR", "WITHIN", "FILTER", "RAISERROR", "THROW", "PRINT", "CURSOR",
    // Types.
    "BIGINT", "BINARY", "BIT", "BLOB", "BOOL", "BOOLEAN", "CHAR", "CHARACTER", "DATE", "DATETIME", "DATETIME2",
    "DATETIMEOFFSET", "DEC", "DECIMAL", "DOUBLE", "ENUM", "FLOAT", "INT", "INTEGER", "LONGBLOB", "LONGTEXT",
    "MEDIUMBLOB", "MEDIUMINT", "MEDIUMTEXT", "MONEY", "NCHAR", "NUMERIC", "NVARCHAR", "REAL", "SMALLINT", "TEXT", "TIME",
    "TIMESTAMP", "TINYBLOB", "TINYINT", "TINYTEXT", "VARBINARY", "VARCHAR", "YEAR", "SMALLDATETIME", "UNIQUEIDENTIFIER",
    "NTEXT", "IMAGE", "JSON", "POINT", "GEOMETRY", "SIGNED", "UNSIGNED", "PRECISION",
    // Functions.
    "ABS", "ACOS", "ADDDATE", "ADDTIME", "AES_DECRYPT", "AES_ENCRYPT", "ASCII", "ASIN", "ATAN", "ATAN2", "AVG",
    "BENCHMARK", "BIN", "BIT_AND", "BIT_COUNT", "BIT_LENGTH", "BIT_OR", "BIT_XOR", "CAST", "CEIL", "CEILING",
    "CHAR_LENGTH", "CHARACTER_LENGTH", "CHARINDEX", "CHARSET", "COALESCE", "COERCIBILITY", "COLLATION", "COMPRESS",
    "CONCAT", "CONCAT_WS", "CONNECTION_ID", "CONV", "CONVERT", "CONVERT_TZ", "COS", "COT", "COUNT", "COUNT_BIG", "CRC32",
    "CUME_DIST", "CURDATE", "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP", "CURRENT_USER", "CURTIME", "DATABASE",
    "DATALENGTH", "DATE_ADD", "DATE_FORMAT", "DATE_SUB", "DATEADD", "DATEDIFF", "DATENAME", "DATEPART", "DAY", "DAYNAME",
    "DAYOFMONTH", "DAYOFWEEK", "DAYOFYEAR", "DEGREES", "DENSE_RANK", "DIV", "ELT", "EOMONTH", "EXP", "EXPORT_SET",
    "EXTRACT", "FIELD", "FIND_IN_SET", "FIRST_VALUE", "FLOOR", "FORMAT", "FOUND_ROWS", "FROM_BASE64", "FROM_DAYS",
    "FROM_UNIXTIME", "GET_FORMAT", "GET_LOCK", "GETDATE", "GETUTCDATE", "GREATEST", "GROUP_CONCAT", "HEX", "HOUR", "IFNULL",
    "IIF", "INET_ATON", "INET_NTOA", "INET6_ATON", "INET6_NTOA", "INSERT", "INSTR", "ISNULL", "IS_FREE_LOCK",
    "IS_USED_LOCK", "ISNUMERIC", "LAG", "LAST_DAY", "LAST_INSERT_ID", "LAST_VALUE", "LCASE", "LEAD", "LEAST", "LEFT",
    "LEN", "LENGTH", "LN", "LOAD_FILE", "LOCALTIME", "LOCALTIMESTAMP", "LOCATE", "LOG", "LOG10", "LOG2", "LOWER", "LPAD",
    "LTRIM", "MAKE_SET", "MAKEDATE", "MAKETIME", "MAX", "MD5", "MICROSECOND", "MID", "MIN", "MINUTE", "MOD", "MONTH",
    "MONTHNAME", "NAME_CONST", "NEWID", "NOW", "NTH_VALUE", "NTILE", "NULLIF", "OCT", "OCTET_LENGTH", "ORD",
    "PERCENT_RANK", "PERIOD_ADD", "PERIOD_DIFF", "PI", "POSITION", "POW", "POWER", "QUARTER", "QUOTE", "QUOTENAME",
    "RADIANS", "RAND", "RANDOM_BYTES", "RANK", "REGEXP_INSTR", "REGEXP_LIKE", "REGEXP_REPLACE", "REGEXP_SUBSTR",
    "RELEASE_ALL_LOCKS", "RELEASE_LOCK", "REPEAT", "REPLACE", "REPLICATE", "REVERSE", "RIGHT", "ROUND", "ROW_COUNT",
    "ROW_NUMBER", "ROWCOUNT_BIG", "RPAD", "RTRIM", "SCHEMA", "SCOPE_IDENTITY", "SEC_TO_TIME", "SECOND", "SESSION_USER",
    "SHA", "SHA1", "SHA2", "SIGN", "SIN", "SLEEP", "SOUNDEX", "SPACE", "SQRT", "STD", "STDDEV", "STDDEV_POP",
    "STDDEV_SAMP", "STR", "STR_TO_DATE", "STRCMP", "STRING_AGG", "STRING_SPLIT", "STUFF", "SUBDATE", "SUBSTR",
    "SUBSTRING", "SUBSTRING_INDEX", "SUBTIME", "SUM", "SYSDATE", "SYSDATETIME", "SYSTEM_USER", "TAN", "TIME_FORMAT",
    "TIME_TO_SEC", "TIMEDIFF", "TIMESTAMPADD", "TIMESTAMPDIFF", "TO_BASE64", "TO_DAYS", "TO_SECONDS", "TRANSLATE",
    "TRIM", "TRUNCATE", "TRY_CAST", "TRY_CONVERT", "UCASE", "UNCOMPRESS", "UNHEX", "UNIX_TIMESTAMP", "UPPER", "USER",
    "UTC_DATE", "UTC_TIME", "UTC_TIMESTAMP", "UUID", "UUID_SHORT", "VAR_POP", "VAR_SAMP", "VARIANCE", "VERSION", "WEEK",
    "WEEKDAY", "WEEKOFYEAR", "YEARWEEK", "OBJECT_ID", "DB_NAME", "ERROR_MESSAGE", "ERROR_NUMBER", "ERROR_LINE",
    "ERROR_SEVERITY", "ERROR_STATE", "ERROR_PROCEDURE", "XACT_STATE", "FORMATMESSAGE", "HASHBYTES", "DECODE", "ENCODE",
    "ENCRYPT", "PASSWORD", "OLD_PASSWORD", "UNIX_TIME", "ROUNDDOWN", "CHR", "NVL", "TO_CHAR", "TO_DATE", "SQL_CALC_FOUND_ROWS",
];

/// Functions taking `FROM` among their arguments (`EXTRACT(YEAR FROM d)`), whose parentheses are skipped.
const FROM_FUNCTIONS: &[&str] = &["EXTRACT", "TRIM", "SUBSTRING", "SUBSTR", "POSITION", "OVERLAY", "CONVERT"];

fn is_builtin(upper: &str) -> bool {
    BUILTINS.contains(&upper) || upper.starts_with("JSON_") || upper.starts_with("ST_") || upper.starts_with("MBR")
}

/// A name in the source: `db.table` gives `["db", "table"]`; quotes are dropped.
type Name = Vec<String>;

struct Scanner<'a> {
    src: &'a str,
    toks: Vec<Tok>,
    facts: FileFacts,
    /// The schema a `USE` or the dump header names.
    schema: Option<String>,
    tsql: bool,
}

pub fn extract(source: &str) -> FileFacts {
    extract_at("", source)
}

/// [`extract`] for the file at `path`, whose name may give the schema (see [`file_schema`]).
pub fn extract_at(path: &str, source: &str) -> FileFacts {
    let (toks, errors, tsql) = tokenize(source);
    let schema = dump_database(source).or_else(|| file_schema(path));
    let mut s = Scanner { src: source, toks, facts: FileFacts::default(), schema, tsql };
    s.facts.parse_errors = errors;
    s.run();
    s.facts
}

/// What SQL text embedded in other code (a CFML `<cfquery>`, with its `#…#` values already replaced) reads, writes
/// and calls: (the name as written, `db.table`; [`RefKind::Read`], [`RefKind::Write`] or [`RefKind::Call`]; line
/// within the text). Common table expressions, temporary tables and variables are left out, and a call is a `CALL x`
/// or T-SQL `EXEC x`, not a function in an expression. T-SQL is told by `[dbo]`-style names, `GO` lines or a few
/// T-SQL-only words.
pub fn embedded(text: &str) -> Vec<(String, RefKind, u32)> {
    let upper = text.to_ascii_uppercase();
    let tsql = is_tsql(text)
        || upper.trim_start().starts_with("EXEC ")
        || ["NOLOCK", "GETDATE()", "SCOPE_IDENTITY", "@@IDENTITY", "ISNULL("].iter().any(|w| upper.contains(w));
    let (toks, _, tsql) = tokenize_as(text, tsql);
    let s = Scanner { src: text, toks, facts: FileFacts::default(), schema: None, tsql };
    let n = s.toks.len();
    let (ctes, temps) = s.locals(0, n);
    // Routines named after `CALL` or `EXEC` (`EXEC @rc = p`).
    let mut called: Vec<String> = Vec::new();
    for k in 0..n {
        if s.is(k, "CALL") || s.is(k, "EXEC") || s.is(k, "EXECUTE") {
            let m = if s.toks.get(k + 1).is_some_and(|t| t.t == T::Var) && s.punct(k + 2, b'=') { k + 3 } else { k + 1 };
            if let Some((name, _)) = s.name(m) {
                called.push(name.last().unwrap().to_lowercase());
            }
        }
    }
    let mut found: Vec<(Name, RefKind, u32)> = Vec::new();
    // DDL run from code (`CREATE TABLE IF NOT EXISTS x`, `ALTER TABLE x`, `DROP TABLE x`) changes the table: a write.
    for k in 0..n {
        if matches!(s.upper(k).as_str(), "CREATE" | "ALTER" | "DROP") && s.is(k + 1, "TABLE") {
            let mut m = k + 2;
            if s.is(m, "IF") {
                m += if s.is(m + 1, "NOT") { 3 } else { 2 };
            }
            if let Some((name, _)) = s.name(m) {
                found.push((name, RefKind::Write, s.toks[m].line));
            }
        }
    }
    s.walk(0, n, &mut found);
    let mut out: Vec<(String, RefKind, u32)> = Vec::new();
    for (name, kind, line) in found {
        let Some(last) = name.last().map(|l| l.to_lowercase()) else { continue };
        if last.is_empty()
            || last.starts_with('#')
            || (name.len() == 1 && ctes.contains(&last))
            || temps.contains(&last)
            || (kind == RefKind::Call && !called.contains(&last))
        {
            continue;
        }
        let joined = name.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(".");
        if !out.iter().any(|(o, k, _)| *k == kind && o.eq_ignore_ascii_case(&joined)) {
            out.push((joined, kind, line));
        }
    }
    out
}

/// Stands for a value spliced into SQL text in code (a CFML `#…#`, a C# interpolation or concatenated variable): a
/// name containing it is built at run time.
pub const DYNAMIC: &str = "cfdyn_";

/// Schemas of the engine's own catalog: `information_schema.columns` is not the app's table.
const SYSTEM_SCHEMAS: &[&str] = &["information_schema", "sys", "mysql", "performance_schema", "pg_catalog"];

/// The tables SQL text in code reads (`sql_read`) or writes (`sql_write`) by a name built at run time
/// (`$"SELECT * FROM {table}"`, `"DELETE FROM " + t`), with their line offsets in the text: [`code_tables`] leaves them
/// out, as no table can be named.
pub fn dynamic_tables(sql: &str) -> Vec<(&'static str, u32)> {
    let mut out = Vec::new();
    for (name, kind, line) in embedded(sql) {
        let last = name.rsplit('.').next().unwrap_or("");
        let kind = match kind {
            RefKind::Write => "sql_write",
            RefKind::Call => continue,
            _ => "sql_read",
        };
        let line = line.saturating_sub(1);
        if last.to_ascii_lowercase().contains(DYNAMIC) && !out.contains(&(kind, line)) {
            out.push((kind, line));
        }
    }
    out
}

/// Tables SQL text in code reads and writes and procedures it calls, as db facts: (name as written, `sql_read`,
/// `sql_write` or `procedure`, line offset in the text). Read by [`embedded`]; names built from a value ([`DYNAMIC`];
/// a dynamic schema keeps its table), catalog tables, `dual`, `JSON_TABLE` and WMI's `Win32_*` classes are left out.
pub fn code_tables(sql: &str) -> Vec<(String, &'static str, u32)> {
    let mut out = Vec::new();
    for (name, kind, line) in embedded(sql) {
        let parts: Vec<&str> = name.split('.').collect();
        let last = *parts.last().unwrap_or(&"");
        let lower = last.to_ascii_lowercase();
        // `dual`, MySQL's `JSON_TABLE(…)` and WMI's `Win32_*` classes (WQL reads like SQL) are no tables.
        if last.is_empty() || lower.contains(DYNAMIC) || lower == "dual" || lower == "json_table" || lower.starts_with("win32_") {
            continue;
        }
        let schema_dynamic = parts[..parts.len() - 1].iter().any(|p| p.to_ascii_lowercase().contains(DYNAMIC));
        if parts.len() > 1 && SYSTEM_SCHEMAS.contains(&parts[parts.len() - 2].to_ascii_lowercase().as_str()) {
            continue;
        }
        let name = if schema_dynamic { last.to_string() } else { name };
        let kind = match kind {
            RefKind::Write => "sql_write",
            RefKind::Call => "procedure",
            _ => "sql_read",
        };
        out.push((name, kind, line.saturating_sub(1)));
    }
    out
}

/// The schema a dump part's file name gives: `shop` for `shop.Views.sql`, `shop.Procs.sql`, `shop.Triggers.sql`,
/// `shop.Events.sql`, `shop.Functions.sql`, `shop.Routines.sql`, `shop.Tables.sql`, `shop.Indexes.sql` or
/// `shop.Constraints.sql` (case aside). Tools that dump a schema one object kind per file name the files so, and the
/// view and constraint parts carry no header naming the database. Other names give none.
pub fn file_schema(path: &str) -> Option<String> {
    const PARTS: &[&str] = &["views", "procs", "procedures", "triggers", "events", "functions", "routines", "tables", "indexes", "constraints"];
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.strip_suffix(".sql").or_else(|| name.strip_suffix(".SQL"))?;
    let (schema, part) = stem.rsplit_once('.')?;
    (PARTS.contains(&part.to_ascii_lowercase().as_str())
        && !schema.is_empty()
        && schema.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$'))
    .then(|| schema.to_string())
}

/// `-- Host: h    Database: account` in a `mysqldump` header, or `Dumping routines for database 'account'`.
fn dump_database(src: &str) -> Option<String> {
    let head = &src[..src.len().min(4096)];
    for line in head.lines().filter(|l| l.starts_with("--")) {
        if let Some(i) = line.find("Database: ") {
            let db = line[i + 10..].trim();
            if !db.is_empty() && !db.contains(' ') {
                return Some(db.to_string());
            }
        }
        if let Some(i) = line.find("for database '") {
            let rest = &line[i + 14..];
            if let Some(end) = rest.find('\'') {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

impl Scanner<'_> {
    fn text(&self, k: usize) -> &str {
        let t = &self.toks[k];
        let (s, e) = t.span();
        &self.src[s..e]
    }

    fn upper(&self, k: usize) -> String {
        match self.toks.get(k) {
            Some(t) if t.t == T::Word => self.src[t.os as usize..t.oe as usize].to_ascii_uppercase(),
            _ => String::new(),
        }
    }

    fn is(&self, k: usize, word: &str) -> bool {
        self.toks.get(k).is_some_and(|t| t.t == T::Word && self.src[t.os as usize..t.oe as usize].eq_ignore_ascii_case(word))
    }

    fn punct(&self, k: usize, p: u8) -> bool {
        self.toks.get(k).is_some_and(|t| t.t == T::Punct && self.src.as_bytes()[t.os as usize] == p)
    }

    fn ends(&self, k: usize) -> bool {
        self.toks.get(k).is_none_or(|t| t.t == T::Delim || (t.t == T::Punct && self.src.as_bytes()[t.os as usize] == b';'))
    }

    /// The index of the `)` matching the `(` at `k`, or the end.
    fn close(&self, k: usize, end: usize) -> usize {
        let mut depth = 0;
        for j in k..end {
            if self.punct(j, b'(') {
                depth += 1;
            } else if self.punct(j, b')') {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            } else if self.toks[j].t == T::Delim {
                return j;
            }
        }
        end
    }

    /// A possibly qualified name at `k`, and the index after it.
    fn name(&self, k: usize) -> Option<(Name, usize)> {
        let ok = |j: usize| self.toks.get(j).is_some_and(|t| matches!(t.t, T::Word | T::Quoted));
        if !ok(k) || (self.toks[k].t == T::Word && NOT_NAMES.contains(&self.upper(k).as_str())) {
            return None;
        }
        let mut parts = vec![self.text(k).to_string()];
        let mut j = k + 1;
        while self.punct(j, b'.') && ok(j + 1) {
            parts.push(self.text(j + 1).to_string());
            j += 2;
        }
        Some((parts, j))
    }

    /// Where the top-level statement starting at `k` ends: its delimiter, or (for a malformed dump whose quotes
    /// don't pair) a statement keyword starting a later line.
    fn statement_end(&self, k: usize) -> usize {
        let mut depth = 0i32;
        for j in k + 1..self.toks.len() {
            let t = &self.toks[j];
            if t.t == T::Delim || (depth <= 0 && self.punct(j, b';')) {
                return j;
            }
            if self.punct(j, b'(') {
                depth += 1;
            } else if self.punct(j, b')') {
                depth -= 1;
            } else if t.bol && t.t == T::Word && matches!(self.upper(j).as_str(), "CREATE" | "ALTER" | "DROP" | "USE" | "LOCK" | "UNLOCK") {
                return j;
            }
        }
        self.toks.len()
    }

    fn run(&mut self) {
        let mut k = 0;
        while k < self.toks.len() {
            if self.ends(k) {
                k += 1;
                continue;
            }
            let end = match self.upper(k).as_str() {
                "USE" => {
                    if let Some((n, _)) = self.name(k + 1) {
                        self.schema = n.last().cloned();
                    }
                    self.statement_end(k)
                }
                "CREATE" => self.create(k),
                "ALTER" => {
                    let end = self.statement_end(k);
                    if self.is(k + 1, "TABLE") {
                        if let Some((n, _)) = self.name(k + 2) {
                            self.reference(None, &n, RefKind::TypeRef, self.toks[k + 2].line);
                        }
                        self.foreign_keys(None, k + 2, end);
                    } else if (self.is(k + 1, "DATABASE") || self.is(k + 1, "SCHEMA")) && self.schema.is_none() {
                        self.schema = self.name(k + 2).and_then(|(n, _)| n.last().cloned()).filter(|s| !s.is_empty());
                    }
                    end
                }
                _ => self.statement_end(k),
            };
            k = end.max(k + 1);
        }
    }

    /// `CREATE …` at `k`; returns where it ends.
    fn create(&mut self, k: usize) -> usize {
        let mut j = k + 1;
        let mut temporary = false;
        let mut kind = None;
        while j < self.toks.len() && j < k + 16 && !self.ends(j) {
            match self.upper(j).as_str() {
                "TABLE" => kind = Some(SymbolKind::Table),
                "VIEW" => kind = Some(SymbolKind::View),
                "PROCEDURE" | "PROC" => kind = Some(SymbolKind::Procedure),
                "FUNCTION" => kind = Some(SymbolKind::Function),
                "TRIGGER" => kind = Some(SymbolKind::Trigger),
                "EVENT" => kind = Some(SymbolKind::Event),
                "INDEX" => {
                    // `CREATE [UNIQUE] INDEX i ON t (…)`
                    let end = self.statement_end(k);
                    if let Some(on) = (j..end).find(|&x| self.is(x, "ON"))
                        && let Some((n, _)) = self.name(on + 1)
                    {
                        self.reference(None, &n, RefKind::TypeRef, self.toks[on + 1].line);
                    }
                    return end;
                }
                "TEMPORARY" | "TEMP" => temporary = true,
                "DATABASE" | "SCHEMA" | "USER" | "ROLE" | "SERVER" | "TABLESPACE" | "SEQUENCE" | "TYPE" | "LOGIN" => {
                    return self.statement_end(k);
                }
                _ => {}
            }
            j += 1;
            if kind.is_some() {
                break;
            }
        }
        let Some(kind) = kind else { return self.statement_end(k) };
        if self.is(j, "IF") && self.is(j + 1, "NOT") && self.is(j + 2, "EXISTS") {
            j += 3;
        }
        let Some((name, after)) = self.name(j) else {
            self.facts.parse_errors += 1;
            return self.statement_end(k);
        };
        match kind {
            SymbolKind::Table if temporary || name.last().is_some_and(|n| n.starts_with('#')) => self.statement_end(k),
            SymbolKind::Table | SymbolKind::View if self.stand_in(k, &name, after) => self.statement_end(k),
            SymbolKind::Table => self.table(k, name, after),
            SymbolKind::View => self.view(k, name, after),
            _ => self.routine(k, kind, name, after),
        }
    }

    /// Whether the `CREATE TABLE` or `CREATE VIEW` at `k` (named `name`, its name ending before `after`) is a stand-in
    /// `mysqldump` writes so that views can refer to each other before the real view is created: under a
    /// "Temporary view structure for view `v`" (or older "Temporary table structure for view") comment, or a view
    /// selecting nothing but constants (`SELECT 1 AS a, 1 AS b`).
    fn stand_in(&self, k: usize, name: &Name, after: usize) -> bool {
        let start = self.toks[k].outer().0;
        let mut from = start.saturating_sub(600);
        while !self.src.is_char_boundary(from) {
            from -= 1;
        }
        let before = &self.src[from..start];
        // Not the "Final view structure for view" comment above the real view.
        let marker = ["Temporary view structure for view ", "Temporary table structure for view "]
            .iter()
            .filter_map(|m| before.rfind(m).map(|i| i + m.len()))
            .max();
        if let Some(i) = marker
            && let Some(last) = name.last()
        {
            let rest = &before[i..];
            let named = rest.trim_start_matches(['`', '"', '[']);
            if named.len() >= last.len()
                && named.is_char_boundary(last.len())
                && named[..last.len()].eq_ignore_ascii_case(last)
                && !rest.contains("CREATE")
            {
                return true;
            }
        }
        // `AS SELECT 1 AS a, 1 AS b` and nothing more.
        let end = self.statement_end(k);
        let Some(select) = (after..end).find(|&x| self.is(x, "SELECT")) else { return false };
        if !self.is(select - 1, "AS") {
            return false;
        }
        let mut x = select + 1;
        let mut items = 0;
        while x < end {
            let constant = self.toks.get(x).is_some_and(|t| t.t == T::Num) || self.is(x, "NULL");
            if !(constant && self.is(x + 1, "AS") && self.toks.get(x + 2).is_some_and(|t| matches!(t.t, T::Word | T::Quoted))) {
                return false;
            }
            items += 1;
            x += 3;
            if self.punct(x, b',') {
                x += 1;
            } else {
                break;
            }
        }
        items > 0 && x >= end
    }

    fn container(&self, name: &Name) -> Option<String> {
        match name.len() {
            0 | 1 => self.schema.clone(),
            n => Some(name[n - 2].clone()).filter(|s| !s.is_empty()).or_else(|| self.schema.clone()),
        }
    }

    fn symbol(&mut self, kind: SymbolKind, name: &Name, k: usize, last: usize, signature: String, complexity: u32) -> usize {
        let start = &self.toks[k];
        let end = &self.toks[last.min(self.toks.len() - 1).max(k)];
        let sym = Symbol {
            parent: None,
            name: name.last().cloned().unwrap_or_default(),
            kind,
            container: self.container(name),
            signature,
            doc: None,
            attrs: Vec::new(),
            tags: Vec::new(),
            start_line: start.line,
            end_line: end.line,
            start_byte: start.os,
            end_byte: end.oe,
            complexity,
            nesting: 0,
            bumps: Vec::new(),
            cond_ops: 0,
            cond_line: 0,
            type_name: None,
            arity: None,
            lambda_types: None,
        };
        self.facts.symbols.push(sym);
        self.facts.symbols.len() - 1
    }

    fn header(&self, k: usize, upto: usize) -> String {
        let s = self.toks[k].outer().0;
        let e = self.toks[upto.saturating_sub(1).max(k)].outer().1;
        collapse(&self.src[s..e], SIGNATURE_CAP)
    }

    fn table(&mut self, k: usize, name: Name, after: usize) -> usize {
        let end = self.statement_end(k);
        let mut signature = self.header(k, after);
        let mut refs = Vec::new();
        let mut options = after;
        if self.punct(after, b'(') {
            let close = self.close(after, end);
            let columns = columns_in(self, after + 1, close).len();
            signature.push_str(&format!(" ({columns} columns)"));
            refs.extend(self.references_in(after + 1, close));
            options = close;
        }
        let sym = self.symbol(SymbolKind::Table, &name, k, end.saturating_sub(1), signature, 0);
        // `ENGINE=BLACKHOLE`: a replica's stand-in that stores nothing (a warehouse copy), tagged `blackhole`.
        if let Some(e) = (options..end).find(|&x| self.is(x, "ENGINE"))
            && (self.is(e + 1, "BLACKHOLE") || (self.punct(e + 1, b'=') && self.is(e + 2, "BLACKHOLE")))
        {
            self.facts.symbols[sym].tags.push("blackhole");
        }
        for (n, line) in refs {
            self.reference(Some(sym), &n, RefKind::TypeRef, line);
        }
        // `CREATE TABLE t LIKE u`, `CREATE TABLE t AS SELECT …`
        if let Some(like) = (after..end).find(|&x| self.is(x, "LIKE"))
            && let Some((n, _)) = self.name(like + 1)
        {
            self.reference(Some(sym), &n, RefKind::TypeRef, self.toks[like + 1].line);
        }
        if let Some(select) = (after..end).find(|&x| self.is(x, "SELECT")) {
            self.body(sym, select, end);
        }
        end
    }

    /// Tables named by `REFERENCES` between `a` and `b`.
    fn references_in(&self, a: usize, b: usize) -> Vec<(Name, u32)> {
        (a..b).filter(|&x| self.is(x, "REFERENCES")).filter_map(|x| Some((self.name(x + 1)?.0, self.toks[x + 1].line))).collect()
    }

    fn foreign_keys(&mut self, from: Option<usize>, a: usize, b: usize) {
        for (n, line) in self.references_in(a, b) {
            self.reference(from, &n, RefKind::TypeRef, line);
        }
    }

    fn view(&mut self, k: usize, name: Name, after: usize) -> usize {
        let end = self.statement_end(k);
        let Some(as_at) = (after..end).find(|&x| self.is(x, "AS")) else {
            self.facts.parse_errors += 1;
            return end;
        };
        let signature = self.header(k, as_at);
        let sym = self.symbol(SymbolKind::View, &name, k, end.saturating_sub(1), signature, 0);
        self.body(sym, as_at + 1, end);
        end
    }

    /// A procedure, function, trigger or event: its header up to the body, and the body.
    fn routine(&mut self, k: usize, kind: SymbolKind, name: Name, after: usize) -> usize {
        let custom = self.toks[k].custom;
        // Where the body starts: after `AS` (T-SQL), `FOR EACH ROW` (a trigger), `DO` (an event), or at its first
        // statement.
        let mut j = after;
        let mut depth = 0;
        let mut table = None;
        let mut body = None;
        while j < self.toks.len() {
            if self.toks[j].t == T::Delim || (!custom && depth == 0 && self.punct(j, b';')) {
                break;
            }
            if self.punct(j, b'(') {
                depth += 1;
            } else if self.punct(j, b')') {
                depth -= 1;
            } else if depth == 0 && self.toks[j].t == T::Word {
                let u = self.upper(j);
                match u.as_str() {
                    "ON" if kind == SymbolKind::Trigger && table.is_none() => {
                        if let Some((n, next)) = self.name(j + 1) {
                            table = Some((n, self.toks[j + 1].line));
                            j = next;
                            continue;
                        }
                    }
                    "ROW" if kind == SymbolKind::Trigger => {
                        let mut b = j + 1;
                        if matches!(self.upper(b).as_str(), "FOLLOWS" | "PRECEDES") {
                            b += 2;
                        }
                        body = Some(b);
                        break;
                    }
                    "AS" if kind != SymbolKind::Event && j > after => {
                        body = Some(j + 1);
                        break;
                    }
                    "DO" if kind == SymbolKind::Event => {
                        body = Some(j + 1);
                        break;
                    }
                    "BEGIN" | "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "RETURN" | "SET" | "CALL" | "IF"
                    | "DECLARE" | "WITH" | "WHILE" | "TRUNCATE" | "MERGE"
                        if !matches!(kind, SymbolKind::Event | SymbolKind::Trigger) && !(u == "SET" && self.is(j - 1, "CHARACTER")) =>
                    {
                        body = Some(j);
                        break;
                    }
                    _ => {}
                }
            }
            j += 1;
        }
        let Some(start) = body else {
            self.facts.parse_errors += 1;
            return self.statement_end(k);
        };
        let end = self.body_end(start, custom);
        let signature = self.header(k, start);
        let complexity = 1 + (start..end)
            .filter(|&x| matches!(self.upper(x).as_str(), "IF" | "ELSEIF" | "WHILE" | "LOOP" | "REPEAT" | "WHEN" | "CASE"))
            .filter(|&x| !self.is(x.wrapping_sub(1), "END") && !self.punct(x + 1, b'('))
            .count() as u32;
        let sym = self.symbol(kind, &name, k, end.saturating_sub(1), signature, complexity);
        if let Some((n, line)) = table {
            self.reference(Some(sym), &n, RefKind::TypeRef, line);
        }
        self.body(sym, start, end);
        end
    }

    /// Where a routine body starting at `start` ends (exclusive). Inside `DELIMITER ;;` or a T-SQL batch, at the
    /// delimiter; otherwise a `BEGIN … END` block ends at its `END`, and a single statement at its `;`.
    fn body_end(&mut self, start: usize, custom: bool) -> usize {
        let n = self.toks.len();
        if custom {
            return (start..n).find(|&x| self.toks[x].t == T::Delim).unwrap_or(n);
        }
        if !self.is(start, "BEGIN") {
            return (start..n).find(|&x| self.ends(x)).unwrap_or(n);
        }
        // Blocks closed by a bare `END`: `BEGIN` and `CASE`. `END IF`, `END LOOP` and the like close statements that
        // were never counted; `END CASE` closes a `CASE` statement.
        let mut stack: Vec<bool> = Vec::new();
        let mut x = start;
        while x < n {
            match self.upper(x).as_str() {
                "BEGIN" if !matches!(self.upper(x + 1).as_str(), "TRAN" | "TRANSACTION" | "WORK" | "DISTRIBUTED") => {
                    stack.push(true);
                }
                "CASE" if !self.is(x.wrapping_sub(1), "END") => stack.push(false),
                "END" => {
                    let next = self.upper(x + 1);
                    if matches!(next.as_str(), "IF" | "LOOP" | "WHILE" | "REPEAT" | "TRY" | "CATCH") && !(next == "IF" && self.punct(x + 2, b'(')) {
                        if matches!(next.as_str(), "TRY" | "CATCH") {
                            stack.pop();
                        }
                        x += 2;
                        continue;
                    }
                    stack.pop();
                    if next == "CASE" {
                        x += 1;
                    }
                    if stack.is_empty() {
                        return x + 1;
                    }
                }
                _ => {}
            }
            x += 1;
        }
        self.facts.parse_errors += 1;
        n
    }

    fn reference(&mut self, from: Option<usize>, name: &Name, kind: RefKind, line: u32) {
        let Some(last) = name.last().filter(|n| !n.is_empty() && !n.starts_with('#')) else { return };
        let receiver = (name.len() > 1).then(|| name[name.len() - 2].to_lowercase()).filter(|r| !r.is_empty());
        let r = Ref { from, name: last.to_lowercase(), receiver, receiver_type: None, kind, line, col: 0, args: None };
        if !self.facts.refs.iter().rev().take(64).any(|x| x.from == r.from && x.name == r.name && x.kind == r.kind && x.receiver == r.receiver && x.line == r.line) {
            self.facts.refs.push(r);
        }
    }

    /// The references in a body (`a..b`) of symbol `sym`.
    fn body(&mut self, sym: usize, a: usize, b: usize) {
        let (ctes, temps) = self.locals(a, b);
        let mut found: Vec<(Name, RefKind, u32)> = Vec::new();
        self.walk(a, b, &mut found);
        // SQL built and run inside the body: its tables and calls aren't read, and the body says so.
        for x in a..b {
            if let Some(construct) = self.dynamic_exec(x) {
                let line = self.toks[x].line;
                if !self.facts.unsupported.iter().any(|u| u.line == line) {
                    self.facts.unsupported.push(Unsupported { area: "sql", construct: construct.to_string(), line });
                }
            }
        }
        for (n, kind, line) in found {
            let last = n.last().map(|l| l.to_lowercase()).unwrap_or_default();
            // A temporary table is never a real one, whatever schema it's created in (`CREATE TEMPORARY TABLE db.t`).
            if (n.len() == 1 && ctes.contains(&last)) || temps.contains(&last) {
                continue;
            }
            self.reference(Some(sym), &n, kind, line);
        }
    }

    /// Dynamic SQL starting at token `x`, in words: T-SQL `EXEC (@sql)`, `EXEC @sql`, `sp_executesql`; MySQL
    /// `PREPARE s FROM @q`.
    fn dynamic_exec(&self, x: usize) -> Option<&'static str> {
        let exec = self.is(x, "EXEC") || self.is(x, "EXECUTE");
        if self.tsql && exec && self.punct(x + 1, b'(') {
            return Some("dynamic SQL run by EXEC (…)");
        }
        if self.tsql && exec && self.toks.get(x + 1).is_some_and(|t| t.t == T::Var) && !self.punct(x + 2, b'=') {
            return Some("dynamic SQL run by EXEC @variable");
        }
        if self.upper(x) == "SP_EXECUTESQL" {
            return Some("dynamic SQL run by sp_executesql");
        }
        if !self.tsql && self.is(x, "PREPARE") && self.is(x + 2, "FROM") {
            return Some("dynamic SQL run by PREPARE … FROM");
        }
        None
    }

    /// Names in `a..b` that aren't tables, lower case: common table expressions, and temporary tables (by their last
    /// part, qualified or not).
    fn locals(&self, a: usize, b: usize) -> (Vec<String>, Vec<String>) {
        let mut local: Vec<String> = Vec::new();
        let mut temps: Vec<String> = Vec::new();
        for x in a..b {
            if (self.is(x, "WITH") || self.punct(x, b',')) && self.is(x + 2, "AS") && self.punct(x + 3, b'(')
                || self.is(x, "WITH") && self.is(x + 1, "RECURSIVE") && self.is(x + 3, "AS")
            {
                let at = if self.is(x + 1, "RECURSIVE") { x + 2 } else { x + 1 };
                if let Some((n, _)) = self.name(at) {
                    local.push(n.last().unwrap().to_lowercase());
                }
            }
            if self.is(x, "TEMPORARY") && self.is(x + 1, "TABLE") {
                let mut at = x + 2;
                if self.is(at, "IF") {
                    at += if self.is(at + 1, "NOT") { 3 } else { 2 };
                }
                if let Some((n, _)) = self.name(at) {
                    temps.push(n.last().unwrap().to_lowercase());
                }
            }
        }
        (local, temps)
    }

    /// Table lists (`FROM a x, b JOIN c ON …`): each table with its alias; ends at the next clause. Subqueries in
    /// it, and in its `ON` conditions, are walked for their own references.
    fn table_list(&self, mut k: usize, b: usize, found: &mut Vec<(Name, RefKind, u32)>) -> (Vec<(Name, Option<String>, u32)>, usize) {
        let mut out = Vec::new();
        loop {
            while JOIN_WORDS.contains(&self.upper(k).as_str()) {
                k += 1;
            }
            if self.punct(k, b'(') {
                let close = self.close(k, b);
                let mut inner = k + 1;
                while self.punct(inner, b'(') {
                    inner += 1;
                }
                if matches!(self.upper(inner).as_str(), "SELECT" | "WITH" | "VALUES") {
                    self.walk(k + 1, close, found);
                } else {
                    // A parenthesized join: `FROM ((a x LEFT JOIN b y ON …) JOIN c …)`.
                    out.extend(self.table_list(k + 1, close, found).0);
                }
                k = close + 1;
            } else if let Some((n, next)) = self.name(k) {
                let line = self.toks[k].line;
                k = next;
                out.push((n, None, line));
            } else {
                break;
            }
            // An alias: `t x`, `t AS x`.
            if self.is(k, "AS") {
                k += 1;
            }
            if let Some(t) = self.toks.get(k)
                && k < b
                && matches!(t.t, T::Word | T::Quoted)
                && (t.t == T::Quoted || !(CLAUSES.contains(&self.upper(k).as_str()) || JOIN_WORDS.contains(&self.upper(k).as_str()) || NOT_NAMES.contains(&self.upper(k).as_str())))
            {
                if let Some(last) = out.last_mut() {
                    last.1 = Some(self.text(k).to_lowercase());
                }
                k += 1;
            }
            // Index hints: `USE INDEX (…)`, `FORCE INDEX (…)`, T-SQL `WITH (NOLOCK)`.
            while matches!(self.upper(k).as_str(), "USE" | "FORCE" | "IGNORE") && matches!(self.upper(k + 1).as_str(), "INDEX" | "KEY") {
                k += 2;
                if matches!(self.upper(k).as_str(), "FOR") {
                    k += 2;
                    if self.is(k, "BY") {
                        k += 1;
                    }
                }
                if self.punct(k, b'(') {
                    k = self.close(k, b) + 1;
                }
            }
            if self.is(k, "WITH") && self.punct(k + 1, b'(') {
                k = self.close(k + 1, b) + 1;
            }
            // A join condition, up to the next join or clause.
            if self.is(k, "ON") || (self.is(k, "USING") && self.punct(k + 1, b'(')) {
                k += 1;
                while k < b && !self.ends(k) {
                    let u = self.upper(k);
                    if JOIN_WORDS.contains(&u.as_str()) || self.punct(k, b',') || (CLAUSES.contains(&u.as_str()) && u != "ON") || self.punct(k, b')') {
                        break;
                    }
                    if self.punct(k, b'(') {
                        let close = self.close(k, b);
                        self.walk(k + 1, close, found);
                        k = close + 1;
                        continue;
                    }
                    k += 1;
                }
            }
            if self.punct(k, b',') || JOIN_WORDS.contains(&self.upper(k).as_str()) {
                if self.punct(k, b',') {
                    k += 1;
                }
                continue;
            }
            break;
        }
        (out, k)
    }

    /// The references between `a` and `b`, into `found`.
    fn walk(&self, a: usize, b: usize, found: &mut Vec<(Name, RefKind, u32)>) {
        let mut k = a;
        while k < b {
            let t = self.toks[k];
            if !matches!(t.t, T::Word | T::Quoted) {
                k += 1;
                continue;
            }
            let u = self.upper(k);
            let prev = if k > a { self.upper(k - 1) } else { String::new() };
            let line = t.line;
            match u.as_str() {
                "INSERT" | "REPLACE" if !self.punct(k + 1, b'(') => {
                    let mut m = k + 1;
                    while matches!(self.upper(m).as_str(), "LOW_PRIORITY" | "DELAYED" | "HIGH_PRIORITY" | "IGNORE" | "INTO") {
                        m += 1;
                    }
                    if let Some((n, next)) = self.name(m) {
                        found.push((n, RefKind::Write, self.toks[m].line));
                        k = next;
                        continue;
                    }
                }
                "UPDATE" if prev != "KEY" && prev != "FOR" && !self.is(k + 1, "STATISTICS") && !self.punct(k + 1, b'(') => {
                    let mut m = k + 1;
                    while matches!(self.upper(m).as_str(), "LOW_PRIORITY" | "IGNORE") {
                        m += 1;
                    }
                    let (tables, next) = self.table_list(m, b, found);
                    if !tables.is_empty() {
                        let set_end = self.find_top(next, b, &["WHERE", "ORDER", "LIMIT"]).unwrap_or(b);
                        // T-SQL: `UPDATE x SET … FROM t x JOIN …`: the target is an alias the FROM declares.
                        let from = self.find_top(next, set_end, &["FROM"]);
                        let (mut aliased, mut skip_to) = (Vec::new(), next);
                        if let Some(f) = from {
                            (aliased, skip_to) = self.table_list(f + 1, b, found);
                        }
                        let assigned = self.set_qualifiers(next, from.unwrap_or(set_end));
                        let written = |name: &Name, alias: &Option<String>| -> bool {
                            let last = name.last().map(|s| s.to_lowercase()).unwrap_or_default();
                            assigned.iter().any(|q| Some(q) == alias.as_ref() || *q == last)
                        };
                        let any_qualified = !assigned.is_empty();
                        for (i, (n, alias, l)) in tables.iter().enumerate() {
                            if tables.len() == 1 && from.is_some() {
                                // The target names (or aliases) one of the FROM's tables.
                                let target = n.last().map(|s| s.to_lowercase()).unwrap_or_default();
                                match aliased.iter().find(|(_, a, _)| a.as_deref() == Some(target.as_str())) {
                                    Some((real, _, rl)) => found.push((real.clone(), RefKind::Write, *rl)),
                                    None => found.push((n.clone(), RefKind::Write, *l)),
                                }
                                continue;
                            }
                            let write = if tables.len() == 1 || !any_qualified { i == 0 } else { written(n, alias) };
                            found.push((n.clone(), if write { RefKind::Write } else { RefKind::Read }, *l));
                        }
                        for (n, alias, l) in &aliased {
                            let target = tables[0].0.last().map(|s| s.to_lowercase()).unwrap_or_default();
                            if tables.len() == 1 && alias.as_deref() == Some(target.as_str()) {
                                continue;
                            }
                            found.push((n.clone(), RefKind::Read, *l));
                        }
                        k = skip_to.max(next);
                        continue;
                    }
                }
                "DELETE" => {
                    let mut m = k + 1;
                    while matches!(self.upper(m).as_str(), "LOW_PRIORITY" | "QUICK" | "IGNORE" | "TOP") {
                        m += 1;
                        if self.punct(m, b'(') {
                            m = self.close(m, b) + 1;
                        }
                    }
                    if self.is(m, "FROM") {
                        // `DELETE FROM t …`, `DELETE FROM t1, t2 USING t1 JOIN t2 …`
                        let (targets, next) = self.table_list(m + 1, b, found);
                        let names: Vec<String> = targets.iter().filter_map(|(n, _, _)| n.last().map(|s| s.to_lowercase())).collect();
                        for (n, _, l) in &targets {
                            found.push((n.clone(), RefKind::Write, *l));
                        }
                        let mut k2 = next;
                        if self.is(next, "USING") {
                            let (used, after) = self.table_list(next + 1, b, found);
                            for (n, _, l) in used {
                                if !n.last().is_some_and(|x| names.contains(&x.to_lowercase())) {
                                    found.push((n, RefKind::Read, l));
                                }
                            }
                            k2 = after;
                        } else if self.is(next, "FROM") {
                            // T-SQL: `DELETE FROM x FROM t x JOIN …`
                            let (from, after) = self.table_list(next + 1, b, found);
                            self.alias_targets(&targets, from, found);
                            k2 = after;
                        }
                        k = k2.max(k + 1);
                        continue;
                    }
                    // `DELETE t1, x FROM t1 JOIN t2 x …`
                    let mut targets = Vec::new();
                    let mut x = m;
                    while let Some((n, next)) = self.name(x) {
                        targets.push((n, None, self.toks[x].line));
                        x = next;
                        if self.punct(x, b'.') && self.punct(x + 1, b'*') {
                            x += 2;
                        }
                        if self.punct(x, b',') {
                            x += 1;
                        } else {
                            break;
                        }
                    }
                    if self.is(x, "FROM") && !targets.is_empty() {
                        let (from, after) = self.table_list(x + 1, b, found);
                        self.alias_targets(&targets, from, found);
                        k = after.max(k + 1);
                        continue;
                    }
                }
                "TRUNCATE" if !self.punct(k + 1, b'(') => {
                    let m = if self.is(k + 1, "TABLE") { k + 2 } else { k + 1 };
                    if let Some((n, next)) = self.name(m) {
                        found.push((n, RefKind::Write, self.toks[m].line));
                        k = next;
                        continue;
                    }
                }
                "MERGE" => {
                    let m = if self.is(k + 1, "INTO") { k + 2 } else { k + 1 };
                    if let Some((n, next)) = self.name(m) {
                        found.push((n, RefKind::Write, self.toks[m].line));
                        if let Some(using) = (next..b).find(|&x| self.is(x, "USING")) {
                            let (used, after) = self.table_list(using + 1, b, found);
                            for (n, _, l) in used {
                                found.push((n, RefKind::Read, l));
                            }
                            k = after;
                        } else {
                            k = next;
                        }
                        continue;
                    }
                }
                "FROM" | "JOIN" => {
                    let (tables, next) = self.table_list(k + 1, b, found);
                    for (n, _, l) in tables {
                        found.push((n, RefKind::Read, l));
                    }
                    k = next.max(k + 1);
                    continue;
                }
                "CALL" => {
                    if let Some((n, next)) = self.name(k + 1) {
                        found.push((n, RefKind::Call, line));
                        k = next;
                        continue;
                    }
                }
                // T-SQL `EXEC sp_x`, `EXEC @rc = sp_x`; in MySQL, `EXECUTE stmt` runs a prepared statement.
                "EXEC" | "EXECUTE" if self.tsql && !self.punct(k + 1, b'(') => {
                    let mut m = k + 1;
                    if self.toks.get(m).is_some_and(|t| t.t == T::Var) && self.punct(m + 1, b'=') {
                        m += 2;
                    }
                    if self.toks.get(m).is_some_and(|t| t.t != T::Var)
                        && let Some((n, next)) = self.name(m)
                    {
                        found.push((n, RefKind::Call, line));
                        k = next;
                        continue;
                    }
                }
                // `DROP TABLE a, b` and `RENAME TABLE a TO b, c TO d` (swapping a table for a rebuilt copy) change
                // the tables: writes. `DROP TEMPORARY TABLE` drops none.
                "DROP" | "RENAME" if self.is(k + 1, "TABLE") => {
                    let mut m = k + 2;
                    if self.is(m, "IF") && self.is(m + 1, "EXISTS") {
                        m += 2;
                    }
                    while let Some((n, next)) = self.name(m) {
                        found.push((n, RefKind::Write, self.toks[m].line));
                        m = next;
                        if self.punct(m, b',') || (u == "RENAME" && self.is(m, "TO")) {
                            m += 1;
                        } else {
                            break;
                        }
                    }
                    k = m.max(k + 2);
                    continue;
                }
                "CREATE" | "ALTER" | "DROP" => {
                    // DDL in a body: skip the object's name and columns, but read what `CREATE … AS SELECT` reads.
                    let end = (k..b).find(|&x| self.ends(x)).unwrap_or(b);
                    if let Some(select) = self.find_top(k, end, &["SELECT"]) {
                        self.walk(select, end, found);
                    }
                    k = end;
                    continue;
                }
                _ if FROM_FUNCTIONS.contains(&u.as_str()) && self.punct(k + 1, b'(') => {
                    // `TRIM(LEADING 'x' FROM col)`: the arguments around the `FROM`, which names no table.
                    let close = self.close(k + 1, b);
                    match self.find_top(k + 2, close, &["FROM"]) {
                        Some(from) => {
                            self.walk(k + 2, from, found);
                            self.walk(from + 2, close, found);
                        }
                        None => self.walk(k + 2, close, found),
                    }
                    k = close + 1;
                    continue;
                }
                _ => {
                    // A function call: `f(…)`, `db.f(…)`, unless a builtin or keyword.
                    if let Some((n, next)) = self.name(k)
                        && self.punct(next, b'(')
                        && !self.punct(k.wrapping_sub(1), b'.')
                        && (t.t == T::Quoted || n.len() > 1 || !is_builtin(&u))
                        && !(n.len() > 1 && is_builtin(&n.last().unwrap().to_ascii_uppercase()))
                        && !matches!(prev.as_str(), "INDEX" | "KEY" | "TABLE" | "INTO" | "REFERENCES" | "PROCEDURE" | "FUNCTION" | "OPEN")
                    {
                        found.push((n, RefKind::Call, line));
                        k = next;
                        continue;
                    }
                }
            }
            k += 1;
        }
    }

    /// The first of `words` between `a` and `b` outside parentheses, before the statement ends.
    fn find_top(&self, a: usize, b: usize, words: &[&str]) -> Option<usize> {
        let mut depth = 0;
        for x in a..b {
            if self.punct(x, b'(') {
                depth += 1;
            } else if self.punct(x, b')') {
                depth -= 1;
            } else if depth == 0 && self.ends(x) {
                return None;
            } else if depth == 0 && words.contains(&self.upper(x).as_str()) {
                return Some(x);
            }
        }
        None
    }

    /// For a delete whose targets (`DELETE x FROM t x …`) name aliases or tables of its FROM list: writes for the
    /// targets, reads for the rest.
    fn alias_targets(&self, targets: &[(Name, Option<String>, u32)], from: Vec<(Name, Option<String>, u32)>, found: &mut Vec<(Name, RefKind, u32)>) {
        let wanted: Vec<String> = targets.iter().filter_map(|(n, _, _)| n.last().map(|s| s.to_lowercase())).collect();
        let mut hit = vec![false; wanted.len()];
        for (n, alias, l) in from {
            let last = n.last().map(|s| s.to_lowercase()).unwrap_or_default();
            let i = wanted.iter().position(|w| Some(w) == alias.as_ref() || *w == last);
            match i {
                Some(i) => {
                    hit[i] = true;
                    found.push((n, RefKind::Write, l));
                }
                None => found.push((n, RefKind::Read, l)),
            }
        }
        for (i, (n, _, l)) in targets.iter().enumerate() {
            if !hit[i] {
                found.push((n.clone(), RefKind::Write, *l));
            }
        }
    }

    /// The qualifiers of the columns a `SET` clause (between `a` and `b`) assigns: `x` for `x.col = …`.
    fn set_qualifiers(&self, a: usize, b: usize) -> Vec<String> {
        let Some(set) = (a..b).find(|&x| self.is(x, "SET")) else { return Vec::new() };
        let mut out = Vec::new();
        let mut depth = 0;
        let mut item_start = true;
        for x in set + 1..b {
            if self.punct(x, b'(') {
                depth += 1;
            } else if self.punct(x, b')') {
                depth -= 1;
            } else if depth == 0 && self.punct(x, b',') {
                item_start = true;
                continue;
            } else if depth == 0 && item_start {
                if self.punct(x + 1, b'.') && matches!(self.toks[x].t, T::Word | T::Quoted) {
                    let q = self.text(x).to_lowercase();
                    if !out.contains(&q) {
                        out.push(q);
                    }
                }
                item_start = false;
            }
        }
        out
    }
}

/// A column of a table or view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Column {
    pub name: String,
    /// The declared type with its options (`int(11) NOT NULL AUTO_INCREMENT`), for a table; empty for a view.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub decl: String,
    /// The line, relative to the statement's first.
    pub line: u32,
}

/// The columns of a `CREATE TABLE` or `CREATE VIEW` statement's text: a table's column definitions, a view's column
/// list or the names its `SELECT` gives its results.
pub fn columns(statement: &str) -> Vec<Column> {
    let (toks, _, tsql) = tokenize(statement);
    let s = Scanner { src: statement, toks, facts: FileFacts::default(), schema: None, tsql };
    let Some(k) = (0..s.toks.len()).find(|&k| s.is(k, "TABLE") || s.is(k, "VIEW")) else { return Vec::new() };
    let view = s.is(k, "VIEW");
    let mut j = k + 1;
    if s.is(j, "IF") {
        j += 3;
    }
    let Some((_, after)) = s.name(j) else { return Vec::new() };
    let end = s.toks.len();
    if s.punct(after, b'(') {
        let close = s.close(after, end);
        return if view {
            (after + 1..close).filter(|&x| matches!(s.toks[x].t, T::Word | T::Quoted)).map(|x| column(&s, x, String::new())).collect()
        } else {
            columns_in(&s, after + 1, close).into_iter().map(|(x, decl)| column(&s, x, decl)).collect()
        };
    }
    if !view {
        return Vec::new();
    }
    // A view's select list: each item's alias, or the column it names.
    let Some(select) = (after..end).find(|&x| s.is(x, "SELECT")) else { return Vec::new() };
    let mut out = Vec::new();
    let mut depth = 0;
    let mut last: Option<usize> = None;
    let mut x = select + 1;
    while x < end {
        if s.punct(x, b'(') {
            depth += 1;
        } else if s.punct(x, b')') {
            depth -= 1;
        } else if depth == 0 && (s.punct(x, b',') || s.is(x, "FROM") || s.ends(x)) {
            if let Some(l) = last.take() {
                out.push(column(&s, l, String::new()));
            }
            if !s.punct(x, b',') {
                break;
            }
        } else if depth == 0 && matches!(s.toks[x].t, T::Word | T::Quoted) && !s.is(x, "AS") && !s.is(x, "DISTINCT") {
            last = Some(x);
        }
        x += 1;
    }
    if let Some(l) = last {
        out.push(column(&s, l, String::new()));
    }
    out
}

fn column(s: &Scanner, x: usize, decl: String) -> Column {
    Column { name: s.text(x).to_string(), decl, line: s.toks[x].line - 1 }
}

/// Column definitions between `a` and `b` (inside `CREATE TABLE t (…)`): each column's token and declaration.
fn columns_in(s: &Scanner, a: usize, b: usize) -> Vec<(usize, String)> {
    const NOT_COLUMNS: &[&str] = &[
        "PRIMARY", "KEY", "INDEX", "UNIQUE", "CONSTRAINT", "FOREIGN", "FULLTEXT", "SPATIAL", "CHECK", "PERIOD", "CLUSTERED",
    ];
    let mut out = Vec::new();
    let mut depth = 0;
    let mut item = a;
    for x in a..=b {
        let boundary = x == b || (depth == 0 && s.punct(x, b','));
        if !boundary {
            if s.punct(x, b'(') {
                depth += 1;
            } else if s.punct(x, b')') {
                depth -= 1;
            }
            continue;
        }
        if item < x
            && matches!(s.toks[item].t, T::Word | T::Quoted)
            && !(s.toks[item].t == T::Word && NOT_COLUMNS.contains(&s.upper(item).as_str()))
        {
            let decl = if item + 1 < x { collapse(&s.src[s.toks[item + 1].outer().0..s.toks[x - 1].outer().1], 120) } else { String::new() };
            out.push((item, decl));
        }
        item = x + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syms(f: &FileFacts) -> Vec<(String, &'static str, Option<String>)> {
        f.symbols.iter().map(|s| (s.name.clone(), s.kind.as_str(), s.container.clone())).collect()
    }

    /// (from symbol, kind, receiver, name) for each reference.
    fn refs(f: &FileFacts) -> Vec<(Option<String>, &'static str, Option<String>, String)> {
        f.refs
            .iter()
            .map(|r| (r.from.map(|i| f.symbols[i].name.clone()), r.kind.as_str(), r.receiver.clone(), r.name.clone()))
            .collect()
    }

    fn of(f: &FileFacts, sym: &str) -> Vec<(&'static str, String)> {
        let mut v: Vec<(&'static str, String)> = f
            .refs
            .iter()
            .filter(|r| r.from.is_some_and(|i| f.symbols[i].name == sym))
            .map(|r| (r.kind.as_str(), r.receiver.as_ref().map_or_else(|| r.name.clone(), |s| format!("{s}.{}", r.name))))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    fn s(kind: &'static str, name: &str) -> (&'static str, String) {
        (kind, name.to_string())
    }

    const DUMP: &str = "-- MySQL dump 10.13\n--\n-- Host: db1    Database: shop\n-- -----\n\
/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;\n\
DROP TABLE IF EXISTS `orders`;\n\
CREATE TABLE `orders` (\n  `order_id` int(11) NOT NULL AUTO_INCREMENT,\n  `customer_id` int(11) NOT NULL,\n  `note` varchar(500) DEFAULT 'a;b',\n  PRIMARY KEY (`order_id`),\n  CONSTRAINT `fk` FOREIGN KEY (`customer_id`) REFERENCES `customers` (`id`)\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\
/*!50003 SET sql_mode = '' */ ;\n\
DELIMITER ;;\n\
CREATE DEFINER=`root`@`%` PROCEDURE `place_order`(p_customer INT, p_note varchar(500))\n\
BEGIN\n  DECLARE v INT;\n  SELECT count(*) INTO v FROM customers c WHERE c.id = p_customer;\n  IF v > 0 THEN\n    INSERT INTO orders (customer_id, note) VALUES (p_customer, strip_note(p_note));\n    CALL log_event('order');\n  END IF;\nEND ;;\n\
DELIMITER ;;\n\
/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`%`*/ /*!50003 TRIGGER `orders_ins` AFTER INSERT ON `orders` FOR EACH ROW BEGIN\n\
INSERT INTO order_history (order_id) VALUES (NEW.order_id);\nEND */;;\n\
DELIMITER ;\n";

    #[test]
    fn a_dump_gives_tables_routines_and_triggers_in_its_database() {
        let f = extract(DUMP);
        assert_eq!(f.parse_errors, 0);
        assert_eq!(
            syms(&f),
            [
                ("orders".to_string(), "table", Some("shop".to_string())),
                ("place_order".to_string(), "procedure", Some("shop".to_string())),
                ("orders_ins".to_string(), "trigger", Some("shop".to_string())),
            ]
        );
        assert_eq!(f.symbols[0].signature, "CREATE TABLE `orders` (3 columns)");
        assert_eq!((f.symbols[1].start_line, f.symbols[1].end_line), (16, 24));
        assert!(DUMP[f.symbols[1].start_byte as usize..f.symbols[1].end_byte as usize].ends_with("END"));
        assert_eq!(of(&f, "orders"), [s("type_ref", "customers")]);
        assert_eq!(
            of(&f, "place_order"),
            [s("call", "log_event"), s("call", "strip_note"), s("read", "customers"), s("write", "orders")]
        );
        assert_eq!(of(&f, "orders_ins"), [s("type_ref", "orders"), s("write", "order_history")]);
        assert_eq!(f.symbols[1].complexity, 2);
    }

    #[test]
    fn columns_come_from_the_statement_text() {
        let f = extract(DUMP);
        let t = &f.symbols[0];
        let cols = columns(&DUMP[t.start_byte as usize..t.end_byte as usize]);
        let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["order_id", "customer_id", "note"]);
        assert_eq!(cols[0].decl, "int(11) NOT NULL AUTO_INCREMENT");
        assert_eq!(cols[1].line, 2);
        let view = columns("CREATE VIEW v AS SELECT o.id, o.note AS remark, count(*) AS n FROM orders o");
        assert_eq!(view.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "remark", "n"]);
        let listed = columns("CREATE VIEW v (a, b) AS SELECT 1, 2");
        assert_eq!(listed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn statements_read_and_write_what_they_name() {
        let src = "CREATE PROCEDURE p()\nBEGIN\n\
  UPDATE a JOIN b ON b.id = a.b_id SET b.x = 1 WHERE a.y = 2;\n\
  UPDATE c SET z = (SELECT max(z) FROM d);\n\
  DELETE e FROM e JOIN f ON f.id = e.f_id;\n\
  DELETE FROM g WHERE id IN (SELECT id FROM h);\n\
  DELETE FROM i USING i JOIN j ON j.id = i.j_id;\n\
  INSERT IGNORE INTO k SELECT * FROM l LEFT OUTER JOIN m USING (id), n;\n\
  REPLACE INTO o VALUES (REPLACE('a', 'b', 'c'));\n\
  INSERT INTO p2 (x) VALUES (1) ON DUPLICATE KEY UPDATE x = 2;\n\
  SELECT EXTRACT(YEAR FROM created) INTO @y FROM q FOR UPDATE;\n\
  TRUNCATE TABLE r;\n\
  WITH cte AS (SELECT * FROM s) SELECT * FROM cte;\n\
  CREATE TEMPORARY TABLE tmp (id INT);\n\
  INSERT INTO tmp SELECT id FROM other.t;\n\
  SELECT `db`.`f`(1), db.g(2), concat('x', 'y') FROM dual;\n\
END;\nSELECT 1;";
        let f = extract(src);
        assert_eq!(f.parse_errors, 0);
        assert_eq!(f.symbols.len(), 1);
        assert_eq!(f.symbols[0].end_line, 17);
        assert_eq!(
            of(&f, "p"),
            [
                s("call", "db.f"),
                s("call", "db.g"),
                s("read", "a"),
                s("read", "d"),
                s("read", "f"),
                s("read", "h"),
                s("read", "j"),
                s("read", "l"),
                s("read", "m"),
                s("read", "n"),
                s("read", "other.t"),
                s("read", "q"),
                s("read", "s"),
                s("write", "b"),
                s("write", "c"),
                s("write", "e"),
                s("write", "g"),
                s("write", "i"),
                s("write", "k"),
                s("write", "o"),
                s("write", "p2"),
                s("write", "r"),
            ]
        );
    }

    #[test]
    fn function_arguments_cursors_and_temporary_selects() {
        let src = "CREATE FUNCTION tidy(n VARCHAR(9)) RETURNS varchar(9)\nRETURN TRIM(IF(LENGTH(nth_word(n, 1)) > 0, nth_word(n, 1), n));\n\
CREATE PROCEDURE p()\nBEGIN\n  DECLARE cur CURSOR(a INT) FOR SELECT x FROM src WHERE id = a;\n  OPEN cur(1);\n\
  CREATE TEMPORARY TABLE days AS SELECT 1 AS d FROM information_schema.columns LIMIT 5;\n\
  SELECT TRIM(LEADING 'x' FROM name) FROM names;\n\
  SELECT 1 FROM ((ja t LEFT JOIN jb u ON t.id = u.id) JOIN jc v ON v.id = t.id);\nEND;\n";
        let f = extract(src);
        assert_eq!(f.parse_errors, 0);
        assert_eq!(of(&f, "tidy"), [s("call", "nth_word")]);
        assert_eq!(of(&f, "p"), [
                s("read", "information_schema.columns"),
                s("read", "ja"),
                s("read", "jb"),
                s("read", "jc"),
                s("read", "names"),
                s("read", "src"),
            ]);
    }

    #[test]
    fn tsql_batches_brackets_and_exec() {
        let src = "USE [Shop]\nGO\nCREATE PROCEDURE [dbo].[usp_Place] @Id INT AS\nBEGIN\n  SET NOCOUNT ON;\n  UPDATE o SET o.Status = 1 FROM [dbo].[Orders] o JOIN dbo.Lines l ON l.OrderId = o.Id WHERE o.Id = @Id;\n  EXEC dbo.usp_Log @Id;\n  EXEC @rc = usp_Other;\n  MERGE INTO Stock AS s USING Lines AS l ON s.Id = l.Id WHEN MATCHED THEN UPDATE SET s.Qty = l.Qty;\nEND\nGO\nCREATE VIEW dbo.vOrders AS SELECT * FROM dbo.Orders;\nGO\nALTER TABLE [dbo].[Lines] ADD CONSTRAINT fk FOREIGN KEY (OrderId) REFERENCES [dbo].[Orders] (Id);\nGO\n";
        let f = extract(src);
        assert_eq!(f.parse_errors, 0);
        assert_eq!(
            syms(&f),
            [("usp_Place".to_string(), "procedure", Some("dbo".to_string())), ("vOrders".to_string(), "view", Some("dbo".to_string()))]
        );
        assert_eq!(
            of(&f, "usp_Place"),
            [
                s("call", "dbo.usp_log"),
                s("call", "usp_other"),
                s("read", "dbo.lines"),
                s("read", "lines"),
                s("write", "dbo.orders"),
                s("write", "stock"),
            ]
        );
        assert_eq!(of(&f, "vOrders"), [s("read", "dbo.orders")]);
        let top: Vec<_> = refs(&f).into_iter().filter(|r| r.0.is_none()).collect();
        assert_eq!(
            top,
            [(None, "type_ref", Some("dbo".to_string()), "lines".to_string()), (None, "type_ref", Some("dbo".to_string()), "orders".to_string())]
        );
    }

    #[test]
    fn migrations_without_a_delimiter_change_end_at_the_block() {
        let src = "USE billing;\nCREATE TABLE IF NOT EXISTS invoice (id INT PRIMARY KEY, total DECIMAL(10,2));\n\
CREATE FUNCTION fx(a INT) RETURNS INT DETERMINISTIC\nBEGIN\n  DECLARE r INT;\n  CASE a WHEN 1 THEN SET r = 1; ELSE SET r = 2; END CASE;\n  SET r = CASE WHEN a > 1 THEN 1 ELSE 0 END;\n  RETURN (SELECT count(*) FROM invoice);\nEND;\n\
CREATE OR REPLACE VIEW billing.open_invoices AS SELECT * FROM invoice WHERE total > 0;\n\
ALTER TABLE invoice ADD COLUMN paid BIT;\nCREATE INDEX ix ON invoice (total);\n";
        let f = extract(src);
        assert_eq!(f.parse_errors, 0);
        assert_eq!(
            syms(&f),
            [
                ("invoice".to_string(), "table", Some("billing".to_string())),
                ("fx".to_string(), "function", Some("billing".to_string())),
                ("open_invoices".to_string(), "view", Some("billing".to_string())),
            ]
        );
        assert_eq!(f.symbols[1].end_line, 9);
        assert_eq!(of(&f, "fx"), [s("read", "invoice")]);
        assert_eq!(of(&f, "open_invoices"), [s("read", "invoice")]);
        assert_eq!(f.refs.iter().filter(|r| r.from.is_none() && r.kind == RefKind::TypeRef).count(), 2);
    }

    #[test]
    fn a_dumped_view_and_malformed_index_lines() {
        let src = "CREATE OR REPLACE VIEW tokens\\n AS \\nselect `shop`.`token`.`id` AS `id` from `shop`.`token` where (`shop`.`token`.`ok` = 1);\n\
ALTER TABLE account_note` ADD INDEX `idx_a` USING BTREE( `account_id` );\n\
ALTER TABLE account_flag` ADD PRIMARY KEY USING BTREE( `id` );\n\
ALTER TABLE ``.`orders`  ADD CONSTRAINT `fk1` FOREIGN KEY (`customer_id`) REFERENCES `customers` ( `id`) ;\n";
        let f = extract(src);
        assert_eq!(syms(&f), [("tokens".to_string(), "view", None)]);
        assert_eq!(of(&f, "tokens"), [s("read", "shop.token")]);
        let top: Vec<String> = f.refs.iter().filter(|r| r.from.is_none()).map(|r| r.name.clone()).collect();
        assert_eq!(top, ["account_note", "account_flag", "orders", "customers"]);
        assert!(f.parse_errors > 0, "the unpaired quotes are counted");
    }

    #[test]
    fn dump_view_stand_ins_are_not_views() {
        let src = "-- Host: db1    Database: shop\n\
--\n-- Temporary view structure for view `open_orders`\n--\n\n\
/*!50001 DROP VIEW IF EXISTS `open_orders`*/;\nSET @saved_cs_client     = @@character_set_client;\n\
/*!50001 CREATE VIEW `open_orders` AS SELECT \n 1 AS `id`,\n 1 AS `total`*/;\nSET character_set_client = @saved_cs_client;\n\
--\n-- Temporary table structure for view `old_orders`\n--\n\nDROP TABLE IF EXISTS `old_orders`;\n\
/*!50001 DROP VIEW IF EXISTS `old_orders`*/;\n/*!50001 CREATE TABLE `old_orders` (\n  `id` tinyint NOT NULL\n) ENGINE=MyISAM */;\n\
CREATE TABLE `orders` (`id` int) ENGINE=BLACKHOLE;\n\
CREATE VIEW `constants` AS SELECT 1 AS `one` FROM dual;\n\
--\n-- Final view structure for view `open_orders`\n--\n\n\
/*!50001 CREATE ALGORITHM=UNDEFINED */\n/*!50001 VIEW `open_orders` AS select `orders`.`id` AS `id` from `orders` */;\n";
        let f = extract(src);
        let names: Vec<(String, &str)> = f.symbols.iter().map(|s| (s.name.clone(), s.kind.as_str())).collect();
        assert_eq!(
            names,
            [("orders".to_string(), "table"), ("constants".to_string(), "view"), ("open_orders".to_string(), "view")]
        );
        assert_eq!(f.symbols[0].tags, ["blackhole"]);
        assert_eq!(of(&f, "open_orders"), [s("read", "orders")]);
    }

    #[test]
    fn a_dump_parts_file_name_gives_its_schema() {
        assert_eq!(file_schema("dumps/shop.Views.sql").as_deref(), Some("shop"));
        assert_eq!(file_schema("shop_2.procs.SQL").as_deref(), Some("shop_2"));
        assert_eq!(file_schema("dumps/shop.sql"), None);
        assert_eq!(file_schema("migrations/001.create.sql"), None);
        assert_eq!(file_schema("a/b.c.Views.sql"), None);
        let f = extract_at("dumps/shop.Views.sql", "CREATE OR REPLACE VIEW v AS SELECT * FROM orders;\n");
        assert_eq!(syms(&f), [("v".to_string(), "view", Some("shop".to_string()))]);
        // A header or `USE` still wins.
        let f = extract_at("dumps/shop.Views.sql", "USE other;\nCREATE VIEW v AS SELECT 1 AS a FROM t;\n");
        assert_eq!(f.symbols[0].container.as_deref(), Some("other"));
    }

    #[test]
    fn temporary_tables_drops_and_renames() {
        let src = "CREATE PROCEDURE p()\nBEGIN\n\
  CREATE TEMPORARY TABLE shop.tmp_orders (id INT);\n\
  INSERT INTO shop.tmp_orders SELECT id FROM orders;\n\
  UPDATE tmp_orders SET id = 1;\n\
  CREATE TABLE orders_new LIKE orders;\n\
  RENAME TABLE orders TO orders_old, orders_new TO orders;\n\
  DROP TABLE IF EXISTS orders_old, archive.stale;\n\
  DROP TEMPORARY TABLE shop.tmp_orders;\n\
END;\n";
        let f = extract(src);
        assert_eq!(f.parse_errors, 0);
        assert_eq!(
            of(&f, "p"),
            [s("read", "orders"), s("write", "archive.stale"), s("write", "orders"), s("write", "orders_new"), s("write", "orders_old")]
        );
        let e: Vec<(String, &str)> = embedded("RENAME TABLE a TO b").into_iter().map(|(n, k, _)| (n, k.as_str())).collect();
        assert_eq!(e, [("a".to_string(), "write"), ("b".to_string(), "write")]);
    }
}

