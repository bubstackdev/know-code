//! ColdFusion (CFML) extraction over tree-sitter-cfml: tag-based templates and components with the `cfml` grammar,
//! and CFScript (`<cfscript>` blocks, script components, `.cfs` files) with its `cfscript` grammar.
//!
//! Every file is one symbol: a component (`.cfc`, a class named after the file) or a template (`.cfm`, `.cfs`).
//! Functions (`<cffunction>`, script `function`) are methods of the component or free functions of the template,
//! with their access and arguments in the signature. CFML is case-insensitive, so reference names are lower case;
//! resolution matches definitions case-insensitively.
//!
//! References carry what resolution needs in `receiver_type`, since CFML names files, not types:
//! `include:<path>` for `<cfinclude>`, `include`, and `<cfmodule template>`; `tag:<name>` for a custom tag
//! (`<cf_x>`, `<cfmodule name>`); `component:<dotted path>` for `createObject("component", …)`, `new a.b.C()`,
//! `<cfobject>`, `<cfinvoke component>` and calls on variables holding one; `java:<class>` for Java objects. Paths are
//! resolved at query time against the repo's files and `this.mappings` (see `kc_store::cfml`).
//!
//! Also extracted: tables read and written by `<cfquery>` and `queryExecute` SQL (`sql_read` / `sql_write` db facts,
//! query-of-queries left out), stored procedures, `<cfhttp>` requests as client routes, and string constants that
//! look like URLs or paths (`<cfset request.api.x = "https://…">`, names lower-cased), for resolving those routes.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::swift::{children, collapse};
use crate::urls::{self, Piece};
use crate::{DbFact, FileFacts, HttpRoute, Ref, RefKind, Relation, RelationKind, StringConst, Symbol, SymbolKind};

pub const VERSION: u32 = 3;

const SIGNATURE_CAP: usize = 240;
const RECEIVER_CAP: usize = 48;

/// Prefix of a `this.mappings` entry stored as a [`StringConst`]: `cfml.mapping:/model` → `app/model`.
pub const MAPPING_PREFIX: &str = "cfml.mapping:";

/// Whether a path is a CFML source file.
pub fn is_cfml_path(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, e)| ["cfm", "cfc", "cfml", "cfs"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Built-in functions, lower case: calls to them are not references to anything in the repo, and they make up most of
/// the calls in a template. A user-defined function can't take a built-in's name.
const BUILTINS: &[&str] = &[
    "abs", "acos", "arrayappend", "arrayavg", "arrayclear", "arraycontains", "arraycontainsnocase", "arraydelete",
    "arraydeleteat", "arraydeletenocase", "arrayeach", "arrayevery", "arrayfilter", "arrayfind", "arrayfindall",
    "arrayfindnocase", "arrayfirst", "arrayindexexists", "arrayinsertat", "arrayisdefined", "arrayisempty", "arraylast",
    "arraylen", "arraymap", "arraymax", "arraymerge", "arraymin", "arraynew", "arrayprepend", "arrayreduce",
    "arrayremoveduplicates", "arrayresize", "arrayreverse", "arrayset", "arrayslice", "arraysome", "arraysort",
    "arraysum", "arrayswap", "arraytolist", "arraytostruct", "asc", "asin", "atn", "binarydecode", "binaryencode",
    "bitand", "bitnot", "bitor", "bitshln", "bitshrn", "bitxor", "cacheclear", "cachedelete", "cacheget",
    "cachekeyexists", "cacheput", "cacheremove", "callstackget", "canonicalize", "ceiling", "charsetdecode",
    "charsetencode", "chr", "cjustify", "compare", "comparenocase", "cos", "createdate", "createdatetime", "createguid",
    "createobject", "createodbcdate", "createodbcdatetime", "createodbctime", "createtime", "createtimespan",
    "createuuid", "dateadd", "datecompare", "dateconvert", "datediff", "dateformat", "datepart", "datetimeformat",
    "day", "dayofweek", "dayofweekasstring", "dayofyear", "daysinmonth", "daysinyear", "de", "decimalformat",
    "decodeforhtml", "decodefrombase64", "decrementvalue", "decrypt", "decryptbinary", "deleteclientvariable",
    "deserializejson", "directorycopy", "directorycreate", "directorydelete", "directoryexists", "directorylist",
    "directoryrename", "dollarformat", "dump", "duplicate", "echo", "encodeforcss", "encodeforhtml",
    "encodeforhtmlattribute", "encodeforjavascript", "encodeforurl", "encodeforxml", "encodeforxmlattribute", "encrypt",
    "encryptbinary", "evaluate", "exp", "expandpath", "fileappend", "fileclose", "filecopy", "filedelete", "fileexists",
    "filegetmimetype", "fileiseof", "filemove", "fileopen", "fileread", "filereadbinary", "filereadline", "fileseek",
    "filesetaccessmode", "filesetattribute", "filesetlastmodified", "fileskipbytes", "fileupload", "filewrite",
    "filewriteline", "find", "findnocase", "findoneof", "firstdayofmonth", "fix", "formatbasen", "generatepbkdfkey",
    "generatesecretkey", "getapplicationmetadata", "getapplicationsettings", "getauthuser", "getbasetagdata",
    "getbasetaglist", "getbasetemplatepath", "getclientvariableslist", "getcomponentmetadata", "getcontextroot",
    "getcpuusage", "getcurrentcontext", "getcurrenttemplatepath", "getdirectoryfrompath", "getencoding", "getexception",
    "getfilefrompath", "getfileinfo", "getfunctioncalledname", "getfunctionlist", "gethttprequestdata",
    "gethttptimestring", "getlocale", "getlocaledisplayname", "getmetadata", "getmetricdata", "getpagecontext",
    "getprofilesections", "getprofilestring", "getsystemfreememory", "getsystemtotalmemory", "gettempdirectory",
    "gettempfile", "gettickcount", "gettimezone", "gettimezoneinfo", "gettoken", "getvariable", "hash", "hmac", "hour",
    "htmlcodeformat", "htmleditformat", "httpencode", "iif", "imagecaptcha", "imagecopy", "imagecrop", "imagedrawline",
    "imagedrawpoint", "imagedrawrect", "imagedrawtext", "imagegetblob", "imagegetexifmetadata", "imagegetheight",
    "imagegetwidth", "imageinfo", "imagenew", "imagepaste", "imageread", "imageresize", "imagerotate",
    "imagescaletofit", "imagesetantialiasing", "imagesetdrawingcolor", "imagewrite", "imagewritetobrowser",
    "incrementvalue", "inputbasen", "insert", "int", "invoke", "isarray", "isbinary", "isboolean", "isclosure",
    "iscustomfunction", "isdate", "isdebugmode", "isdefined", "isempty", "ishtml", "isinstanceof", "isipinrange",
    "isipv6", "isjson", "isleapyear", "islocalhost", "isnull", "isnumeric", "isnumericdate", "isobject", "isquery",
    "issimplevalue", "isstruct", "isuserinrole", "isuserloggedin", "isvalid", "isxml", "isxmldoc", "isxmlelem",
    "isxmlnode", "isxmlroot", "javacast", "jsstringformat", "lcase", "left", "len", "listappend", "listavg",
    "listchangedelims", "listcompact", "listcontains", "listcontainsnocase", "listdeleteat", "listeach", "listevery",
    "listfilter", "listfind", "listfindnocase", "listfirst", "listgetat", "listinsertat", "listitemtrim", "listlast",
    "listlen", "listmap", "listprepend", "listqualify", "listreduce", "listremoveduplicates", "listrest", "listsetat",
    "listsome", "listsort", "listtoarray", "listtrim", "listvaluecount", "listvaluecountnocase", "ljustify", "location",
    "log", "log10", "lscurrencyformat", "lsdateformat", "lsdatetimeformat", "lseurocurrencyformat", "lsiscurrency",
    "lsisdate", "lsisnumeric", "lsnumberformat", "lsparsecurrency", "lsparsedatetime", "lsparsenumber", "lstimeformat",
    "ltrim", "max", "mid", "min", "minute", "month", "monthasstring", "now", "nullvalue", "numberformat",
    "pagepoolclear", "paragraphformat", "parsedatetime", "pi", "precisionevaluate", "preservesinglequotes", "quarter",
    "queryaddcolumn", "queryaddrow", "querycolumnarray", "querycolumnexists", "querycolumnlist", "queryconvertforgrid",
    "querydeleterow", "queryexecute", "queryfilter", "querygetcell", "querygetrow", "querykeyexists", "querymap",
    "querynew", "queryrecordcount", "querysetcell", "queryslice", "querysort", "quotedvaluelist", "rand", "randomize",
    "randrange", "reescape", "refind", "refindnocase", "rematch", "rematchnocase", "removechars", "repeatstring",
    "replace", "replacelist", "replacenocase", "rereplace", "rereplacenocase", "reverse", "right", "rjustify", "round",
    "rtrim", "second", "sendgatewaymessage", "serialize", "serializejson", "sessioninvalidate", "sessionrotate",
    "setencoding", "setlocale", "setprofilestring", "settimezone", "setvariable", "sgn", "sin", "sleep",
    "spanexcluding", "spanincluding", "sqr", "stringeach", "stringfilter", "stringmap", "stringreduce", "stripcr",
    "structappend", "structclear", "structcopy", "structcount", "structdelete", "structeach", "structevery",
    "structfilter", "structfind", "structfindkey", "structfindvalue", "structget", "structinsert", "structisempty",
    "structkeyarray", "structkeyexists", "structkeylist", "structkeytranslate", "structmap", "structnew",
    "structreduce", "structsome", "structsort", "structtosorted", "structupdate", "systemoutput", "tan", "threadjoin",
    "threadterminate", "throw", "timeformat", "tobase64", "tobinary", "tonumeric", "toscript", "tostring", "trim",
    "ucase", "ucfirst", "urldecode", "urlencode", "urlencodedformat", "val", "valuearray", "valuelist", "week", "wrap",
    "writedump", "writelog", "writeoutput", "xmlchildpos", "xmlelemnew", "xmlformat", "xmlgetnodetype", "xmlnew",
    "xmlparse", "xmlsearch", "xmltransform", "xmlvalidate", "year", "yesnoformat",
];

/// Tags called as functions in CFScript (`cfhttp(url = …)`), handled like the tag.
const TAG_FUNCTIONS: &[&str] = &["cfhttp", "cfinclude", "cfmodule", "cfinvoke", "cfobject", "cfstoredproc"];

/// Argument and property types that are not components.
const PLAIN_TYPES: &[&str] = &[
    "any", "array", "binary", "boolean", "component", "date", "email", "function", "guid", "numeric", "query",
    "string", "struct", "uuid", "variablename", "void", "xml", "regex", "creditcard", "eurodate", "integer", "float",
    "ssn", "telephone", "url", "usdate", "zipcode", "object",
];

pub fn extract(path: &str, source: &str) -> FileFacts {
    let file = path.rsplit('/').next().unwrap_or(path);
    let (stem, ext) = file.rsplit_once('.').unwrap_or((file, ""));
    let is_cfc = ext.eq_ignore_ascii_case("cfc");
    let script_file = ext.eq_ignore_ascii_case("cfs");
    let name = if stem.is_empty() { file } else { stem };
    let mut cx = Cx {
        path,
        component: is_cfc.then(|| name.to_string()),
        facts: FileFacts::default(),
        scopes: Vec::new(),
        seen: HashSet::new(),
        script: None,
        tags: None,
    };
    let lines = source.lines().count().max(1) as u32;
    cx.facts.symbols.push(Symbol {
        parent: None,
        name: name.to_string(),
        kind: if is_cfc { SymbolKind::Class } else { SymbolKind::Template },
        container: None,
        signature: if is_cfc { format!("component {name}") } else { format!("template {path}") },
        doc: None,
        attrs: Vec::new(),
        tags: Vec::new(),
        start_line: 1,
        end_line: lines,
        start_byte: 0,
        end_byte: source.len() as u32,
        complexity: 1,
        nesting: 0,
        bumps: Vec::new(),
        cond_ops: 0,
        cond_line: 0,
        type_name: None,
        arity: None,
        lambda_types: None,
    });
    cx.scopes.push(Scope::new(0));
    // A component written in CFScript, or a `.cfs` file, is script throughout.
    let script = script_file || (is_cfc && !source.to_ascii_lowercase().contains("<cfcomponent"));
    let masked = mask_comments(source, script);
    let source = masked.as_deref().unwrap_or(source);
    let tx = Tx { src: source, byte: 0, row: 0, file: source };
    if script_file {
        cx.script_tree(tx);
    } else {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_cfml::LANGUAGE_CFML.into()).expect("tree-sitter-cfml ABI matches tree-sitter");
        match parser.parse(source, None) {
            Some(tree) => cx.visit(tx, tree.root_node()),
            None => cx.facts.parse_errors += 1,
        }
    }
    let top = cx.scopes.pop().expect("file scope");
    cx.facts.symbols[0].complexity = 1 + top.branches;
    cx.facts.symbols[0].nesting = top.max_depth;
    cx.facts
}

/// The text a tree was parsed from, and where it sits in the file.
#[derive(Clone, Copy)]
struct Tx<'s> {
    src: &'s str,
    byte: usize,
    row: u32,
    /// The whole file, for columns.
    file: &'s str,
}

impl<'s> Tx<'s> {
    fn text(&self, n: Node) -> &'s str {
        &self.src[n.byte_range()]
    }
    fn line(&self, n: Node) -> u32 {
        n.start_position().row as u32 + self.row + 1
    }
    /// Where `n` starts in the file: its line and 1-based byte column.
    fn at(&self, n: Node) -> (u32, u32) {
        let b = (self.byte + n.start_byte()).min(self.file.len());
        let start = self.file.as_bytes()[..b].iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
        (self.line(n), (b - start) as u32 + 1)
    }
    fn end_line(&self, n: Node) -> u32 {
        n.end_position().row as u32 + self.row + 1
    }
    /// The part of the text `n` spans, as a tree of its own.
    fn inner(&self, n: Node) -> Tx<'s> {
        Tx { src: self.text(n), byte: self.byte + n.start_byte(), row: self.row + n.start_position().row as u32, file: self.file }
    }
}

/// What a variable is known to hold.
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    /// A component, by its dotted path as written.
    Component(String),
    /// A Java object, by class.
    Java(String),
}

impl Ty {
    fn encode(&self) -> String {
        match self {
            Ty::Component(c) => format!("component:{c}"),
            Ty::Java(c) => format!("java:{c}"),
        }
    }
}

struct Scope {
    sym: usize,
    branches: u32,
    depth: u32,
    max_depth: u32,
    /// Variables by normalized name (see [`var_key`]).
    vars: HashMap<String, Ty>,
}

impl Scope {
    fn new(sym: usize) -> Scope {
        Scope { sym, branches: 0, depth: 0, max_depth: 0, vars: HashMap::new() }
    }
}

struct Cx<'a> {
    path: &'a str,
    /// The component's name, in a `.cfc`.
    component: Option<String>,
    facts: FileFacts,
    scopes: Vec<Scope>,
    seen: HashSet<(Option<usize>, String, RefKind, u32, u32)>,
    script: Option<Parser>,
    /// For query bodies (see [`Cx::query_expressions`]).
    tags: Option<Parser>,
}

/// A tag attribute or a named argument: lower-case name and value node.
type Attr<'t> = (String, Node<'t>);

impl<'a> Cx<'a> {
    fn sym(&self) -> usize {
        self.scopes.last().expect("scope").sym
    }

    fn branch(&mut self) {
        self.scopes.last_mut().expect("scope").branches += 1;
    }

    fn enter_nest(&mut self) {
        let s = self.scopes.last_mut().expect("scope");
        s.depth += 1;
        s.max_depth = s.max_depth.max(s.depth);
    }

    fn leave_nest(&mut self) {
        let s = self.scopes.last_mut().expect("scope");
        s.depth = s.depth.saturating_sub(1);
    }

    /// A reference written at `at` (line, column).
    fn add_ref(&mut self, name: &str, kind: RefKind, receiver: Option<String>, receiver_type: Option<String>, at: (u32, u32)) {
        let (line, col) = at;
        let name = name.to_ascii_lowercase();
        if name.is_empty() {
            return;
        }
        let from = Some(self.sym());
        if self.seen.insert((from, name.clone(), kind, line, col)) {
            self.facts.refs.push(Ref { from, name, receiver, receiver_type, kind, line, col, args: None });
        }
    }

    fn lookup(&self, key: &str) -> Option<Ty> {
        let local = self.scopes.last().and_then(|s| s.vars.get(key));
        local.or_else(|| self.scopes.first().and_then(|s| s.vars.get(key))).cloned()
    }

    /// Records that `target` (as written) holds `ty`: in the function's scope for `var` and `local.` names and
    /// arguments, else in the file's (`variables`, `this`, and unscoped names, which CFML puts in `variables`).
    fn bind(&mut self, target: &str, declared_local: bool, ty: Ty) {
        let (key, local) = var_key(target);
        let scope = if (declared_local || local) && self.scopes.len() > 1 { self.scopes.len() - 1 } else { 0 };
        self.scopes[scope].vars.insert(key, ty);
    }

    // ---- The tag-based tree ----

    fn visit(&mut self, tx: Tx, node: Node) {
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        match node.kind() {
            "html_text" | "comment" | "cf_comment" | "string_fragment" | "attribute_value" => return,
            "component_file" => {
                for c in children(node) {
                    if c.kind() == "cf_component_content" {
                        self.script_tree(tx.inner(c));
                    } else {
                        self.visit(tx, c);
                    }
                }
                return;
            }
            "cf_script_tag" => {
                for c in children(node) {
                    if c.kind() == "cf_script_content" {
                        self.script_tree(tx.inner(c));
                    }
                }
                return;
            }
            "cf_component_open_tag" => {
                let attrs = tag_attrs(tx, node);
                self.component_header(tx, node, &attrs);
            }
            "cf_function_tag" => return self.function_tag(tx, node),
            "cf_query_tag" => return self.query_tag(tx, node),
            "cf_set_tag" => {
                let declared_local = children(node).any(|c| c.kind() == "cf_var");
                for c in children(node) {
                    if c.kind() == "assignment_expression" {
                        self.assignment(tx, c, declared_local);
                    }
                }
            }
            "cf_if_tag" => {
                self.branch();
                self.enter_nest();
                self.visit_children(tx, node);
                self.leave_nest();
                return;
            }
            "cf_elseif_tag" => self.branch(),
            "cf_selfclose_tag" | "cf_tag" | "cf_start_tag_with_selfclose" => {
                if let Some(name) = tag_name(tx, node) {
                    let attrs = tag_attrs(tx, node);
                    let nests = matches!(name.as_str(), "loop" | "switch" | "try");
                    if matches!(name.as_str(), "loop" | "case" | "catch") {
                        self.branch();
                    }
                    self.tag(tx, node, &name, &attrs);
                    if nests {
                        self.enter_nest();
                        self.visit_children(tx, node);
                        self.leave_nest();
                        return;
                    }
                }
            }
            _ => {
                if self.expression(tx, node) {
                    return;
                }
            }
        }
        self.visit_children(tx, node);
    }

    fn visit_children(&mut self, tx: Tx, node: Node) {
        for c in children(node) {
            self.visit(tx, c);
        }
    }

    /// `<cfcomponent extends=… implements=…>` or `component extends=… {`.
    fn component_header(&mut self, tx: Tx, node: Node, attrs: &[Attr]) {
        let line = tx.line(node);
        for (name, value) in attrs {
            if !matches!(name.as_str(), "extends" | "implements") {
                continue;
            }
            let Some(v) = static_value(tx, *value) else { continue };
            for target in v.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                self.facts.relations.push(Relation { from: 0, target: target.to_string(), kind: RelationKind::Inherits, line });
            }
        }
        let head = tx.text(node);
        let head = head.split('{').next().unwrap_or(head);
        self.facts.symbols[0].signature = collapse(head, SIGNATURE_CAP);
        if let Some(hint) = attr_value(tx, attrs, "hint") {
            self.facts.symbols[0].doc = Some(collapse(&hint, 120));
        } else if node.kind() == "component" {
            self.facts.symbols[0].doc = doc_comment(tx, node);
        }
    }

    fn function_tag(&mut self, tx: Tx, node: Node) {
        let attrs = tag_attrs(tx, node);
        let Some(name) = attr_value(tx, &attrs, "name") else { return self.visit_children(tx, node) };
        let mut args = Vec::new();
        let mut typed = Vec::new();
        for c in children(node) {
            if c.kind() == "cf_selfclose_tag" && tag_name(tx, c).as_deref() == Some("argument") {
                let a = tag_attrs(tx, c);
                let Some(arg) = attr_value(tx, &a, "name") else { continue };
                let ty = attr_value(tx, &a, "type");
                let required = attr_value(tx, &a, "required").is_some_and(|r| matches!(r.to_ascii_lowercase().as_str(), "yes" | "true"));
                let mut s = String::new();
                if required {
                    s.push_str("required ");
                }
                if let Some(t) = &ty {
                    s.push_str(t);
                    s.push(' ');
                }
                s.push_str(&arg);
                args.push(s);
                if let Some(t) = ty.filter(|t| is_component_type(t)) {
                    typed.push((arg, t));
                }
            }
        }
        let access = attr_value(tx, &attrs, "access");
        let returns = attr_value(tx, &attrs, "returntype");
        let mut sig = String::new();
        for part in [access.as_deref(), returns.as_deref()].into_iter().flatten() {
            sig.push_str(part);
            sig.push(' ');
        }
        sig.push_str(&format!("function {name}({})", args.join(", ")));
        let doc = attr_value(tx, &attrs, "hint");
        let idx = self.push_function(tx, node, &name, collapse(&sig, SIGNATURE_CAP), access, doc);
        self.scopes.push(Scope::new(idx));
        for (arg, t) in typed {
            self.bind(&arg, true, Ty::Component(t));
        }
        self.visit_children(tx, node);
        self.pop_function(idx);
    }

    fn push_function(&mut self, tx: Tx, node: Node, name: &str, signature: String, access: Option<String>, doc: Option<String>) -> usize {
        let parent = self.sym();
        let (kind, container) = match &self.component {
            Some(c) => (SymbolKind::Method, Some(c.clone())),
            None => (SymbolKind::Function, None),
        };
        self.facts.symbols.push(Symbol {
            parent: Some(parent),
            name: name.to_string(),
            kind,
            container,
            signature,
            doc,
            attrs: access.map(|a| a.to_ascii_lowercase()).into_iter().collect(),
            tags: Vec::new(),
            start_line: tx.line(node),
            end_line: tx.end_line(node),
            start_byte: (tx.byte + node.start_byte()) as u32,
            end_byte: (tx.byte + node.end_byte()) as u32,
            complexity: 1,
            nesting: 0,
            bumps: Vec::new(),
            cond_ops: 0,
            cond_line: 0,
            type_name: None,
            arity: None,
            lambda_types: None,
        });
        self.facts.symbols.len() - 1
    }

    fn pop_function(&mut self, idx: usize) {
        let s = self.scopes.pop().expect("function scope");
        self.facts.symbols[idx].complexity = 1 + s.branches;
        self.facts.symbols[idx].nesting = s.max_depth;
    }

    fn query_tag(&mut self, tx: Tx, node: Node) {
        let attrs = tag_attrs(tx, node);
        let of_queries = attr_value(tx, &attrs, "dbtype").is_some_and(|d| d.eq_ignore_ascii_case("query"));
        for c in children(node) {
            match c.kind() {
                "cf_query_content" => {
                    if !of_queries {
                        self.sql(tx.text(c), tx.line(c));
                    }
                    self.query_expressions(tx, c);
                }
                "cf_attribute" => self.visit(tx, c),
                _ => {}
            }
        }
    }

    /// The `#…#` values and CFML tags in a query body (`'#getIP()#'`, `<cfqueryparam value="#left(x, 50)#">`,
    /// `<cfif isNew(x)>`), which the grammar leaves as raw text: the body is parsed again as the inside of a
    /// `<cfoutput>`, where `#…#` is an expression. Its parse errors aren't the file's.
    fn query_expressions(&mut self, tx: Tx, content: Node) {
        const OPEN: &str = "<cfoutput>";
        let text = tx.text(content);
        if !text.contains('#') && !text.to_ascii_lowercase().contains("<cf") {
            return;
        }
        let wrapped = format!("{OPEN}{}</cfoutput>", literal_hashes(text));
        let parser = self.tags.get_or_insert_with(|| {
            let mut p = Parser::new();
            p.set_language(&tree_sitter_cfml::LANGUAGE_CFML.into()).expect("tree-sitter-cfml ABI matches tree-sitter");
            p
        });
        let Some(tree) = parser.parse(&wrapped, None) else { return };
        let inner = Tx {
            src: &wrapped,
            byte: (tx.byte + content.start_byte()).saturating_sub(OPEN.len()),
            row: tx.row + content.start_position().row as u32,
            file: tx.file,
        };
        let errors = self.facts.parse_errors;
        self.visit(inner, tree.root_node());
        self.facts.parse_errors = errors;
    }

    /// Records the tables and procedures a SQL text starting at `line` uses.
    fn sql(&mut self, text: &str, line: u32) {
        let sym = Some(self.sym());
        for (name, kind, offset) in query_tables(text) {
            self.facts.db.push(DbFact { kind, sym, entity: String::new(), name, line: line + offset });
        }
    }

    /// A CFML tag by its lower-case name without `cf`, or a script tag statement or tag function.
    fn tag(&mut self, tx: Tx, node: Node, name: &str, attrs: &[Attr]) {
        let line = tx.line(node);
        let at = tx.at(node);
        match name {
            "include" => {
                if let Some(t) = attr_value(tx, attrs, "template") {
                    self.include(&t, at);
                }
            }
            "module" => {
                if let Some(t) = attr_value(tx, attrs, "template") {
                    self.include(&t, at);
                } else if let Some(n) = attr_value(tx, attrs, "name") {
                    self.custom_tag(&n, at);
                }
            }
            "invoke" => self.invoke(tx, attrs, at),
            "object" => {
                let kind = attr_value(tx, attrs, "type").unwrap_or_else(|| "component".into()).to_ascii_lowercase();
                let var = attr_value(tx, attrs, "name");
                let ty = match kind.as_str() {
                    "component" => attr_value(tx, attrs, "component").map(Ty::Component),
                    "java" => attr_value(tx, attrs, "class").map(Ty::Java),
                    _ => None,
                };
                if let Some(Ty::Component(c)) = &ty {
                    self.add_ref(last_segment(c), RefKind::Instantiate, None, Some(format!("component:{c}")), at);
                }
                if let (Some(v), Some(t)) = (var, ty) {
                    self.bind(&v, false, t);
                }
            }
            "http" => self.http(tx, attrs, line),
            "storedproc" => {
                if let Some(p) = attr_value(tx, attrs, "procedure") {
                    let sym = Some(self.sym());
                    self.facts.db.push(DbFact { kind: "procedure", sym, entity: String::new(), name: p, line });
                }
            }
            "property" => self.property(tx, node, attrs),
            _ => {
                if let Some(custom) = name.strip_prefix('_') {
                    self.custom_tag(custom, at);
                }
            }
        }
    }

    fn include(&mut self, template: &str, at: (u32, u32)) {
        if template.contains('#') {
            return;
        }
        let file = template.rsplit(['/', '\\']).next().unwrap_or(template);
        let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
        self.add_ref(stem, RefKind::Call, None, Some(format!("include:{template}")), at);
    }

    fn custom_tag(&mut self, name: &str, at: (u32, u32)) {
        if name.contains('#') {
            return;
        }
        self.add_ref(last_segment(name), RefKind::Call, None, Some(format!("tag:{name}")), at);
    }

    /// `<cfinvoke component="a.b.C" method="m">`, `component="#obj#"`, or no component: the caller's own.
    fn invoke(&mut self, tx: Tx, attrs: &[Attr], at: (u32, u32)) {
        let Some(method) = attr_value(tx, attrs, "method") else { return };
        let component = attrs.iter().find(|(n, _)| n == "component").map(|(_, v)| *v);
        match component {
            None => self.add_ref(&method, RefKind::Call, None, None, at),
            Some(v) => match static_value(tx, v) {
                Some(c) => {
                    self.add_ref(last_segment(&c), RefKind::Instantiate, None, Some(format!("component:{c}")), at);
                    self.add_ref(&method, RefKind::Call, Some(collapse(&c, RECEIVER_CAP)), Some(format!("component:{c}")), at);
                }
                None => {
                    let Some(expr) = hash_inner(v) else { return };
                    let receiver = match expr.kind() {
                        "this" => "this".to_string(),
                        _ => collapse(tx.text(expr), RECEIVER_CAP),
                    };
                    let ty = self.ty_of(tx, expr).map(|t| t.encode());
                    self.add_ref(&method, RefKind::Call, Some(receiver), ty, at);
                }
            },
        }
    }

    fn http(&mut self, tx: Tx, attrs: &[Attr], line: u32) {
        let Some((_, url)) = attrs.iter().find(|(n, _)| n == "url") else { return };
        let mut pieces = pieces(tx, *url);
        // `#server#:443/x`: the port belongs to the base address.
        if let [Piece::Ref(_) | Piece::Unknown, Piece::Text(t), ..] = pieces.as_mut_slice()
            && let Some(rest) = t.strip_prefix(':')
        {
            *t = rest.trim_start_matches(|c: char| c.is_ascii_digit()).to_string();
        }
        let Some(path) = urls::encode(&pieces, true) else { return };
        if !urls::looks_like_path(&path) {
            return;
        }
        let verb = attr_value(tx, attrs, "method").and_then(|m| urls::verb(&m)).unwrap_or_else(|| "GET".to_string());
        let sym = self.sym();
        self.facts.http_routes.push(HttpRoute { sym: Some(sym), side: "client", verb, path, path_ref: None, service: None, service_ref: None, line });
    }

    /// `<cfproperty name="dao" type="model.Dao">` or script `property name="dao" type="model.Dao";`.
    fn property(&mut self, tx: Tx, node: Node, attrs: &[Attr]) {
        let Some(name) = attr_value(tx, attrs, "name") else { return };
        let ty = attr_value(tx, attrs, "type").or_else(|| attr_value(tx, attrs, "component")).filter(|t| is_component_type(t));
        self.facts.symbols.push(Symbol {
            parent: Some(0),
            name: name.clone(),
            kind: SymbolKind::Property,
            container: self.component.clone(),
            signature: collapse(tx.text(node), SIGNATURE_CAP),
            doc: None,
            attrs: Vec::new(),
            tags: Vec::new(),
            start_line: tx.line(node),
            end_line: tx.end_line(node),
            start_byte: (tx.byte + node.start_byte()) as u32,
            end_byte: (tx.byte + node.end_byte()) as u32,
            complexity: 0,
            nesting: 0,
            bumps: Vec::new(),
            cond_ops: 0,
            cond_line: 0,
            type_name: ty.clone(),
            arity: None,
            lambda_types: None,
        });
        if let Some(t) = ty {
            self.bind(&name, false, Ty::Component(t));
        }
    }

    /// `x = expr`: what `x` holds, and the constant it names when `expr` is a URL-like string.
    fn assignment(&mut self, tx: Tx, node: Node, declared_local: bool) {
        let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else { return };
        self.bind_value(tx, left, right, declared_local);
    }

    fn bind_value(&mut self, tx: Tx, left: Node, right: Node, declared_local: bool) {
        if let Some(target) = dotted(tx, left) {
            if let Some(ty) = self.ty_of(tx, right) {
                self.bind(&target, declared_local, ty);
            } else if let Some(value) = static_value(tx, right).filter(|v| looks_like_url(v)) {
                self.facts.constants.push(StringConst { qualified: target.to_ascii_lowercase(), value, line: tx.line(left) });
            }
        } else if left.kind() == "subscript_expression" {
            // `this.mappings["/model"] = expandPath("./model")`.
            let object = left.child_by_field_name("object").map(|o| tx.text(o).to_ascii_lowercase());
            if object.as_deref() == Some("this.mappings")
                && let Some(index) = left.child_by_field_name("index").and_then(|i| static_value(tx, i))
                && let Some(dir) = self.mapping_dir(tx, right)
            {
                let logical = format!("/{}", index.trim_matches(['/', '\\']));
                self.facts.constants.push(StringConst { qualified: format!("{MAPPING_PREFIX}{logical}"), value: dir, line: tx.line(left) });
            }
        }
    }

    /// The repo-relative directory a mapping's value names: `expandPath("/x")` from the web root (taken to be the repo
    /// root), `expandPath("./x")` or `getDirectoryFromPath(getCurrentTemplatePath()) & "x"` from this file's directory.
    fn mapping_dir(&self, tx: Tx, value: Node) -> Option<String> {
        let here = self.path.rsplit_once('/').map_or("", |(d, _)| d);
        let rel = match value.kind() {
            "call_expression" => {
                let f = value.child_by_field_name("function")?;
                if !tx.text(f).eq_ignore_ascii_case("expandpath") {
                    return None;
                }
                let arg = value.child_by_field_name("arguments").and_then(|a| children(a).find(|c| c.is_named()))?;
                static_value(tx, arg)?
            }
            "binary_expression" => {
                let left = value.child_by_field_name("left")?;
                if !tx.text(left).to_ascii_lowercase().replace(' ', "").contains("getcurrenttemplatepath") {
                    return None;
                }
                static_value(tx, value.child_by_field_name("right")?)?
            }
            _ => return None,
        };
        let rel = rel.replace('\\', "/");
        let joined = if let Some(abs) = rel.strip_prefix('/') { abs.to_string() } else { format!("{here}/{rel}") };
        Some(normalize_path(&joined))
    }

    /// What an expression is known to hold.
    fn ty_of(&self, tx: Tx, node: Node) -> Option<Ty> {
        match node.kind() {
            "call_expression" => {
                let f = node.child_by_field_name("function")?;
                match f.kind() {
                    "identifier" if tx.text(f).eq_ignore_ascii_case("createobject") => create_object(tx, node),
                    // `createObject(…).init(…)`: `init` returns the component.
                    "member_expression" => {
                        let prop = f.child_by_field_name("property")?;
                        if tx.text(prop).eq_ignore_ascii_case("init") { self.ty_of(tx, f.child_by_field_name("object")?) } else { None }
                    }
                    _ => None,
                }
            }
            "new_expression" => {
                let c = node.child_by_field_name("constructor")?;
                dotted(tx, c).filter(|d| !d.eq_ignore_ascii_case("java")).map(Ty::Component)
            }
            "identifier" | "member_expression" => self.lookup(&var_key(&dotted(tx, node)?).0),
            "parenthesized_expression" => self.ty_of(tx, children(node).find(|c| c.is_named())?),
            _ => None,
        }
    }

    /// References in an expression node. Returns whether it handled the node's children too.
    fn expression(&mut self, tx: Tx, node: Node) -> bool {
        match node.kind() {
            "call_expression" => {
                let Some(f) = node.child_by_field_name("function") else { return false };
                match f.kind() {
                    "identifier" => {
                        let name = tx.text(f).to_ascii_lowercase();
                        if name == "createobject" {
                            if let Some(Ty::Component(c)) = create_object(tx, node) {
                                self.add_ref(last_segment(&c), RefKind::Instantiate, None, Some(format!("component:{c}")), tx.at(node));
                            }
                        } else if TAG_FUNCTIONS.contains(&name.as_str()) {
                            let attrs = named_args(tx, node);
                            self.tag(tx, node, &name[2..], &attrs);
                        } else if !BUILTINS.contains(&name.as_str()) {
                            self.add_ref(&name, RefKind::Call, None, None, tx.at(f));
                        }
                    }
                    "member_expression" => {
                        let (Some(object), Some(prop)) = (f.child_by_field_name("object"), f.child_by_field_name("property")) else {
                            return false;
                        };
                        let receiver = match object.kind() {
                            "this" => "this".to_string(),
                            "super" => "super".to_string(),
                            _ => collapse(tx.text(object), RECEIVER_CAP),
                        };
                        let ty = self.ty_of(tx, object).map(|t| t.encode());
                        self.add_ref(tx.text(prop), RefKind::Call, Some(receiver), ty, tx.at(prop));
                    }
                    _ => {}
                }
                false
            }
            "new_expression" => {
                if let Some(Ty::Component(c)) = self.ty_of(tx, node) {
                    self.add_ref(last_segment(&c), RefKind::Instantiate, None, Some(format!("component:{c}")), tx.at(node));
                }
                false
            }
            "binary_expression" => {
                if let Some(op) = node.child_by_field_name("operator")
                    && matches!(tx.text(op).to_ascii_lowercase().as_str(), "and" | "or" | "&&" | "||")
                {
                    self.branch();
                }
                false
            }
            "ternary_expression" | "elvis_expression" => {
                self.branch();
                false
            }
            "assignment_expression" => {
                self.assignment(tx, node, false);
                false
            }
            _ => false,
        }
    }

    // ---- CFScript ----

    fn script_tree(&mut self, tx: Tx) {
        let parser = self.script.get_or_insert_with(|| {
            let mut p = Parser::new();
            p.set_language(&tree_sitter_cfml::LANGUAGE_CFSCRIPT.into()).expect("tree-sitter-cfml ABI matches tree-sitter");
            p
        });
        match parser.parse(tx.src, None) {
            Some(tree) => self.script_visit(tx, tree.root_node()),
            None => self.facts.parse_errors += 1,
        }
    }

    fn script_visit(&mut self, tx: Tx, node: Node) {
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        match node.kind() {
            "comment" | "string_fragment" | "query_text" => return,
            "component" => {
                let attrs = component_attrs(tx, node);
                self.component_header(tx, node, &attrs);
            }
            "function_declaration" => return self.script_function(tx, node),
            "property_declaration" => {
                let attrs = component_attrs(tx, node);
                self.property(tx, node, &attrs);
                return;
            }
            "include_statement" => {
                if let Some(t) = children(node).find(|c| c.is_named()).and_then(|c| static_value(tx, c)) {
                    self.include(&t, tx.at(node));
                }
                return;
            }
            "tag_statement" => {
                if let Some(t) = node.child_by_field_name("tag") {
                    let name = tx.text(t).to_ascii_lowercase();
                    let name = name.strip_prefix("cf").unwrap_or(&name).to_string();
                    let attrs = named_args(tx, node);
                    self.tag(tx, node, &name, &attrs);
                }
            }
            "query_expression" => {
                if let Some(q) = children(node).find(|c| c.kind() == "query_text") {
                    self.sql(tx.text(q), tx.line(q));
                }
            }
            "variable_declaration" => {
                for d in children(node).filter(|c| c.kind() == "variable_declarator") {
                    if let (Some(n), Some(v)) = (d.child_by_field_name("name"), d.child_by_field_name("value")) {
                        self.bind_value(tx, n, v, true);
                    }
                }
            }
            "if_statement" => {
                self.branch();
                let else_if = node.parent().is_some_and(|p| p.kind() == "else_clause");
                if !else_if {
                    self.enter_nest();
                }
                self.script_children(tx, node);
                if !else_if {
                    self.leave_nest();
                }
                return;
            }
            "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
                self.branch();
                self.enter_nest();
                self.script_children(tx, node);
                self.leave_nest();
                return;
            }
            "switch_statement" | "try_statement" => {
                self.enter_nest();
                self.script_children(tx, node);
                self.leave_nest();
                return;
            }
            "switch_case" | "catch_clause" => self.branch(),
            _ => {
                if self.expression(tx, node) {
                    return;
                }
            }
        }
        self.script_children(tx, node);
    }

    fn script_children(&mut self, tx: Tx, node: Node) {
        for c in children(node) {
            self.script_visit(tx, c);
        }
    }

    fn script_function(&mut self, tx: Tx, node: Node) {
        let Some(name_node) = node.child_by_field_name("name") else { return self.script_children(tx, node) };
        let name = tx.text(name_node).to_string();
        let head_end = node.child_by_field_name("body").map_or(node.end_byte(), |b| b.start_byte());
        let signature = collapse(&tx.src[node.start_byte()..head_end], SIGNATURE_CAP);
        let access = children(node).find(|c| c.kind() == "access_type").map(|a| tx.text(a).to_string());
        let doc = doc_comment(tx, node);
        let idx = self.push_function(tx, node, &name, signature, access, doc);
        self.scopes.push(Scope::new(idx));
        if let Some(params) = node.child_by_field_name("parameters") {
            // `required model.Dao dao`: a parameter_type, then the identifier.
            let mut ty: Option<String> = None;
            for p in children(params) {
                match p.kind() {
                    "parameter_type" => ty = Some(tx.text(p).to_string()),
                    "identifier" | "assignment_pattern" => {
                        let ident = if p.kind() == "identifier" { Some(p) } else { p.child_by_field_name("left") };
                        if let (Some(i), Some(t)) = (ident, ty.take().filter(|t| is_component_type(t))) {
                            self.bind(tx.text(i), true, Ty::Component(t));
                        }
                    }
                    "," => ty = None,
                    _ => {}
                }
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.script_visit(tx, body);
        }
        self.pop_function(idx);
    }
}

/// The lower-case name of a tag node without `cf`: `include`, `loop`, `_mytag`.
fn tag_name(tx: Tx, node: Node) -> Option<String> {
    let start = match node.kind() {
        "cf_tag" => children(node).find(|c| matches!(c.kind(), "cf_start_tag" | "cf_start_tag_with_selfclose"))?,
        _ => node,
    };
    if let Some(n) = children(start).find(|c| c.kind() == "cf_tag_name") {
        return Some(tx.text(n).to_ascii_lowercase());
    }
    // A void tag's name is a hidden token right after `<cf`.
    let open = start.child(0)?;
    let rest = &tx.src[open.end_byte()..start.end_byte()];
    let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// A tag's attributes, from its own children and its start tag's.
fn tag_attrs<'t>(tx: Tx, node: Node<'t>) -> Vec<Attr<'t>> {
    fn collect<'t>(tx: Tx, node: Node<'t>, out: &mut Vec<Attr<'t>>, depth: u8) {
        for c in children(node) {
            match c.kind() {
                "cf_attribute" => {
                    let name = children(c).find(|n| n.kind() == "cf_attribute_name");
                    let value = children(c).find(|n| matches!(n.kind(), "quoted_cf_attribute_value" | "cf_attribute_value"));
                    if let (Some(n), Some(v)) = (name, value) {
                        out.push((tx.text(n).to_ascii_lowercase(), v));
                    }
                }
                "cf_tag_attributes" | "cf_start_tag" | "cf_start_tag_with_selfclose" if depth < 2 => collect(tx, c, out, depth + 1),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    collect(tx, node, &mut out, 0);
    out
}

/// `name = value` arguments of a script tag statement (`http url="…" method="post" {`) or tag function
/// (`cfhttp(url = "…")`).
fn named_args<'t>(tx: Tx, node: Node<'t>) -> Vec<Attr<'t>> {
    let mut out = Vec::new();
    for c in children(node) {
        let list: Vec<Node> = if c.kind() == "arguments" { children(c).collect() } else { vec![c] };
        for a in list {
            if a.kind() == "assignment_expression"
                && let (Some(l), Some(r)) = (a.child_by_field_name("left"), a.child_by_field_name("right"))
            {
                out.push((tx.text(l).to_ascii_lowercase(), r));
            }
        }
    }
    out
}

/// `extends="…"` attributes of a script component or property declaration.
fn component_attrs<'t>(tx: Tx, node: Node<'t>) -> Vec<Attr<'t>> {
    children(node)
        .filter(|c| c.kind() == "component_attribute")
        .filter_map(|c| {
            let name = children(c).find(|n| n.kind() == "identifier")?;
            let value = children(c).filter(|n| n.is_named()).nth(1)?;
            Some((tx.text(name).to_ascii_lowercase(), value))
        })
        .collect()
}

fn attr_value(tx: Tx, attrs: &[Attr], name: &str) -> Option<String> {
    attrs.iter().find(|(n, _)| n == name).and_then(|(_, v)| static_value(tx, *v))
}

/// The text of a literal value: a quoted attribute or string without `#…#`, an unquoted attribute, a bare word.
fn static_value(tx: Tx, node: Node) -> Option<String> {
    match node.kind() {
        "quoted_cf_attribute_value" | "string" => {
            let mut out = String::new();
            for c in children(node) {
                match c.kind() {
                    "attribute_value" | "string_fragment" => out.push_str(tx.text(c)),
                    "hash_expression" | "hash_empty" => return None,
                    "\"\"" => out.push('"'),
                    "''" => out.push('\''),
                    _ if c.is_named() => return None,
                    _ => {}
                }
            }
            Some(out)
        }
        "cf_attribute_value" | "attribute_value" => Some(tx.text(node).to_string()),
        _ => None,
    }
}

/// The expression inside a value that is one `#…#`: `component="#obj#"`.
fn hash_inner(value: Node) -> Option<Node> {
    let named: Vec<Node> = children(value).filter(|c| c.is_named()).collect();
    match named.as_slice() {
        [h] if h.kind() == "hash_expression" => children(*h).find(|c| c.is_named()),
        _ => None,
    }
}

/// A URL value as [`Piece`]s: literal text, constants (`#request.api.x#`, as lower case), and other values.
fn pieces(tx: Tx, node: Node) -> Vec<Piece> {
    let mut out = Vec::new();
    push_pieces(tx, node, &mut out);
    out
}

fn push_pieces(tx: Tx, node: Node, out: &mut Vec<Piece>) {
    match node.kind() {
        "quoted_cf_attribute_value" | "string" => {
            for c in children(node) {
                match c.kind() {
                    "attribute_value" | "string_fragment" => out.push(Piece::Text(tx.text(c).to_string())),
                    "hash_expression" => match children(c).find(|n| n.is_named()) {
                        Some(e) => push_pieces(tx, e, out),
                        None => out.push(Piece::Unknown),
                    },
                    _ => {}
                }
            }
        }
        "cf_attribute_value" => out.push(Piece::Text(tx.text(node).to_string())),
        "identifier" | "member_expression" => match dotted(tx, node) {
            Some(d) => out.push(Piece::Ref(d.to_ascii_lowercase())),
            None => out.push(Piece::Unknown),
        },
        "binary_expression" if node.child_by_field_name("operator").is_some_and(|o| tx.text(o) == "&") => {
            for side in ["left", "right"] {
                if let Some(s) = node.child_by_field_name(side) {
                    push_pieces(tx, s, out);
                }
            }
        }
        "call_expression" => out.push(Piece::Call),
        "parenthesized_expression" => match children(node).find(|c| c.is_named()) {
            Some(e) => push_pieces(tx, e, out),
            None => out.push(Piece::Unknown),
        },
        _ => out.push(Piece::Unknown),
    }
}

/// `createObject("component", "a.b.C")`, `createObject("a.b.C")`, `createObject("java", "x.Y")`.
fn create_object(tx: Tx, call: Node) -> Option<Ty> {
    let args: Vec<Node> = call.child_by_field_name("arguments").map(|a| children(a).filter(|c| c.is_named()).collect())?;
    let first = static_value(tx, *args.first()?)?;
    match (first.to_ascii_lowercase().as_str(), args.get(1)) {
        ("component", Some(a)) => static_value(tx, *a).map(Ty::Component),
        ("java", Some(a)) => static_value(tx, *a).map(Ty::Java),
        ("webservice" | "com" | ".net" | "dotnet" | "corba", _) => None,
        (_, None) => Some(Ty::Component(first)),
        _ => None,
    }
}

/// `a.b.c` for an identifier or a member chain of identifiers (`this`, `super` included).
fn dotted(tx: Tx, node: Node) -> Option<String> {
    match node.kind() {
        "identifier" | "this" | "super" | "property_identifier" => Some(tx.text(node).to_string()),
        "member_expression" => {
            let object = dotted(tx, node.child_by_field_name("object")?)?;
            let prop = node.child_by_field_name("property")?;
            Some(format!("{object}.{}", tx.text(prop)))
        }
        _ => None,
    }
}

/// A variable's key, lower case, without the scope prefix that doesn't change where it lives (`local.`,
/// `arguments.`, `variables.`, `this.`), and whether that prefix made it local to the function.
fn var_key(name: &str) -> (String, bool) {
    let lower = name.to_ascii_lowercase();
    for (prefix, local) in [("local.", true), ("arguments.", true), ("variables.", false), ("this.", false)] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            return (rest.to_string(), local);
        }
    }
    (lower, false)
}

fn last_segment(dotted: &str) -> &str {
    dotted.rsplit(['.', '/', ':']).next().unwrap_or(dotted)
}

/// An argument or property type naming a component: `model.Dao`, `UserService`, not `string` or `numeric`.
fn is_component_type(t: &str) -> bool {
    !t.is_empty()
        && !PLAIN_TYPES.contains(&t.to_ascii_lowercase().as_str())
        && !t.ends_with("[]")
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

fn looks_like_url(v: &str) -> bool {
    !v.contains(char::is_whitespace) && (v.contains("://") || (v.starts_with('/') && v.len() > 1 && !v.starts_with("//")))
}

/// `a/./b/../c` → `a/c`, without leading or trailing `/`.
fn normalize_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// First line of the `/** … */` comment directly above a script function.
fn doc_comment(tx: Tx, node: Node) -> Option<String> {
    let prev = node.prev_sibling().filter(|p| p.kind() == "comment" && p.end_position().row + 1 >= node.start_position().row)?;
    let text = tx.text(prev);
    if !text.starts_with("/**") {
        return None;
    }
    let first = text.trim_start_matches('/').trim_start_matches('*').lines().map(|l| l.trim().trim_start_matches('*').trim()).find(|l| !l.is_empty() && *l != "/")?;
    let first = first.trim_end_matches("*/").trim();
    (!first.is_empty()).then(|| collapse(first, 120))
}

// ---- SQL ----

/// Stands for a `#…#` value in query text: a name containing it is built at run time.
use crate::sql::DYNAMIC;

/// Query text with each `#` that isn't closed on its own line written `##`: a lone `#` starts a MySQL comment
/// (`#all orders btwn …`), not a value spanning lines, which would read the SQL after it as an expression.
fn literal_hashes(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('#') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut changed = false;
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut rest = line;
        while let Some(i) = rest.find('#') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            if let Some(stripped) = after.strip_prefix('#') {
                out.push_str("##");
                rest = stripped;
            } else if let Some(j) = after.find('#') {
                out.push_str(&rest[i..i + 1 + j + 1]);
                rest = &after[j + 1..];
            } else {
                out.push_str("##");
                changed = true;
                rest = after;
            }
        }
        out.push_str(rest);
    }
    if changed { out.into() } else { text.into() }
}

/// A query body as SQL: CFML comments and tags (`<cfqueryparam>`, `<cfif>`) blanked, `##` read as the `#` it
/// writes (a MySQL comment, a T-SQL temporary table), and each `#…#` value replaced by [`DYNAMIC`]. Line breaks are
/// kept, so lines still match the file.
fn query_sql(raw: &str) -> String {
    let raw = &literal_hashes(raw);
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let newlines = |s: &[u8], out: &mut Vec<u8>| out.extend(s.iter().filter(|&&c| c == b'\n'));
    let starts = |i: usize, s: &str| b.len() >= i + s.len() && b[i..i + s.len()].eq_ignore_ascii_case(s.as_bytes());
    let mut i = 0;
    while i < b.len() {
        if starts(i, "<!---") {
            let end = raw[i..].find("--->").map_or(b.len(), |j| i + j + 4);
            newlines(&b[i..end], &mut out);
            out.push(b' ');
            i = end;
        } else if starts(i, "<cf") || starts(i, "</cf") {
            let mut j = i;
            let mut quote = None;
            while j < b.len() {
                match (b[j], quote) {
                    (q @ (b'"' | b'\''), None) => quote = Some(q),
                    (q, Some(open)) if q == open => quote = None,
                    (b'>', None) => break,
                    _ => {}
                }
                j += 1;
            }
            let end = (j + 1).min(b.len());
            newlines(&b[i..end], &mut out);
            out.push(b' ');
            i = end;
        } else if b[i] == b'#' {
            if b.get(i + 1) == Some(&b'#') {
                out.push(b'#');
                i += 2;
            } else {
                let end = raw[i + 1..].find('#').map_or(b.len(), |j| i + 1 + j + 1);
                out.extend_from_slice(DYNAMIC.as_bytes());
                newlines(&b[i..end], &mut out);
                i = end;
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Tables a query body reads and writes and procedures it calls: see [`crate::sql::code_tables`].
fn query_tables(raw: &str) -> Vec<(String, &'static str, u32)> {
    crate::sql::code_tables(&query_sql(raw))
}

/// The source with the text of every CFML comment (`<!--- … --->`, nested) blanked, its line breaks and the outer
/// delimiters kept. The grammar ends a comment at a bare `-->` inside it, which would leave the rest as live code.
///
/// A `<!---` opens a comment only in template text: not inside a quoted attribute or expression string of a CFML tag
/// (`<cfif findNoCase("<!---", s)>`), nor in CFScript (a `<cfscript>` block, or a script component when `script` is
/// set), where it can only be in a string or a comment.
fn mask_comments(src: &str, script: bool) -> Option<String> {
    if !src.contains("<!---") {
        return None;
    }
    let mut b = src.as_bytes().to_vec();
    let n = b.len();
    let at = |b: &[u8], i: usize, s: &str| b.len() >= i + s.len() && b[i..i + s.len()].eq_ignore_ascii_case(s.as_bytes());
    let mut depth = 0u32;
    let mut in_script = script;
    let mut i = 0;
    while i < n {
        if depth > 0 {
            if at(&b, i, "<!---") {
                b[i..i + 5].fill(b' ');
                depth += 1;
                i += 5;
            } else if at(&b, i, "--->") {
                depth -= 1;
                if depth > 0 {
                    b[i..i + 4].fill(b' ');
                }
                i += 4;
            } else {
                if b[i] != b'\n' && b[i] != b'\r' {
                    b[i] = b' ';
                }
                i += 1;
            }
            continue;
        }
        if in_script {
            match b[i] {
                b'"' | b'\'' => i = skip_string(&b, i),
                b'/' if b.get(i + 1) == Some(&b'/') => i = b[i..].iter().position(|&c| c == b'\n').map_or(n, |p| i + p),
                b'/' if b.get(i + 1) == Some(&b'*') => i = find(&b, i + 2, b"*/").map_or(n, |p| p + 2),
                b'<' if !script && at(&b, i, "</cfscript") => {
                    in_script = false;
                    i += 10;
                }
                _ => i += 1,
            }
            continue;
        }
        if at(&b, i, "<!---") {
            depth = 1;
            i += 5;
        } else if (at(&b, i, "<cf") || at(&b, i, "</cf")) && b.get(i + if b[i + 1] == b'/' { 4 } else { 3 }).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') {
            // A CFML tag, up to its `>`: quoted values and expressions don't open comments.
            let opens_script = at(&b, i, "<cfscript") && !b.get(i + 9).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
            let mut j = i + 3;
            while j < n && b[j] != b'>' {
                if matches!(b[j], b'"' | b'\'') {
                    j = skip_string(&b, j);
                } else {
                    j += 1;
                }
            }
            i = j + 1;
            if opens_script {
                in_script = true;
            }
        } else {
            i += 1;
        }
    }
    // Only whole characters were blanked, so the text stays UTF-8.
    Some(String::from_utf8(b).expect("blanking keeps UTF-8"))
}

/// Past a CFML string opened at `i`: a doubled quote escapes, and a `#…#` expression inside may hold quotes of its own.
fn skip_string(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            c if c == q => {
                if b.get(j + 1) == Some(&q) {
                    j += 2;
                    continue;
                }
                return j + 1;
            }
            b'#' if b.get(j + 1) == Some(&b'#') => j += 2,
            b'#' => {
                // `"#f("x")#"`: to the closing `#`, unless the line ends first (a lone `#` in text).
                match b[j + 1..].iter().position(|&c| c == b'#' || c == b'\n') {
                    Some(p) if b[j + 1 + p] == b'#' => j = j + 1 + p + 1,
                    _ => j += 1,
                }
            }
            _ => j += 1,
        }
    }
    b.len()
}

fn find(b: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    b.get(from..)?.windows(pat.len()).position(|w| w == pat).map(|p| from + p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(f: &FileFacts) -> Vec<(String, &'static str, Option<String>, Option<String>)> {
        f.refs.iter().map(|r| (r.name.clone(), r.kind.as_str(), r.receiver.clone(), r.receiver_type.clone())).collect()
    }

    fn has(f: &FileFacts, name: &str, kind: &str, rtype: Option<&str>) -> bool {
        refs(f).iter().any(|(n, k, _, t)| n == name && *k == kind && t.as_deref() == rtype)
    }

    #[test]
    fn tag_component_functions_arguments_and_calls() {
        let src = r##"<cfcomponent extends="model.Base" hint="Users.">
<cfproperty name="dao" type="model.Dao">
<cffunction name="init" access="public" returntype="any">
  <cfargument name="dsn" type="string" required="yes">
  <cfargument name="mailer" type="lib.Mailer">
  <cfset var svc = createObject("component", "model.UserService")>
  <cfset variables.cache = new lib.Cache()>
  <cfset svc.load(arguments.dsn)>
  <cfset mailer.send()>
  <cfset dao.find(1)>
  <cfif len(arguments.dsn) GT 0 and isDefined("x")><cfset helper()></cfif>
  <cfloop from="1" to="3" index="i"><cfset cache.put(i)></cfloop>
  <cfreturn this>
</cffunction>
<cffunction name="helper" access="private"><cfreturn 1></cffunction>
</cfcomponent>
"##;
        let f = extract("model/Users.cfc", src);
        assert_eq!(f.parse_errors, 0);
        let c = &f.symbols[0];
        assert_eq!((c.name.as_str(), c.kind), ("Users", SymbolKind::Class));
        assert_eq!(c.doc.as_deref(), Some("Users."));
        assert_eq!(f.relations[0].target, "model.Base");
        let init = f.symbols.iter().find(|s| s.name == "init").expect("init");
        assert_eq!(init.kind, SymbolKind::Method);
        assert_eq!(init.container.as_deref(), Some("Users"));
        assert_eq!(init.signature, "public any function init(required string dsn, lib.Mailer mailer)");
        assert_eq!(init.attrs, vec!["public"]);
        // `if` with `and`, and a loop.
        assert_eq!(init.complexity, 4);
        assert_eq!(init.nesting, 1);
        let dao = f.symbols.iter().find(|s| s.name == "dao").expect("property");
        assert_eq!(dao.kind, SymbolKind::Property);
        assert!(has(&f, "userservice", "instantiate", Some("component:model.UserService")));
        assert!(has(&f, "cache", "instantiate", Some("component:lib.Cache")));
        assert!(has(&f, "load", "call", Some("component:model.UserService")));
        assert!(has(&f, "send", "call", Some("component:lib.Mailer")));
        assert!(has(&f, "find", "call", Some("component:model.Dao")));
        assert!(has(&f, "put", "call", Some("component:lib.Cache")));
        assert!(has(&f, "helper", "call", None));
        // Built-ins are not references.
        assert!(!refs(&f).iter().any(|(n, ..)| n == "len" || n == "isdefined"));
    }

    #[test]
    fn templates_include_query_http_and_custom_tags() {
        let src = r##"<cfinclude template="_inc/header.cfm">
<cfset request.api.orders = "https://api.example.com/v1/orders">
<cfquery name="q" datasource="#request.dsn#">
  SELECT u.id, o.total FROM dbo.Users u WITH (NOLOCK)
  INNER JOIN [dbo].[Orders] o ON o.uid = u.id
  WHERE u.id = <cfqueryparam value="#url.id#" cfsqltype="cf_sql_integer">
</cfquery>
<cfquery name="q2" dbtype="query">SELECT * FROM q</cfquery>
<cfquery name="upd">UPDATE o SET o.paid = 1 FROM Orders o WHERE o.id = #id#</cfquery>
<cfhttp url="#request.api.orders#/#id#/lines" method="post" result="r"></cfhttp>
<cfoutput>#formatName(q.name)#</cfoutput>
<cf_recaptcha action="x">
<cfmodule template="mods/box.cfm">
<cfinvoke component="_inc.CallerService" method="doPost" returnvariable="r">
"##;
        let f = extract("site/page.cfm", src);
        assert_eq!(f.symbols[0].kind, SymbolKind::Template);
        assert_eq!(f.symbols[0].name, "page");
        assert!(has(&f, "header", "call", Some("include:_inc/header.cfm")));
        assert!(has(&f, "box", "call", Some("include:mods/box.cfm")));
        assert!(has(&f, "recaptcha", "call", Some("tag:recaptcha")));
        assert!(has(&f, "formatname", "call", None));
        assert!(has(&f, "callerservice", "instantiate", Some("component:_inc.CallerService")));
        assert!(has(&f, "dopost", "call", Some("component:_inc.CallerService")));
        let tables: Vec<(&str, &str)> = f.db.iter().map(|d| (d.name.as_str(), d.kind)).collect();
        // `UPDATE o … FROM Orders o` writes Orders through its alias.
        assert_eq!(tables, vec![("dbo.Users", "sql_read"), ("dbo.Orders", "sql_read"), ("Orders", "sql_write")]);
        assert_eq!(f.constants[0].qualified, "request.api.orders");
        let route = &f.http_routes[0];
        assert_eq!((route.verb.as_str(), route.path.as_str()), ("POST", "{=request.api.orders}/{=id}/lines"));
    }

    #[test]
    fn script_component_with_mappings_and_query_execute() {
        let src = r##"/** Orders. */
component extends="model.Base" {
    property name="dao" type="model.Dao";
    this.mappings["/model"] = expandPath("./model");
    /** Loads one. */
    public string function load(required numeric id, model.Repo repo) {
        var svc = new model.UserService();
        svc.find(id);
        repo.get(id);
        this.helper();
        include "/_inc/funcs.cfm";
        cfhttp(url = "https://api.example.com/orders/#id#", method = "GET");
        q = queryExecute("DELETE FROM Carts WHERE id = :id", {id: id});
        if (a) { x = 1; } else if (b) { x = 2; }
        return formatPrice(1);
    }
    private void function helper() {}
}
"##;
        let f = extract("app/Orders.cfc", src);
        assert_eq!(f.parse_errors, 0);
        let load = f.symbols.iter().find(|s| s.name == "load").expect("load");
        assert_eq!(load.signature, "public string function load(required numeric id, model.Repo repo)");
        assert_eq!(load.doc.as_deref(), Some("Loads one."));
        assert_eq!(load.complexity, 3);
        assert_eq!(load.nesting, 1);
        assert!(has(&f, "find", "call", Some("component:model.UserService")));
        assert!(has(&f, "get", "call", Some("component:model.Repo")));
        assert!(refs(&f).iter().any(|(n, _, r, _)| n == "helper" && r.as_deref() == Some("this")));
        assert!(has(&f, "funcs", "call", Some("include:/_inc/funcs.cfm")));
        assert_eq!(f.db[0].name, "Carts");
        assert_eq!(f.db[0].kind, "sql_write");
        assert_eq!(f.http_routes[0].path, "https://api.example.com/orders/{=id}");
        let mapping = f.constants.iter().find(|c| c.qualified == "cfml.mapping:/model").expect("mapping");
        assert_eq!(mapping.value, "app/model");
        assert_eq!(f.relations[0].target, "model.Base");
    }

    #[test]
    fn a_port_after_a_base_address_is_not_a_path() {
        let f = extract("a.cfm", "<cfhttp url=\"#server#:443\" method=\"post\"></cfhttp>\n<cfhttp url=\"#server#:8080/api/x\"></cfhttp>\n");
        let paths: Vec<&str> = f.http_routes.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["{=server}/api/x"]);
    }

    #[test]
    fn javascript_functions_are_not_cfml() {
        let src = "<script>\nfunction show(x) { alert(x); }\n</script>\n<cfscript>\nfunction real() { return go(); }\n</cfscript>\n";
        let f = extract("a.cfm", src);
        let names: Vec<&str> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "real"]);
        assert_eq!(f.symbols[1].kind, SymbolKind::Function);
        assert!(has(&f, "go", "call", None));
        assert!(!has(&f, "alert", "call", None));
    }

    #[test]
    fn sql_reads_writes_and_procedures() {
        let t = |s: &str| query_tables(s).into_iter().map(|(n, k, _)| format!("{k}:{n}")).collect::<Vec<_>>();
        assert_eq!(t("INSERT INTO Orders (a) SELECT a FROM Carts"), vec!["sql_write:Orders", "sql_read:Carts"]);
        assert_eq!(t("DELETE FROM Carts WHERE id IN (SELECT id FROM Old)"), vec!["sql_write:Carts", "sql_read:Old"]);
        assert_eq!(t("SELECT a, b FROM x, y z WHERE EXTRACT(YEAR FROM created) = 1"), vec!["sql_read:x", "sql_read:y"]);
        assert_eq!(t("WITH recent AS (SELECT * FROM Orders) SELECT * FROM recent"), vec!["sql_read:Orders"]);
        assert_eq!(t("EXEC dbo.sp_DoIt @a = 1"), vec!["procedure:dbo.sp_DoIt"]);
        assert_eq!(t("SELECT * FROM #tableName# WHERE x = 'FROM y'"), Vec::<String>::new());
        assert_eq!(t("UPDATE Users SET n = 1 <cfif x>, m = 2</cfif> WHERE id = #id#"), vec!["sql_write:Users"]);
        // MySQL: modifiers, upserts, the catalog, and a dynamic schema.
        assert_eq!(
            t("INSERT IGNORE INTO a (x) VALUES (1) ON DUPLICATE KEY UPDATE x = VALUES(x), updated_at = CURRENT_TIMESTAMP"),
            vec!["sql_write:a"]
        );
        assert_eq!(t("SELECT column_name FROM information_schema.COLUMNS WHERE table_name = 'x'"), Vec::<String>::new());
        assert_eq!(t("SELECT id FROM Jobs WHERE n = 1 FOR UPDATE"), vec!["sql_read:Jobs"]);
        assert_eq!(t("SELECT * FROM #db#.users"), vec!["sql_read:users"]);
        // TRUNCATE writes.
        assert_eq!(t("TRUNCATE TABLE staging"), vec!["sql_write:staging"]);
        // Parenthesized joins keep every table.
        assert_eq!(
            t("SELECT * FROM (((orders o INNER JOIN order_detail d ON d.oid = o.id) INNER JOIN product p ON p.id = d.pid) LEFT JOIN customers c ON c.id = o.cid)"),
            vec!["sql_read:orders", "sql_read:order_detail", "sql_read:product", "sql_read:customers"]
        );
        // `##` writes `#`: a MySQL comment, whose words are not tables.
        assert_eq!(t("## totals for 82.6 from related.line42\nSELECT n FROM totals"), vec!["sql_read:totals"]);
        // A table built from a value is left out, and the rest of the statement is still read.
        assert_eq!(t("INSERT INTO #dyn#qrya (w) SELECT w FROM product_keyword"), vec!["sql_read:product_keyword"]);
        assert_eq!(t("CREATE TABLE IF NOT EXISTS `db`.`t` (id int); DROP TABLE old"), vec!["sql_write:db.t", "sql_write:old"]);
        // Lines are offsets in the body.
        assert_eq!(query_tables("\n\nSELECT 1 FROM a")[0].2, 2);
    }

    #[test]
    fn calls_inside_query_bodies_are_references() {
        let src = "<cfquery name=\"q\">\n  INSERT INTO hits (ip) VALUES ('#getIPv()#', <cfqueryparam value=\"#left(trackIp(),50)#\">)\n  <cfif isGood(x)>, 1</cfif>\n</cfquery>\n";
        let f = extract("a.cfm", src);
        let calls: Vec<(&str, u32)> = f.refs.iter().map(|r| (r.name.as_str(), r.line)).collect();
        assert_eq!(calls, [("getipv", 2), ("trackip", 2), ("isgood", 3)]);
        assert_eq!(f.db.iter().map(|d| (d.name.as_str(), d.line)).collect::<Vec<_>>(), [("hits", 2)]);
        assert_eq!(f.parse_errors, 0);
    }

    #[test]
    fn a_comment_opener_in_a_string_is_not_a_comment() {
        // In a tag's expression string, and in a <cfscript> string and comment.
        let src = "<cfif findNoCase(\"<!---\", s)><cfset a = one()></cfif>\n<cfscript>\nx = \"<!--- head\"; // <!---\n</cfscript>\n<cffunction name=\"after\"><cfreturn two()></cffunction>\n<!--- real <cfset gone()> --->\n";
        let f = extract("a.cfm", src);
        let names: Vec<&str> = f.refs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["one", "two"]);
        assert!(f.symbols.iter().any(|s| s.name == "after"));
        // A script component: `<!---` only in strings.
        let f = extract("C.cfc", "component {\n  p = \"<!---.*<cf\";\n  function later() { return go(); }\n}\n");
        assert!(f.symbols.iter().any(|s| s.name == "later"));
    }

    #[test]
    fn a_lone_hash_in_a_query_is_a_mysql_comment() {
        let src = "<cfquery name=\"q\">\n#all orders on the where pp\nselect id from orders o where o.id = #id#\n</cfquery>\n";
        let f = extract("a.cfm", src);
        assert!(f.refs.is_empty(), "{:?}", f.refs.iter().map(|r| &r.name).collect::<Vec<_>>());
        assert_eq!(f.db.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["orders"]);
    }

    #[test]
    fn a_bare_html_comment_end_inside_a_cfml_comment_stays_comment() {
        let src = "<!--- old: <!-- x --> <cfinclude template=\"self.cfm\"> --->\n<cfset y = go()>\n";
        let f = extract("a.cfm", src);
        let names: Vec<&str> = f.refs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["go"]);
    }
}
