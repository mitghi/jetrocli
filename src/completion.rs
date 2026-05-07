//! Completion engine: JSON shape + jetro builtins, merged + ranked.
//!
//! Given a partial expression (and cursor position), emits candidates:
//!   - Field names at the current path (from shape inference)
//!   - Builtin method names (receiver-type aware)
//!   - Snippets for common forms (lambda, projection)

use crate::shape::Shape;

mod builtins {
    /// Hardcoded list of jetro builtin method names. Kept in sync manually
    /// because `jetro_core` no longer exposes a public registry accessor.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "abs", "accumulate", "all", "any", "append", "approx_count_distinct", "avg",
            "byte_len", "bytes",
            "camel_case", "capitalize", "captures", "captures_all", "ceil", "center", "chars",
            "chars_of", "chunk", "collect", "compact", "contains_all",
            "contains_any", "count", "count_by", "cummax", "cummin",
            "dedent", "deep_find", "deep_like", "deep_merge", "deep_shape", "defaults",
            "del_path", "del_paths", "diff", "diff_window", "drop_while",
            "ends_with", "entries", "enumerate", "equi_join", "explode",
            "fanout", "filter", "filter_keys", "filter_values", "find", "find_all",
            "find_first", "find_index", "find_one", "first", "flat_map", "flatten", "flatten_keys", "floor",
            "from_base64", "from_json", "from_pairs",
            "get_path", "group_by", "group_shape",
            "has", "has_path", "html_escape", "html_unescape",
            "implode", "includes", "indent", "index", "index_by", "index_of",
            "indices_of", "indices_where", "intersect", "invert",
            "is_alpha", "is_ascii", "is_blank", "is_numeric",
            "join", "kebab_case", "keys",
            "lag", "last", "last_index_of", "lead", "len", "lines", "lower",
            "map", "match_all", "match_first", "matches", "max", "max_by", "merge",
            "min", "min_by", "missing",
            "nth",
            "omit", "or",
            "pad_left", "pad_right", "pairwise", "parse_bool", "parse_float", "parse_int",
            "partition", "pascal_case", "pct_change", "pick", "pivot", "prepend",
            "re_match", "rec", "remove", "rename", "repeat",
            "replace", "replace_all", "replace_all_re", "replace_re", "reverse",
            "reverse_str", "rolling_avg", "rolling_max", "rolling_min", "rolling_sum", "round",
            "scan", "schema", "set", "set_path", "skip", "slice", "snake_case", "sort", "split",
            "split_re", "starts_with", "strip_prefix", "strip_suffix", "sum",
            "take", "take_while", "title_case", "to_base64", "to_bool", "to_csv", "to_json",
            "to_number", "to_pairs", "to_string", "to_tsv", "trace_path",
            "transform_keys", "transform_values", "trim", "trim_left", "trim_right", "type",
            "union", "unique", "unique_by", "unflatten_keys", "update", "upper",
            "url_decode", "url_encode",
            "values",
            "walk", "walk_pre", "window", "words",
            "zip", "zip_longest", "zip_shape", "zscore",
        ]
    }
}
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub text:  String,
    pub kind:  CandKind,
    pub doc:   String, // multi-line help: signature, summary, example
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandKind {
    Field,
    Method,
    Snippet,
    Keyword,
}

/// Compute completion candidates for the expression typed up to `cursor`.
/// Context: `doc` shape provides field suggestions; `expr[..cursor]` determines receiver.
pub fn complete(expr: &str, cursor: usize, doc: &Value) -> Vec<Candidate> {
    let head  = &expr[..cursor.min(expr.len())];
    let shape = Shape::of(doc);

    let (path, prefix, ctx) = split_context(head);

    let mut out: Vec<Candidate> = Vec::new();

    let recv_shape_owned = resolve_shape(&shape, &path);
    let recv_shape: Option<&Shape> = recv_shape_owned.as_ref();

    match ctx {
        Ctx::AfterDot => {
            // fields first (object only), then methods
            if let Some(obj_keys) = object_keys(recv_shape) {
                for k in obj_keys {
                    if k.starts_with(prefix) {
                        let sub = recv_shape.and_then(|s| s.field(k));
                        out.push(Candidate {
                            text: k.to_string(),
                            kind: CandKind::Field,
                            doc: field_doc(k, sub),
                        });
                    }
                }
            }
            // For arrays: element object keys auto-unwrap (filter/map sugar)
            if let Some(Shape::Object(m)) = recv_shape.and_then(|s| s.element()) {
                for k in m.keys() {
                    let name = k.as_ref();
                    if name.starts_with(prefix) {
                        out.push(Candidate {
                            text: name.to_string(),
                            kind: CandKind::Field,
                            doc: format!(
                                "{}  (element field)\n\n\
                                 When receiver is an array of objects, bare field names\n\
                                 resolve against each element.\n\n\
                                 Example:\n  $.books.title\n  -> [\"Dune\", \"Foundation\"]",
                                name
                            ),
                        });
                    }
                }
            }
            // methods — filter by receiver kind
            for name in builtins::all_names() {
                if !name.starts_with(prefix) { continue; }
                if !method_applies(name, &recv_shape) { continue; }
                out.push(Candidate {
                    text: format!("{}()", name),
                    kind: CandKind::Method,
                    doc:  method_doc(name),
                });
            }
        }
        Ctx::AfterLParen => {
            let inner = recv_shape.and_then(|s| s.element()).or(recv_shape);
            if let Some(obj_keys) = object_keys(inner) {
                for k in obj_keys {
                    if k.starts_with(prefix) {
                        out.push(Candidate {
                            text: k.to_string(),
                            kind: CandKind::Field,
                            doc:  format!("{}\n\nElement field of the current receiver.", k),
                        });
                    }
                }
            }
            for kw in ["lambda", "let", "not", "and", "or", "kind", "is", "as", "when",
                       "for", "in", "if", "else", "try", "match", "with"] {
                if kw.starts_with(prefix) {
                    out.push(Candidate {
                        text: kw.into(),
                        kind: CandKind::Keyword,
                        doc:  keyword_doc(kw),
                    });
                }
            }
            out.push(Candidate {
                text: "lambda x: ".into(),
                kind: CandKind::Snippet,
                doc: "lambda <param>: <body>\n\n\
                      Anonymous function. Used as predicate or projection in\n\
                      methods like filter, map, sort.\n\n\
                      Example:\n  $.books.sort(lambda b: b.price)".into(),
            });
        }
        Ctx::Root => {
            for t in ["$", "@", "let", "patch", "match", "try", "[", "{"] {
                if t.starts_with(prefix) {
                    out.push(Candidate {
                        text: t.into(),
                        kind: CandKind::Keyword,
                        doc:  keyword_doc(t),
                    });
                }
            }
            if "match".starts_with(prefix) {
                out.push(Candidate {
                    text: "match $ with { _ -> null }".into(),
                    kind: CandKind::Snippet,
                    doc:  "match <scrutinee> with { <pat> -> <body>, … }\n\n\
                           Pattern match expression.\n\n\
                           Example:\n  match $.event with {\n    {kind: \"click\"} -> 1,\n    _ -> 0\n  }".into(),
                });
            }
            if "try".starts_with(prefix) {
                out.push(Candidate {
                    text: "try $ else null".into(),
                    kind: CandKind::Snippet,
                    doc:  "try <body> else <default>\n\nFallback expression.\n\nExample:\n  try $.user.email else \"unknown\"".into(),
                });
            }
            if let Some(obj_keys) = object_keys(Some(&shape)) {
                for k in obj_keys {
                    if k.starts_with(prefix) {
                        let sub = shape.field(k);
                        out.push(Candidate {
                            text: format!("$.{}", k),
                            kind: CandKind::Field,
                            doc:  field_doc(k, sub),
                        });
                    }
                }
            }
        }
    }

    // rank: Field > Method > Keyword > Snippet, then by length
    out.sort_by_key(|c| {
        let order = match c.kind {
            CandKind::Field   => 0,
            CandKind::Method  => 1,
            CandKind::Keyword => 2,
            CandKind::Snippet => 3,
        };
        (order, c.text.len(), c.text.clone())
    });
    out.dedup_by(|a, b| a.text == b.text);
    out
}

#[derive(Debug, Clone, Copy)]
enum Ctx { AfterDot, AfterLParen, Root }

/// Find the logical path up to the current completion point, plus the partial
/// identifier being typed and whether we are after `.` or `(`.
fn split_context(head: &str) -> (Vec<PathSeg>, &str, Ctx) {
    // scan from end backwards to find trigger point
    let bytes = head.as_bytes();
    let mut i = bytes.len();
    // find where current word starts
    while i > 0 {
        let c = bytes[i - 1] as char;
        if c.is_alphanumeric() || c == '_' { i -= 1; } else { break; }
    }
    let prefix = &head[i..];
    let before = &head[..i];
    let trimmed = before.trim_end();

    let ctx = if trimmed.ends_with('.') {
        Ctx::AfterDot
    } else if trimmed.ends_with('(') || trimmed.ends_with(',') {
        Ctx::AfterLParen
    } else if trimmed.is_empty() {
        Ctx::Root
    } else {
        Ctx::Root
    };

    // path: strip the trailing trigger char, then parse
    let path_src = match ctx {
        Ctx::AfterDot    => trimmed.trim_end_matches('.'),
        Ctx::AfterLParen => {
            // Walk back to the method call owner: `$.a.b.method(`  → `$.a.b`
            // Find matching `.ident(` and use everything before it
            let t = trimmed.trim_end_matches(|c: char| c == '(' || c == ',' || c.is_whitespace());
            // strip `.ident` if present (the method name)
            if let Some(dot_at) = t.rfind('.') {
                &t[..dot_at]
            } else { t }
        }
        Ctx::Root => "",
    };

    (parse_path(path_src), prefix, ctx)
}

#[derive(Debug, Clone)]
enum PathSeg {
    Field(String),
    Index,
    Method(String),
}

fn parse_path(src: &str) -> Vec<PathSeg> {
    let s = src.trim();
    if s.is_empty() || s == "$" || s == "@" { return Vec::new(); }
    let body = s.trim_start_matches('$').trim_start_matches('@').trim_start_matches('.');
    let chars: Vec<char> = body.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '.' { i += 1; continue; }
        if c == '[' {
            let mut depth = 1; i += 1;
            while i < chars.len() && depth > 0 {
                match chars[i] { '[' => depth += 1, ']' => depth -= 1, _ => {} }
                i += 1;
            }
            out.push(PathSeg::Index);
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
            let name: String = chars[start..i].iter().collect();
            if i < chars.len() && chars[i] == '(' {
                let mut depth = 1; i += 1;
                let mut in_str = false;
                let mut esc = false;
                while i < chars.len() && depth > 0 {
                    let cc = chars[i];
                    if esc { esc = false; i += 1; continue; }
                    if in_str {
                        if cc == '\\' { esc = true; }
                        else if cc == '"' { in_str = false; }
                    } else {
                        match cc {
                            '"' => in_str = true,
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                    }
                    i += 1;
                }
                out.push(PathSeg::Method(name));
            } else {
                out.push(PathSeg::Field(name));
            }
            continue;
        }
        i += 1;
    }
    out
}

fn resolve_shape(root: &Shape, path: &[PathSeg]) -> Option<Shape> {
    let mut cur: Option<Shape> = Some(root.clone());
    for seg in path {
        cur = match (cur.as_ref(), seg) {
            (Some(s), PathSeg::Field(f))   => s.field(f).cloned(),
            (Some(s), PathSeg::Index)      => s.element().cloned(),
            (Some(s), PathSeg::Method(m))  => apply_method_shape(s, &m),
            _ => None,
        };
    }
    cur
}

fn apply_method_shape(s: &Shape, name: &str) -> Option<Shape> {
    let arr_of = |e: Shape| Shape::Array(Box::new(e));
    let empty_obj = || Shape::Object(Default::default());
    match name {
        // element extraction
        "first" | "last" | "nth" | "find" | "find_first" | "find_one" | "index"
        | "min_by" | "max_by" => s.element().cloned(),
        // array → array (same element type)
        "filter" | "sort" | "reverse" | "unique" | "unique_by"
        | "compact" | "take" | "skip"
        | "take_while" | "drop_while"
        | "enumerate" | "accumulate" | "append" | "prepend" | "remove"
        | "diff" | "intersect" | "union" | "slice" | "find_all"
        | "collect" | "flatten" | "explode"
        | "lag" | "lead" => Some(s.clone()),
        // numeric series
        "rolling_sum" | "rolling_avg" | "rolling_min" | "rolling_max"
        | "cummin" | "cummax" | "diff_window" | "pct_change" | "zscore"
            => Some(arr_of(Shape::Float)),
        // index lookups
        "find_index" | "byte_len" | "chars_of"
            => Some(Shape::Int),
        "indices_of" | "indices_where"
            => Some(arr_of(Shape::Int)),
        // string predicates
        "is_alpha" | "is_ascii" | "is_blank" | "is_numeric"
        | "contains_all" | "contains_any" | "re_match"
            => Some(Shape::Bool),
        // string regex captures
        "captures" | "match_all"      => Some(arr_of(Shape::Str)),
        "captures_all"                => Some(arr_of(arr_of(Shape::Str))),
        "match_first"                 => Some(Shape::Str),
        "replace_re" | "replace_all_re" | "reverse_str"
        | "center" | "to_base64" | "from_base64" => Some(Shape::Str),
        "split_re"                    => Some(arr_of(Shape::Str)),
        // case conversions
        "camel_case" | "snake_case" | "kebab_case" | "pascal_case"
            => Some(Shape::Str),
        // bytes
        "bytes" => Some(arr_of(Shape::Int)),
        // parsing
        "parse_int" | "parse_float" => Some(Shape::Float),
        "parse_bool" => Some(Shape::Bool),
        // array-of-array
        "window" | "chunk" | "pairwise" | "partition" | "zip" | "zip_longest" =>
            Some(arr_of(s.clone())),
        // map: can't infer element, mark Unknown
        "map" | "flat_map" | "fanout" | "transform_values"
        | "transform_keys" => Some(arr_of(Shape::Unknown)),
        // object keys/values/entries
        "keys" => Some(arr_of(Shape::Str)),
        "values" => Some(match s {
            Shape::Object(m) => {
                let mut it = m.values();
                match it.next().cloned() {
                    Some(first) => arr_of(it.fold(first, |acc, v| acc.merge(v.clone()))),
                    None => arr_of(Shape::Unknown),
                }
            }
            _ => arr_of(Shape::Unknown),
        }),
        "entries" | "to_pairs" => Some(arr_of(arr_of(Shape::Unknown))),
        "from_pairs" | "invert" | "pivot" | "rename"
        | "pick" | "omit" | "merge" | "deep_merge" | "defaults"
        | "filter_keys" | "filter_values" | "flatten_keys" | "unflatten_keys"
        | "set" | "update" | "group_by" | "count_by" | "index_by" => Some(empty_obj()),
        // numeric results
        "len" | "count" | "index_of" | "last_index_of"
        | "approx_count_distinct" => Some(Shape::Int),
        // coalesce: keep receiver's shape
        "or" => Some(s.clone()),
        "sum" | "avg" | "min" | "max" | "abs" | "round" | "ceil" | "floor"
        | "to_number" => Some(Shape::Float),
        // boolean results
        "any" | "all" | "includes" | "has" | "has_path"
        | "starts_with" | "ends_with" | "matches" | "to_bool" => Some(Shape::Bool),
        // string results
        "upper" | "lower" | "capitalize" | "title_case" | "trim"
        | "trim_left" | "trim_right" | "replace" | "replace_all"
        | "strip_prefix" | "strip_suffix" | "indent" | "dedent"
        | "repeat" | "pad_left" | "pad_right" | "url_encode" | "url_decode"
        | "html_escape" | "html_unescape" | "to_string" | "to_json"
        | "to_csv" | "to_tsv" | "join" | "implode" | "type" => Some(Shape::Str),
        // string → [string]
        "lines" | "words" | "chars" | "split" | "scan" => Some(arr_of(Shape::Str)),
        // paths / traversal
        "get_path" | "set_path" | "del_path" | "del_paths"
        | "walk" | "walk_pre" | "from_json" => Some(Shape::Unknown),
        "trace_path" | "rec" | "deep_find" | "deep_like" => Some(arr_of(Shape::Unknown)),
        "schema" | "deep_shape" | "group_shape" | "zip_shape" => Some(Shape::Unknown),
        "missing" => Some(arr_of(Shape::Str)),
        _ => Some(Shape::Unknown),
    }
}

fn object_keys(s: Option<&Shape>) -> Option<Vec<&str>> {
    match s? {
        Shape::Object(m) => Some(m.keys().map(|k| k.as_ref()).collect()),
        _ => None,
    }
}

fn shape_hint(s: Option<&Shape>) -> String {
    match s {
        Some(Shape::Str)            => "string".into(),
        Some(Shape::Int)            => "int".into(),
        Some(Shape::Float)          => "float".into(),
        Some(Shape::Bool)           => "bool".into(),
        Some(Shape::Null)           => "null".into(),
        Some(Shape::Array(e))       => format!("array[{}]", shape_name(e)),
        Some(Shape::Object(m))      => format!("object ({} keys)", m.len()),
        Some(Shape::Union(_))       => "union".into(),
        Some(Shape::Unknown) | None => "".into(),
    }
}

fn shape_name(s: &Shape) -> &'static str {
    match s {
        Shape::Str       => "string",
        Shape::Int       => "int",
        Shape::Float     => "float",
        Shape::Bool      => "bool",
        Shape::Null      => "null",
        Shape::Array(_)  => "array",
        Shape::Object(_) => "object",
        Shape::Union(_)  => "union",
        Shape::Unknown   => "?",
    }
}

fn method_applies(name: &str, recv: &Option<&Shape>) -> bool {
    // permissive: always show unless receiver clearly incompatible
    let Some(s) = recv else { return true; };
    let is_arr = matches!(s, Shape::Array(_));
    let is_obj = matches!(s, Shape::Object(_));
    let is_str = matches!(s, Shape::Str);
    let arr_only  = ["filter","map","flat_map","sort","flatten","first","last","nth",
                     "append","prepend","remove","diff","intersect","union","enumerate",
                     "window","chunk","take","skip",
                     "take_while","drop_while",
                     "accumulate","partition","zip","zip_longest","pairwise","reverse","unique",
                     "unique_by","compact","sum","avg","count","count_by","group_by",
                     "index_by","min","max","min_by","max_by","any","all","equi_join","explode",
                     "fanout","find","find_all","find_first","find_one","find_index",
                     "indices_of","indices_where","approx_count_distinct",
                     "contains_all","contains_any","rolling_sum","rolling_avg","rolling_min",
                     "rolling_max","cummin","cummax","diff_window","pct_change","lag","lead",
                     "zscore","collect","index"];
    let str_only  = ["upper","lower","capitalize","title_case","camel_case","snake_case",
                     "kebab_case","pascal_case","trim","trim_left","trim_right",
                     "lines","words",
                     "chars","to_number","to_bool","url_encode","url_decode","html_escape",
                     "html_unescape","to_base64","from_base64",
                     "repeat","reverse_str","pad_left","pad_right",
                     "center","starts_with","ends_with","replace","replace_all","replace_re",
                     "replace_all_re","split","split_re","strip_prefix","strip_suffix","slice",
                     "indent","dedent","matches","scan","captures","captures_all","match_first",
                     "match_all","re_match","is_alpha","is_ascii","is_blank","is_numeric",
                     "byte_len","bytes","chars_of","parse_int","parse_float","parse_bool"];
    let obj_only  = ["keys","values","entries","to_pairs","from_pairs","invert","pick","omit",
                     "merge","deep_merge","defaults","rename","transform_keys","transform_values",
                     "filter_keys","filter_values","pivot","flatten_keys","unflatten_keys"];
    if arr_only.contains(&name) && !is_arr { return false; }
    if str_only.contains(&name) && !is_str { return false; }
    if obj_only.contains(&name) && !is_obj { return false; }
    true
}

fn field_doc(name: &str, shape: Option<&Shape>) -> String {
    let ty = shape_hint(shape);
    let header = if ty.is_empty() {
        format!("{}  (field)", name)
    } else {
        format!("{}  :  {}", name, ty)
    };
    let detail = match shape {
        Some(Shape::Object(m)) => {
            let keys: Vec<&str> = m.keys().map(|k| k.as_ref()).take(8).collect();
            format!("Object with keys: {}", keys.join(", "))
        }
        Some(Shape::Array(e)) => format!("Array of {}.", shape_name(e)),
        Some(Shape::Str)   => "String value.".into(),
        Some(Shape::Int)   => "Integer value.".into(),
        Some(Shape::Float) => "Floating-point value.".into(),
        Some(Shape::Bool)  => "Boolean value.".into(),
        Some(Shape::Null)  => "Null.".into(),
        _ => String::new(),
    };
    format!("{}\n\n{}", header, detail)
}

fn keyword_doc(kw: &str) -> String {
    match kw {
        "$" => "$ — root document.\n\nExample:\n  $.store.books".into(),
        "@" => "@ — current element (inside predicates / lambdas).\n\nExample:\n  $.books.filter(@.price > 10)".into(),
        "let" => "let <name> = <expr> ; <body>\n\nBind a local name.\n\nExample:\n  let avg = $.prices.avg() ; $.prices.filter(@ > avg)".into(),
        "patch" => "patch { … }\n\nApply structured patch operations.".into(),
        "lambda" => "lambda <param>: <body>\n\nAnonymous function.\n\nExample:\n  $.books.sort(lambda b: b.price)".into(),
        "not"  => "not <expr>  — logical negation.".into(),
        "and"  => "<a> and <b> — logical and.".into(),
        "or"   => "<a> or <b> — logical or.".into(),
        "kind" => "kind(<expr>) — runtime type: \"string\"|\"number\"|\"array\"|\"object\"|…".into(),
        "when" => "when <cond>: <expr> — conditional projection.".into(),
        "for"  => "for <x> in <coll>: <expr> — comprehension.".into(),
        "in"   => "<x> in <coll> — membership test.".into(),
        "if"   => "if <cond>: <a> else: <b> — conditional.".into(),
        "else" => "else — alternative branch (paired with `if` or `try`).".into(),
        "is"   => "<expr> is <kind> — runtime kind test (alias of `kind`).\n\nExample:\n  $.value is string".into(),
        "as"   => "<expr> as <kind> — coerce to kind (when applicable).".into(),
        "try"  => "try <body> else <default>\n\nEvaluate body; on error / missing fall back to default.\n\nExample:\n  try $.user.email else \"unknown\"".into(),
        "match" => "match <scrutinee> with { <pat> -> <body>, … }\n\n\
                    Pattern match. Patterns: literals, ranges (1..10, 1..=10), bindings (x),\n\
                    wildcards (_), kind tests (x: string), object {k: pat, ...}, array\n\
                    [a, b, ...rest], or-patterns (a | b), guards (pat when <cond>).\n\n\
                    Example:\n  match $.event with {\n    {kind: \"click\", x: x, y: y} -> [x, y],\n    {kind: \"key\", code: c} when c > 0 -> c,\n    _ -> null\n  }\n\n\
                    Deep variants:\n  $..match { pat -> body }      — collect all matching descendants\n  $..match! { pat -> body }     — first matching descendant only".into(),
        "with"  => "with — separator between `match` scrutinee and arms.\n\n\
                    Example:\n  match $.x with { 0 -> \"zero\", n -> n }".into(),
        "["    => "[ ... ] — array literal or index.".into(),
        "{"    => "{ ... } — object literal.".into(),
        _ => format!("{}  (keyword)", kw),
    }
}

fn method_doc(name: &str) -> String {
    let entry: Option<(&str, &str, &str)> = match name {
        "map" => Some((
            "map(expr) → array",
            "Apply expr to each element; on objects maps values.",
            "$.books.map(.title)\n-> [\"Dune\", \"Foundation\"]",
        )),
        "filter" => Some((
            "filter(pred) → array",
            "Keep elements where pred is truthy. @ refers to the current element.",
            "$.books.filter(.price > 10)\n-> [{\"title\":\"Dune\", ...}]",
        )),
        "flat_map" => Some((
            "flat_map(expr) → array",
            "Map then flatten one level.",
            "$.authors.flat_map(.books)",
        )),
        "sort" => Some((
            "sort([key | lambda]) → array",
            "Stable sort. Optional key expression or lambda; prefix with - for descending.",
            "$.books.sort(.price)",
        )),
        "reverse" => Some((
            "reverse() → array",
            "Reverse element order.",
            "$.items.reverse()",
        )),
        "unique" => Some((
            "unique() → array",
            "Remove duplicate elements (preserves first occurrence).",
            "$.tags.unique()",
        )),
        "first" => Some((
            "first() → any",
            "First element of array or null.",
            "$.books.first()",
        )),
        "last" => Some((
            "last() → any",
            "Last element of array or null.",
            "$.books.last()",
        )),
        "nth" => Some((
            "nth(i) → any",
            "Element at index i (negative counts from end).",
            "$.books.nth(0)",
        )),
        "take" => Some((
            "take(n) → array",
            "First n elements (streaming prefix).",
            "$.events.take(10)",
        )),
        "skip" => Some((
            "skip(n) → array",
            "Drop first n elements; emit the rest.",
            "$.events.skip(10)",
        )),
        "take_while" => Some((
            "take_while(pred) → array",
            "Prefix of elements satisfying pred.",
            "$.nums.take_while(@ < 10)",
        )),
        "drop_while" => Some((
            "drop_while(pred) → array",
            "Suffix after the prefix satisfying pred.",
            "$.nums.drop_while(@ < 10)",
        )),
        "flatten" => Some((
            "flatten([n]) → array",
            "Flatten nested arrays up to n levels (default 1).",
            "$.groups.flatten()",
        )),
        "enumerate" => Some((
            "enumerate() → array",
            "Pair each element with its index.",
            "$.items.enumerate()\n-> [[0,a],[1,b]]",
        )),
        "window" => Some((
            "window(n) → array",
            "Sliding windows of size n.",
            "$.nums.window(3)",
        )),
        "chunk" => Some((
            "chunk(n) → array",
            "Partition into contiguous chunks of size n.",
            "$.items.chunk(2)",
        )),
        "pairwise" => Some((
            "pairwise() → array",
            "Adjacent pairs of elements.",
            "$.nums.pairwise()",
        )),
        "zip" => Some((
            "zip(other, …) → array",
            "Element-wise tuple; truncates to shortest.",
            "$.xs.zip($.ys)",
        )),
        "zip_longest" => Some((
            "zip_longest(other, …) → array",
            "Element-wise tuple; pads shorter with null.",
            "$.xs.zip_longest($.ys)",
        )),
        "sum" => Some((
            "sum([field]) → number",
            "Sum numeric values (optionally projected by field/expr).",
            "$.books.sum(.price)",
        )),
        "avg" => Some((
            "avg([field]) → number",
            "Arithmetic mean of numeric values.",
            "$.books.avg(.price)",
        )),
        "min" => Some((
            "min([field]) → any",
            "Minimum value.",
            "$.books.min(.price)",
        )),
        "max" => Some((
            "max([field]) → any",
            "Maximum value.",
            "$.books.max(.price)",
        )),
        "count" => Some((
            "count([pred]) → int",
            "Count of elements (optionally matching pred).",
            "$.books.count(.price > 10)",
        )),
        "any" => Some((
            "any(pred) → bool",
            "True if any element matches pred.",
            "$.books.any(.price > 100)",
        )),
        "approx_count_distinct" => Some((
            "approx_count_distinct() → int",
            "Approximate distinct count (HyperLogLog).",
            "$.events.approx_count_distinct()",
        )),
        "all" => Some((
            "all(pred) → bool",
            "True if all elements match pred.",
            "$.books.all(.in_stock)",
        )),
        "group_by" => Some((
            "group_by(key) → {key: [items]}",
            "Group array elements by key expression.",
            "$.books.group_by(.author)",
        )),
        "count_by" => Some((
            "count_by(key) → {key: int}",
            "Count occurrences per group key.",
            "$.events.count_by(.type)",
        )),
        "index_by" => Some((
            "index_by(key) → {key: item}",
            "Index elements by unique key (last wins on conflict).",
            "$.users.index_by(.id)",
        )),
        "partition" => Some((
            "partition(pred) → [match, rest]",
            "Split into two arrays by predicate.",
            "$.books.partition(.price > 10)",
        )),
        "accumulate" => Some((
            "accumulate(expr) → array",
            "Running fold / prefix reduction.",
            "$.nums.accumulate(@ + acc)",
        )),
        "compact" => Some((
            "compact() → array",
            "Drop null / empty values.",
            "$.xs.compact()",
        )),
        "equi_join" => Some((
            "equi_join(other, lk, rk) → array",
            "Inner join two arrays on equal keys.",
            "$.orders.equi_join($.users, .user_id, .id)",
        )),
        "keys" => Some((
            "keys() → [string]",
            "Keys of an object.",
            "$.user.keys()",
        )),
        "values" => Some((
            "values() → [any]",
            "Values of an object.",
            "$.user.values()",
        )),
        "entries" | "to_pairs" => Some((
            "entries() → [[k,v]]",
            "Object as array of key-value pairs.",
            "$.user.entries()",
        )),
        "from_pairs" => Some((
            "from_pairs() → object",
            "Build object from array of [k,v] pairs.",
            "$.pairs.from_pairs()",
        )),
        "invert" => Some((
            "invert() → object",
            "Swap keys and values (values must be stringifiable).",
            "$.lookup.invert()",
        )),
        "pick" => Some((
            "pick(k, …) → object",
            "Keep only listed keys.",
            "$.user.pick(\"id\", \"email\")",
        )),
        "omit" => Some((
            "omit(k, …) → object",
            "Drop listed keys.",
            "$.user.omit(\"password\")",
        )),
        "merge" => Some((
            "merge(other) → object",
            "Shallow merge (right wins).",
            "$.a.merge($.b)",
        )),
        "deep_merge" => Some((
            "deep_merge(other) → object",
            "Recursive merge.",
            "$.config.deep_merge($.override)",
        )),
        "defaults" => Some((
            "defaults(other) → object",
            "Fill missing keys from other (left wins).",
            "$.user.defaults($.defaults)",
        )),
        "rename" => Some((
            "rename({old: new, …}) → object",
            "Rename keys.",
            "$.user.rename({\"uid\": \"id\"})",
        )),
        "transform_keys" => Some((
            "transform_keys(fn) → object",
            "Apply fn to each key.",
            "$.user.transform_keys(lambda k: k.upper())",
        )),
        "transform_values" => Some((
            "transform_values(fn) → object",
            "Apply fn to each value.",
            "$.user.transform_values(lambda v: v.to_string())",
        )),
        "filter_keys" => Some((
            "filter_keys(pred) → object",
            "Keep entries whose key satisfies pred.",
            "$.headers.filter_keys(lambda k: k.starts_with(\"x-\"))",
        )),
        "filter_values" => Some((
            "filter_values(pred) → object",
            "Keep entries whose value satisfies pred.",
            "$.scores.filter_values(@ > 0)",
        )),
        "upper" => Some(("upper() → string", "Uppercase.", "$.name.upper()")),
        "lower" => Some(("lower() → string", "Lowercase.", "$.name.lower()")),
        "capitalize" => Some(("capitalize() → string", "Capitalize first letter.", "$.word.capitalize()")),
        "title_case"  => Some(("title_case() → string", "Title-case each word.", "$.heading.title_case()")),
        "trim" => Some(("trim() → string", "Strip leading/trailing whitespace.", "$.s.trim()")),
        "split" => Some(("split(sep) → [string]", "Split string by separator.", "$.csv.split(\",\")")),
        "join" => Some(("join(sep) → string", "Join array of strings.", "$.words.join(\"-\")")),
        "starts_with" => Some(("starts_with(s) → bool", "Prefix test.", "$.name.starts_with(\"Mr \")")),
        "ends_with"   => Some(("ends_with(s) → bool", "Suffix test.", "$.file.ends_with(\".json\")")),
        "replace"     => Some(("replace(from, to) → string", "Replace first occurrence.", "$.s.replace(\"a\", \"b\")")),
        "replace_all" => Some(("replace_all(from, to) → string", "Replace all occurrences.", "$.s.replace_all(\" \", \"_\")")),
        "matches" => Some(("matches(regex) → bool", "Regex match.", "$.email.matches(\"^[a-z]+@\")")),
        "scan"    => Some(("scan(regex) → [string]", "All regex matches.", "$.text.scan(\"\\\\d+\")")),
        "to_number" => Some(("to_number() → number", "Parse string as number.", "$.price.to_number()")),
        "to_bool"   => Some(("to_bool() → bool", "Parse string as boolean.", "$.flag.to_bool()")),
        "len" => Some(("len() → int", "Length of string, array, or object.", "$.books.len()")),
        "includes" | "has" => Some((
            "includes(x) → bool",
            "Membership test: substring in string, element in array, key in object.",
            "$.tags.includes(\"sci-fi\")",
        )),
        "index_of" => Some((
            "index_of(x) → int",
            "First index of x, or -1 if absent.",
            "$.items.index_of(\"needle\")",
        )),
        "last_index_of" => Some((
            "last_index_of(x) → int",
            "Last index of x, or -1.",
            "$.items.last_index_of(\"needle\")",
        )),
        "find" => Some((
            "find(pred) → any",
            "First element matching pred, or null.",
            "$.users.find(.id == 42)",
        )),
        "find_all" => Some((
            "find_all(pred) → array",
            "All elements matching pred.",
            "$.users.find_all(.active)",
        )),
        "find_first" => Some((
            "find_first(pred) → any",
            "First element matching pred, or null. Streaming variant of find.",
            "$.users.find_first(.role == \"admin\")",
        )),
        "find_one" => Some((
            "find_one(pred) → any",
            "Single element matching pred (errors if more than one).",
            "$.users.find_one(.id == 42)",
        )),
        "append" => Some((
            "append(x) → array",
            "Append element to end.",
            "$.items.append(\"new\")",
        )),
        "prepend" => Some((
            "prepend(x) → array",
            "Prepend element to front.",
            "$.items.prepend(\"first\")",
        )),
        "remove" => Some((
            "remove(x) → array",
            "Remove all equal elements.",
            "$.tags.remove(\"draft\")",
        )),
        "diff" => Some((
            "diff(other) → array",
            "Elements in self not in other.",
            "$.a.diff($.b)",
        )),
        "intersect" => Some((
            "intersect(other) → array",
            "Common elements of self and other.",
            "$.a.intersect($.b)",
        )),
        "union" => Some((
            "union(other) → array",
            "Set union (deduped).",
            "$.a.union($.b)",
        )),
        "unique_by" => Some((
            "unique_by(key) → array",
            "Unique by key expression.",
            "$.users.unique_by(.email)",
        )),
        "slice" => Some((
            "slice(start, [end]) → array|string",
            "Subsequence (negative indices count from end).",
            "$.items.slice(0, 5)",
        )),
        "collect" => Some((
            "collect() → array",
            "Materialize lazy sequence into array.",
            "$.stream.collect()",
        )),
        "explode" => Some((
            "explode() → array",
            "Flatten nested arrays into rows (cartesian-like expansion).",
            "$.orders.explode()",
        )),
        "fanout" => Some((
            "fanout(expr, …) → array",
            "Produce parallel projections of each element.",
            "$.users.fanout(.id, .name)",
        )),
        "pivot" => Some((
            "pivot(key, value) → object",
            "Pivot array of records into key→value map.",
            "$.rows.pivot(.k, .v)",
        )),
        "abs"   => Some(("abs() → number", "Absolute value.", "$.n.abs()")),
        "ceil"  => Some(("ceil() → int", "Round up.", "$.x.ceil()")),
        "floor" => Some(("floor() → int", "Round down.", "$.x.floor()")),
        "round" => Some(("round([digits]) → number", "Banker-round to digits.", "$.x.round(2)")),
        "set"       => Some(("set(field, val) → object", "Set field (non-mutating).", "$.user.set(\"active\", true)")),
        "update"    => Some(("update(field, fn) → object", "Apply fn to field value.", "$.user.update(\"age\", lambda a: a + 1)")),
        "get_path"  => Some(("get_path(p) → any", "Read value at path.", "$.get_path([\"store\",\"books\",0])")),
        "set_path"  => Some(("set_path(p, val) → any", "Set value at path.", "$.set_path([\"store\",\"name\"], \"Nova\")")),
        "has_path"  => Some(("has_path(p) → bool", "Check whether path exists.", "$.has_path([\"store\",\"books\"])")),
        "del_path"  => Some(("del_path(p) → any", "Delete value at path.", "$.del_path([\"store\",\"books\",0])")),
        "del_paths" => Some(("del_paths([p, …]) → any", "Delete multiple paths.", "$.del_paths([[\"a\"],[\"b\",\"c\"]])")),
        "trace_path" => Some(("trace_path(p) → array", "Return values along path.", "$.trace_path([\"a\",\"b\"])")),
        "flatten_keys"   => Some(("flatten_keys([sep]) → object", "Flatten nested keys into dotted paths.", "$.obj.flatten_keys(\".\")")),
        "unflatten_keys" => Some(("unflatten_keys([sep]) → object", "Inverse of flatten_keys.", "$.obj.unflatten_keys(\".\")")),
        "walk"     => Some(("walk(fn) → any", "Post-order transform of every subtree.", "$.doc.walk(lambda n: n)")),
        "walk_pre" => Some(("walk_pre(fn) → any", "Pre-order transform of every subtree.", "$.doc.walk_pre(lambda n: n)")),
        "rec"      => Some(("rec(key) → array", "Recursive descent for key.", "$.rec(\"id\")")),
        "deep_find" => Some(("deep_find(pred) → array", "Recursive find of all matching subtrees.", "$.deep_find(.price > 10)")),
        "deep_like" => Some(("deep_like(pattern) → array", "Recursive structural match.", "$.deep_like({\"type\":\"book\"})")),
        "deep_shape" => Some(("deep_shape() → schema", "Infer schema for entire subtree.", "$.deep_shape()")),
        "group_shape" => Some(("group_shape() → object", "Shape grouped by element type.", "$.items.group_shape()")),
        "zip_shape"   => Some(("zip_shape() → schema", "Shape inferred across zipped arrays.", "$.streams.zip_shape()")),
        "schema" => Some(("schema() → schema", "Inferred schema of current value.", "$.schema()")),
        "type"   => Some(("type() → string", "Runtime type name: string|number|array|object|bool|null.", "$.value.type()")),
        "missing" => Some(("missing([keys]) → array", "List required keys missing from object.", "$.user.missing([\"id\",\"email\"])")),
        "or" => Some((
            "or(default) → any",
            "Coalesce: returns receiver unless null/missing, else default.",
            "$.user.email.or(\"unknown@example.com\")",
        )),
        "trim_left"  => Some(("trim_left() → string", "Strip leading whitespace.", "$.s.trim_left()")),
        "trim_right" => Some(("trim_right() → string", "Strip trailing whitespace.", "$.s.trim_right()")),
        "lines" => Some(("lines() → [string]", "Split string on newlines.", "$.text.lines()")),
        "words" => Some(("words() → [string]", "Split string on whitespace.", "$.text.words()")),
        "chars" => Some(("chars() → [string]", "String as array of characters.", "$.s.chars()")),
        "indent" => Some(("indent(n | str) → string", "Indent every line.", "$.s.indent(2)")),
        "dedent" => Some(("dedent() → string", "Remove common leading indentation.", "$.s.dedent()")),
        "repeat" => Some(("repeat(n) → string", "Repeat string n times.", "$.s.repeat(3)")),
        "pad_left"  => Some(("pad_left(n, [ch]) → string", "Pad left to width n.", "$.s.pad_left(8)")),
        "pad_right" => Some(("pad_right(n, [ch]) → string", "Pad right to width n.", "$.s.pad_right(8)")),
        "strip_prefix" => Some(("strip_prefix(s) → string", "Remove leading prefix if present.", "$.name.strip_prefix(\"Mr \")")),
        "strip_suffix" => Some(("strip_suffix(s) → string", "Remove trailing suffix if present.", "$.file.strip_suffix(\".json\")")),
        "url_encode"   => Some(("url_encode() → string", "Percent-encode string.", "$.q.url_encode()")),
        "url_decode"   => Some(("url_decode() → string", "Percent-decode string.", "$.q.url_decode()")),
        "to_base64"    => Some(("to_base64() → string", "Base64 encode string.", "$.payload.to_base64()")),
        "from_base64"  => Some(("from_base64() → string", "Decode base64 string.", "$.token.from_base64()")),
        "html_escape"   => Some(("html_escape() → string", "Escape HTML entities.", "$.text.html_escape()")),
        "html_unescape" => Some(("html_unescape() → string", "Unescape HTML entities.", "$.text.html_unescape()")),
        "from_json" => Some(("from_json() → any", "Parse JSON string.", "$.payload.from_json()")),
        "to_json"   => Some(("to_json([pretty]) → string", "Encode value as JSON.", "$.user.to_json(true)")),
        "to_string" => Some(("to_string() → string", "Stringify value.", "$.n.to_string()")),
        "to_csv"    => Some(("to_csv() → string", "Encode array of records as CSV.", "$.rows.to_csv()")),
        "to_tsv"    => Some(("to_tsv() → string", "Encode array of records as TSV.", "$.rows.to_tsv()")),
        "implode" => Some(("implode(sep) → string", "Join array of strings.", "$.words.implode(\" \")")),

        // ── case conversion ─────────────────────────────────────────────────
        "camel_case"  => Some(("camel_case() → string",  "Convert to camelCase.",  "$.field.camel_case()  // \"user_id\" -> \"userId\"")),
        "snake_case"  => Some(("snake_case() → string",  "Convert to snake_case.", "$.field.snake_case()  // \"userId\" -> \"user_id\"")),
        "kebab_case"  => Some(("kebab_case() → string",  "Convert to kebab-case.", "$.field.kebab_case()  // \"userId\" -> \"user-id\"")),
        "pascal_case" => Some(("pascal_case() → string", "Convert to PascalCase.", "$.field.pascal_case() // \"user_id\" -> \"UserId\"")),

        // ── string predicates ──────────────────────────────────────────────
        "is_alpha"   => Some(("is_alpha() → bool",   "True if every char is alphabetic.",            "$.s.is_alpha()")),
        "is_ascii"   => Some(("is_ascii() → bool",   "True if every char is ASCII.",                 "$.s.is_ascii()")),
        "is_blank"   => Some(("is_blank() → bool",   "True if string is empty or whitespace only.",  "$.s.is_blank()")),
        "is_numeric" => Some(("is_numeric() → bool", "True if every char is a digit.",               "$.code.is_numeric()")),

        // ── byte / char level ──────────────────────────────────────────────
        "byte_len"  => Some(("byte_len() → int",     "Length in bytes (UTF-8).",     "$.s.byte_len()")),
        "bytes"     => Some(("bytes() → [int]",      "Array of byte values.",        "$.s.bytes()")),
        "chars_of"  => Some(("chars_of(set) → int",  "Count chars from given set.",  "$.s.chars_of(\"aeiou\")")),
        "center"    => Some(("center(n, [ch]) → string", "Center-pad to width n.",   "$.s.center(20)")),
        "reverse_str" => Some(("reverse_str() → string", "Reverse character order.", "$.s.reverse_str()")),

        // ── regex variants ─────────────────────────────────────────────────
        "re_match"        => Some(("re_match(re) → bool",                  "Regex match (anchored or not — see jetro docs).", "$.email.re_match(\"^[a-z]+@\")")),
        "match_first"     => Some(("match_first(re) → string|null",        "First regex match.",                              "$.text.match_first(\"\\\\d+\")")),
        "match_all"       => Some(("match_all(re) → [string]",             "All regex matches.",                              "$.text.match_all(\"\\\\d+\")")),
        "captures"        => Some(("captures(re) → [string]",              "First match's capture groups.",                   "$.text.captures(\"(\\\\d+)-(\\\\w+)\")")),
        "captures_all"    => Some(("captures_all(re) → [[string]]",        "All matches' capture groups.",                    "$.text.captures_all(\"(\\\\d+)-(\\\\w+)\")")),
        "replace_re"      => Some(("replace_re(re, to) → string",          "Regex replace first match.",                      "$.s.replace_re(\"\\\\d+\", \"#\")")),
        "replace_all_re"  => Some(("replace_all_re(re, to) → string",      "Regex replace all matches.",                      "$.s.replace_all_re(\"\\\\s+\", \" \")")),
        "split_re"        => Some(("split_re(re) → [string]",              "Split string by regex.",                          "$.csv.split_re(\",\\\\s*\")")),

        // ── parsing ────────────────────────────────────────────────────────
        "parse_int"   => Some(("parse_int([base]) → int",       "Parse integer (default base 10).",  "$.s.parse_int(16)")),
        "parse_float" => Some(("parse_float() → float",         "Parse floating-point number.",      "$.s.parse_float()")),
        "parse_bool"  => Some(("parse_bool() → bool",           "Parse \"true\"/\"false\" / yes/no.","$.flag.parse_bool()")),

        // ── indices / search on arrays ─────────────────────────────────────
        "index"        => Some(("index(i) → any",            "Element at index (negative wraps from end).", "$.items.index(-1)")),
        "find_index"   => Some(("find_index(pred) → int",    "Index of first match, or -1.",                              "$.users.find_index(.id == 42)")),
        "indices_of"   => Some(("indices_of(x) → [int]",     "All indices where element equals x.",                       "$.tags.indices_of(\"draft\")")),
        "indices_where"=> Some(("indices_where(pred) → [int]","All indices satisfying pred.",                              "$.nums.indices_where(@ < 0)")),
        "contains_all" => Some(("contains_all(xs) → bool",   "True if every x in xs is present.",                         "$.tags.contains_all([\"a\",\"b\"])")),
        "contains_any" => Some(("contains_any(xs) → bool",   "True if any x in xs is present.",                           "$.tags.contains_any([\"draft\",\"todo\"])")),

        // ── aggregates / by-key ────────────────────────────────────────────
        "min_by" => Some(("min_by(key|lambda) → any", "Element with minimum projected key.", "$.books.min_by(.price)")),
        "max_by" => Some(("max_by(key|lambda) → any", "Element with maximum projected key.", "$.books.max_by(.price)")),

        // ── windowed / running stats ───────────────────────────────────────
        "rolling_sum" => Some(("rolling_sum(n) → [number]", "Rolling window sum of size n.", "$.prices.rolling_sum(7)")),
        "rolling_avg" => Some(("rolling_avg(n) → [number]", "Rolling window average of size n.","$.prices.rolling_avg(7)")),
        "rolling_min" => Some(("rolling_min(n) → [number]", "Rolling window minimum of size n.","$.prices.rolling_min(7)")),
        "rolling_max" => Some(("rolling_max(n) → [number]", "Rolling window maximum of size n.","$.prices.rolling_max(7)")),
        "cummin"      => Some(("cummin() → [number]",       "Cumulative minimum.",              "$.prices.cummin()")),
        "cummax"      => Some(("cummax() → [number]",       "Cumulative maximum.",              "$.prices.cummax()")),
        "diff_window" => Some(("diff_window(n) → [number]", "x[i] − x[i−n] differences.",       "$.prices.diff_window(1)")),
        "pct_change"  => Some(("pct_change([n]) → [number]","Percentage change vs n steps back (default 1).", "$.prices.pct_change()")),
        "lag"         => Some(("lag(n) → [any]",            "Shift series forward by n (pad with null).",     "$.prices.lag(1)")),
        "lead"        => Some(("lead(n) → [any]",           "Shift series backward by n (pad with null).",    "$.prices.lead(1)")),
        "zscore"      => Some(("zscore() → [float]",        "Standard score (x − mean) / stdev.",             "$.prices.zscore()")),

        _ => None,
    };
    match entry {
        Some((sig, summary, example)) =>
            format!("{}\n\n{}\n\nExample:\n  {}", sig, summary, example),
        None => format!("{}(…)\n\njetro builtin.", name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "store": {
                "books": [{"title": "Dune", "price": 12.99, "tags": ["sci-fi"]}],
                "name": "Nova"
            }
        })
    }

    #[test]
    fn field_completion_after_dot() {
        let d = doc();
        let c = complete("$.store.", 8, &d);
        let names: Vec<&str> = c.iter().map(|x| x.text.as_str()).collect();
        assert!(names.contains(&"books"));
        assert!(names.contains(&"name"));
    }

    #[test]
    fn array_completion_shows_element_keys() {
        let d = doc();
        let c = complete("$.store.books.", 14, &d);
        let names: Vec<&str> = c.iter().map(|x| x.text.as_str()).collect();
        assert!(names.iter().any(|n| *n == "title"));
        assert!(names.iter().any(|n| n.starts_with("filter")));
    }

    #[test]
    fn inside_paren_suggests_element_fields() {
        let d = doc();
        let c = complete("$.store.books.filter(", 21, &d);
        let names: Vec<&str> = c.iter().map(|x| x.text.as_str()).collect();
        assert!(names.contains(&"title"));
        assert!(names.contains(&"price"));
    }

    #[test]
    fn prefix_filters_candidates() {
        let d = doc();
        let c = complete("$.store.books.fil", 17, &d);
        let names: Vec<&str> = c.iter().map(|x| x.text.as_str()).collect();
        assert!(names.iter().any(|n| n.starts_with("filter")));
        assert!(!names.iter().any(|n| *n == "map()"));
    }
}
