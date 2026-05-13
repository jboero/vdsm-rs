//! Build-time codegen for vdsm-schema.
//!
//! Reads `<workspace>/schema/vdsm-api.yml`, walks the type and verb tables,
//! and emits typed Rust into `$OUT_DIR/generated.rs`. The crate's `lib.rs`
//! `include!`s that file.
//!
//! Two non-obvious things going on:
//!
//! 1. **Anchor/alias pre-pass.** vdsm-api.yml uses `&Foo` / `*Foo`
//!    extensively, with the anchor name always equal to the type's
//!    `name:` field. We strip anchors and rewrite aliases to bare strings
//!    *before* feeding the YAML to the parser, which lets us avoid the
//!    rabbit hole of getting a generic Rust YAML parser to expose anchor
//!    tables. After the pass, every `type: *Foo` reads as `type: Foo`,
//!    which our codegen treats as a named type ref.
//!
//! 2. **Duplicate keys.** The upstream schema (which is consumed by
//!    PyYAML) has a handful of duplicate keys inside property entries —
//!    PyYAML silently last-wins; serde_yaml errors. yaml-rust2 silently
//!    last-wins (its `Hash` is a `linked-hash-map` that replaces on
//!    insert), matching the original semantics, so we use it.
//!
//! Codegen rules (deliberately permissive — engine schema can grow new
//! optional fields without breaking us):
//!
//!   - object  -> `pub struct` with serde derives, every field
//!                `Option<T>` and `#[serde(default)]`.
//!   - enum    -> `pub enum`, `#[serde(rename = "...")]` on variants
//!                whose Rust ident differs from the YAML key.
//!   - alias   -> `pub type Foo = SourceType;`
//!   - union   -> `#[serde(untagged)] pub enum`, variants V0..VN.
//!   - map     -> `pub type Foo = HashMap<K, V>;`
//!
//! Verbs become a `Verb` enum + per-verb `<Verb>Request` struct (and
//! `<Verb>Response` type alias when the schema declares a return type).

use std::collections::HashSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};
use yaml_rust2::yaml::Hash as YHash;
use yaml_rust2::Yaml;

/// Mirror of [`yaml_rust2::yaml::YamlLoader`] that performs **silent
/// last-wins** when a mapping has duplicate keys, instead of erroring.
/// This matches PyYAML's behavior, which the upstream `vdsm-api.yml`
/// implicitly relies on (it ships with several genuine duplicates that
/// PyYAML quietly overwrites).
#[derive(Default)]
struct LenientLoader {
    docs: Vec<Yaml>,
    doc_stack: Vec<(Yaml, usize)>,
    key_stack: Vec<Yaml>,
    anchor_map: std::collections::BTreeMap<usize, Yaml>,
}

impl LenientLoader {
    fn insert_node(&mut self, node: (Yaml, usize)) {
        if node.1 > 0 {
            self.anchor_map.insert(node.1, node.0.clone());
        }
        if self.doc_stack.is_empty() {
            self.doc_stack.push(node);
            return;
        }
        let parent = self.doc_stack.last_mut().unwrap();
        match parent {
            (Yaml::Array(v), _) => v.push(node.0),
            (Yaml::Hash(h), _) => {
                let cur_key = self.key_stack.last_mut().unwrap();
                if cur_key.is_badvalue() {
                    *cur_key = node.0;
                } else {
                    let mut newkey = Yaml::BadValue;
                    std::mem::swap(&mut newkey, cur_key);
                    // Last-wins: overwrite without error on duplicate.
                    let _ = h.insert(newkey, node.0);
                }
            }
            _ => unreachable!(),
        }
    }
}

impl MarkedEventReceiver for LenientLoader {
    fn on_event(&mut self, ev: Event, _mark: Marker) {
        match ev {
            Event::DocumentStart | Event::Nothing | Event::StreamStart | Event::StreamEnd => {}
            Event::DocumentEnd => match self.doc_stack.len() {
                0 => self.docs.push(Yaml::BadValue),
                1 => self.docs.push(self.doc_stack.pop().unwrap().0),
                _ => unreachable!(),
            },
            Event::SequenceStart(aid, _) => {
                self.doc_stack.push((Yaml::Array(Vec::new()), aid));
            }
            Event::SequenceEnd => {
                let node = self.doc_stack.pop().unwrap();
                self.insert_node(node);
            }
            Event::MappingStart(aid, _) => {
                self.doc_stack.push((Yaml::Hash(YHash::new()), aid));
                self.key_stack.push(Yaml::BadValue);
            }
            Event::MappingEnd => {
                self.key_stack.pop().unwrap();
                let node = self.doc_stack.pop().unwrap();
                self.insert_node(node);
            }
            Event::Scalar(v, style, aid, tag) => {
                let node = scalar_to_yaml(v, style, tag);
                self.insert_node((node, aid));
            }
            Event::Alias(id) => {
                let n = self
                    .anchor_map
                    .get(&id)
                    .cloned()
                    .unwrap_or(Yaml::BadValue);
                self.insert_node((n, 0));
            }
        }
    }
}

fn scalar_to_yaml(v: String, style: TScalarStyle, tag: Option<Tag>) -> Yaml {
    if style != TScalarStyle::Plain {
        return Yaml::String(v);
    }
    if let Some(Tag { handle, suffix }) = tag {
        if handle == "tag:yaml.org,2002:" {
            return match suffix.as_str() {
                "bool" => match v.as_str() {
                    "true" | "True" | "TRUE" => Yaml::Boolean(true),
                    "false" | "False" | "FALSE" => Yaml::Boolean(false),
                    _ => Yaml::BadValue,
                },
                "int" => v
                    .parse::<i64>()
                    .map(Yaml::Integer)
                    .unwrap_or(Yaml::BadValue),
                "float" => {
                    if v.parse::<f64>().is_ok() {
                        Yaml::Real(v)
                    } else {
                        Yaml::BadValue
                    }
                }
                "null" => match v.as_ref() {
                    "~" | "null" => Yaml::Null,
                    _ => Yaml::BadValue,
                },
                _ => Yaml::String(v),
            };
        }
        return Yaml::String(v);
    }
    Yaml::from_str(&v)
}

const PRIMITIVES: &[(&str, &str)] = &[
    ("string", "String"),
    ("str", "String"),
    ("int", "i32"),
    ("uint", "u32"),
    ("long", "i64"),
    ("ulong", "u64"),
    ("float", "f64"),
    ("double", "f64"),
    ("boolean", "bool"),
    ("bool", "bool"),
    ("dict", "serde_json::Value"),
    ("any", "serde_json::Value"),
    // The schema occasionally uses these kind keywords as a property
    // type, meaning "any value" / unspecified. Map them through to the
    // dynamic JSON value type rather than emitting a phantom
    // `Object`/`Enum`/etc. that won't resolve.
    ("object", "serde_json::Value"),
    ("enum", "serde_json::Value"),
    ("alias", "serde_json::Value"),
    ("union", "serde_json::Value"),
    ("map", "serde_json::Value"),
];

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
    "super", "trait", "true", "type", "unsafe", "use", "where", "while",
    "async", "await", "dyn", "abstract", "become", "box", "do", "final",
    "macro", "override", "priv", "typeof", "unsized", "virtual", "yield",
    "try", "union",
];

fn is_keyword(s: &str) -> bool {
    RUST_KEYWORDS.contains(&s)
}

fn map_primitive(s: &str) -> Option<&'static str> {
    PRIMITIVES.iter().find(|(k, _)| *k == s).map(|(_, v)| *v)
}

fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if up {
                out.extend(c.to_uppercase());
                up = false;
            } else {
                out.push(c);
            }
        } else {
            up = true;
        }
    }
    if out.is_empty() {
        return "Unnamed".into();
    }
    if out.chars().next().unwrap().is_ascii_digit() {
        return format!("V{out}");
    }
    out
}

fn snake(s: &str) -> String {
    let mut out = String::new();
    let mut prev_lower_or_digit = false;
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower_or_digit && !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(c.to_lowercase());
            prev_lower_or_digit = false;
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_lower_or_digit = c.is_ascii_lowercase() || c.is_ascii_digit();
        } else {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            prev_lower_or_digit = false;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    while out.starts_with('_') {
        out.remove(0);
    }
    if out.is_empty() {
        return "field".into();
    }
    if out.chars().next().unwrap().is_ascii_digit() {
        return format!("f_{out}");
    }
    out
}

fn rust_field_ident(yaml_name: &str) -> (String, bool) {
    let snk = snake(yaml_name);
    let needs_rename = snk != yaml_name;
    let ident = if is_keyword(&snk) {
        format!("r#{snk}")
    } else {
        snk
    };
    (ident, needs_rename)
}

fn rust_variant_ident(yaml_key: &str) -> (String, bool) {
    let mut v = pascal(yaml_key);
    if is_keyword(&v) {
        v.push('_');
    }
    let needs_rename = v != yaml_key;
    (v, needs_rename)
}

fn esc_doc(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip YAML anchor declarations (`&Foo`) and inline aliases (`*Foo`) by
/// rewriting them to bare identifiers as plain strings. This gives the
/// downstream parser an alias-free document where every `type: *Foo`
/// reads as `type: Foo` (a YAML string we then pascal-case).
fn strip_anchors_and_aliases(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if (b == b'&' || b == b'*') && i + 1 < bytes.len() {
            let n = bytes[i + 1];
            // Anchor/alias names start with letter or underscore.
            if n.is_ascii_alphabetic() || n == b'_' {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() {
                    let c = bytes[j];
                    if c.is_ascii_alphanumeric() || c == b'_' {
                        j += 1;
                    } else {
                        break;
                    }
                }
                let ident = &input[start..j];
                if b == b'*' {
                    // Alias -> bare identifier (parses as plain string).
                    out.push_str(ident);
                }
                // Anchor declaration: skip both `&` and the name; the
                // following value (mapping/scalar) is preserved in place.
                i = j;
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
}

/// Resolve a property's `type:` field to a Rust type expression.
fn resolve_type(node: &Yaml) -> String {
    match node {
        Yaml::String(s) => map_primitive(s)
            .map(String::from)
            .unwrap_or_else(|| pascal(s)),
        Yaml::Array(arr) => {
            if let Some(first) = arr.first() {
                format!("Vec<{}>", resolve_type(first))
            } else {
                "Vec<serde_json::Value>".into()
            }
        }
        Yaml::Hash(h) => {
            // Should not happen post-pre-pass, but stay defensive.
            if let Some(name) = h
                .get(&Yaml::String("name".into()))
                .and_then(|n| n.as_str())
            {
                pascal(name)
            } else {
                "serde_json::Value".into()
            }
        }
        _ => "serde_json::Value".into(),
    }
}

#[derive(Debug, Clone)]
struct PropDef {
    yaml_name: String,
    rust_type: String,
    doc: Option<String>,
}

#[derive(Debug, Clone)]
enum TypeKind {
    Object { properties: Vec<PropDef> },
    Enum { variants: Vec<(String, String)> },
    Alias { source: String },
    Union { variants: Vec<String> },
    Map { key: String, value: String },
}

#[derive(Debug, Clone)]
struct TypeDef {
    name: String,
    kind: TypeKind,
    doc: Option<String>,
    added: Option<String>,
}

#[derive(Debug, Clone)]
struct VerbDef {
    yaml_name: String,
    rust_name: String,
    doc: Option<String>,
    added: Option<String>,
    params: Vec<PropDef>,
    ret: Option<String>,
}

fn yaml_get<'a>(node: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    node.as_hash()
        .and_then(|h| h.get(&Yaml::String(key.into())))
}

fn yaml_str<'a>(node: &'a Yaml, key: &str) -> Option<&'a str> {
    yaml_get(node, key).and_then(|n| n.as_str())
}

fn parse_property(node: &Yaml) -> Option<PropDef> {
    let name = yaml_str(node, "name")?;
    let type_node = yaml_get(node, "type")?;
    let rust_type = resolve_type(type_node);
    let doc = yaml_str(node, "description").map(esc_doc);
    Some(PropDef {
        yaml_name: name.into(),
        rust_type,
        doc,
    })
}

fn parse_type_entry(yaml_key: &str, def: &Yaml) -> Option<TypeDef> {
    let doc = yaml_str(def, "description").map(esc_doc);
    let added = yaml_str(def, "added").map(String::from);
    // The schema occasionally has a yaml_key that differs from the inner
    // `name:` field (e.g. `ImageTicketInfo` -> `name: TicketInfo`). All
    // anchor references in the schema use the yaml_key form, so that's
    // what we treat as canonical for the Rust type name.
    let rust_name = pascal(yaml_key);

    let kind_str = yaml_str(def, "type");
    let has_props = yaml_get(def, "properties")
        .and_then(|n| n.as_vec())
        .is_some();

    let kind = match kind_str {
        Some("object") => {
            let props = yaml_get(def, "properties")
                .and_then(|n| n.as_vec())
                .map(|v| v.iter().filter_map(parse_property).collect())
                .unwrap_or_default();
            TypeKind::Object { properties: props }
        }
        Some("enum") => {
            let mut variants = Vec::new();
            if let Some(values) = yaml_get(def, "values").and_then(|n| n.as_hash()) {
                for (k, v) in values {
                    let key = match k {
                        Yaml::String(s) => s.clone(),
                        Yaml::Integer(i) => i.to_string(),
                        Yaml::Real(r) => r.clone(),
                        Yaml::Boolean(b) => b.to_string(),
                        _ => continue,
                    };
                    let desc = match v {
                        Yaml::String(s) => esc_doc(s),
                        _ => String::new(),
                    };
                    variants.push((key, desc));
                }
            }
            TypeKind::Enum { variants }
        }
        Some("alias") => {
            let src = yaml_get(def, "sourcetype")
                .map(resolve_type)
                .unwrap_or_else(|| "serde_json::Value".into());
            TypeKind::Alias { source: src }
        }
        Some("union") => {
            let variants = yaml_get(def, "values")
                .and_then(|n| n.as_vec())
                .map(|v| v.iter().map(resolve_type).collect())
                .unwrap_or_default();
            TypeKind::Union { variants }
        }
        Some("map") => {
            let key = yaml_get(def, "key-type")
                .map(resolve_type)
                .unwrap_or_else(|| "String".into());
            let value = yaml_get(def, "value-type")
                .map(resolve_type)
                .unwrap_or_else(|| "serde_json::Value".into());
            TypeKind::Map { key, value }
        }
        // `type: <SomeOtherTypeName>` -> treat as a type alias. The schema
        // uses this idiom for "a list of X" where the description carries
        // the list-ness but the structural type is just X.
        Some(other) if !other.is_empty() => {
            let src = map_primitive(other)
                .map(String::from)
                .unwrap_or_else(|| pascal(other));
            TypeKind::Alias { source: src }
        }
        // `type:` missing entirely. If we have properties, default to
        // object — several upstream entries (Lldp, MultipathStatus,
        // ScreenshotResponse, ...) drop the trailing `type: object` line.
        None if has_props => {
            let props = yaml_get(def, "properties")
                .and_then(|n| n.as_vec())
                .map(|v| v.iter().filter_map(parse_property).collect())
                .unwrap_or_default();
            TypeKind::Object { properties: props }
        }
        _ => return None,
    };

    Some(TypeDef {
        name: rust_name,
        kind,
        doc,
        added,
    })
}

fn parse_verb(yaml_name: &str, def: &Yaml) -> Option<VerbDef> {
    if def.as_hash().is_none() {
        return None;
    }
    let doc = yaml_str(def, "description").map(esc_doc);
    let added = yaml_str(def, "added").map(String::from);
    let params = yaml_get(def, "params")
        .and_then(|n| n.as_vec())
        .map(|v| v.iter().filter_map(parse_property).collect())
        .unwrap_or_default();
    let ret = yaml_get(def, "return")
        .and_then(|r| yaml_get(r, "type"))
        .map(resolve_type);
    Some(VerbDef {
        yaml_name: yaml_name.into(),
        rust_name: pascal(yaml_name),
        doc,
        added,
        params,
        ret,
    })
}

fn emit_doc(out: &mut String, doc: Option<&str>, added: Option<&str>) {
    let mut wrote = false;
    if let Some(d) = doc {
        if !d.is_empty() {
            for line in d.split('\n') {
                writeln!(out, "/// {}", line.trim()).unwrap();
                wrote = true;
            }
        }
    }
    if let Some(a) = added {
        if !a.is_empty() {
            if wrote {
                writeln!(out, "///").unwrap();
            }
            writeln!(out, "/// (added in oVirt {a})").unwrap();
        }
    }
}

fn emit_indent_doc(out: &mut String, indent: &str, doc: Option<&str>) {
    if let Some(d) = doc {
        if !d.is_empty() {
            writeln!(out, "{indent}/// {d}").unwrap();
        }
    }
}

fn emit_type(out: &mut String, t: &TypeDef) {
    emit_doc(out, t.doc.as_deref(), t.added.as_deref());
    match &t.kind {
        TypeKind::Object { properties } => {
            writeln!(
                out,
                "#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]"
            )
            .unwrap();
            writeln!(out, "pub struct {} {{", t.name).unwrap();
            let mut seen: HashSet<String> = HashSet::new();
            for p in properties {
                let (ident, needs_rename) = rust_field_ident(&p.yaml_name);
                let bare = ident.trim_start_matches("r#").to_string();
                if !seen.insert(bare) {
                    continue;
                }
                emit_indent_doc(out, "    ", p.doc.as_deref());
                if needs_rename {
                    writeln!(out, "    #[serde(rename = \"{}\")]", p.yaml_name).unwrap();
                }
                writeln!(
                    out,
                    "    #[serde(default, skip_serializing_if = \"Option::is_none\")]"
                )
                .unwrap();
                writeln!(out, "    pub {ident}: Option<{}>,", p.rust_type).unwrap();
            }
            writeln!(out, "}}\n").unwrap();
        }
        TypeKind::Enum { variants } => {
            writeln!(
                out,
                "#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]"
            )
            .unwrap();
            writeln!(out, "pub enum {} {{", t.name).unwrap();
            let mut seen: HashSet<String> = HashSet::new();
            for (key, desc) in variants {
                let (ident, needs_rename) = rust_variant_ident(key);
                if !seen.insert(ident.clone()) {
                    continue;
                }
                if !desc.is_empty() {
                    writeln!(out, "    /// {desc}").unwrap();
                }
                if needs_rename {
                    writeln!(out, "    #[serde(rename = \"{key}\")]").unwrap();
                }
                writeln!(out, "    {ident},").unwrap();
            }
            writeln!(out, "}}\n").unwrap();
        }
        TypeKind::Alias { source } => {
            writeln!(out, "pub type {} = {source};\n", t.name).unwrap();
        }
        TypeKind::Union { variants } => {
            writeln!(
                out,
                "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"
            )
            .unwrap();
            writeln!(out, "#[serde(untagged)]").unwrap();
            writeln!(out, "pub enum {} {{", t.name).unwrap();
            for (i, v) in variants.iter().enumerate() {
                writeln!(out, "    V{i}({v}),").unwrap();
            }
            writeln!(out, "}}\n").unwrap();
        }
        TypeKind::Map { key, value } => {
            writeln!(
                out,
                "pub type {} = std::collections::HashMap<{key}, {value}>;\n",
                t.name
            )
            .unwrap();
        }
    }
}

fn emit_verb_table(out: &mut String, verbs: &[VerbDef]) {
    writeln!(out, "/// Every verb defined by vdsm-api.yml.").unwrap();
    writeln!(out, "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]").unwrap();
    writeln!(out, "#[non_exhaustive]").unwrap();
    writeln!(out, "pub enum Verb {{").unwrap();
    let mut seen: HashSet<String> = HashSet::new();
    for v in verbs {
        if !seen.insert(v.rust_name.clone()) {
            continue;
        }
        writeln!(out, "    {},", v.rust_name).unwrap();
    }
    writeln!(out, "}}\n").unwrap();

    writeln!(out, "impl Verb {{").unwrap();
    writeln!(out, "    pub const fn as_str(&self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    let mut seen2: HashSet<String> = HashSet::new();
    for v in verbs {
        if !seen2.insert(v.rust_name.clone()) {
            continue;
        }
        writeln!(
            out,
            "            Verb::{} => \"{}\",",
            v.rust_name, v.yaml_name
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}\n").unwrap();

    writeln!(out, "    pub fn from_wire(s: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match s {{").unwrap();
    let mut seen3: HashSet<String> = HashSet::new();
    for v in verbs {
        if !seen3.insert(v.rust_name.clone()) {
            continue;
        }
        writeln!(
            out,
            "            \"{}\" => Some(Verb::{}),",
            v.yaml_name, v.rust_name
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}\n").unwrap();
}

fn emit_verb_request_structs(out: &mut String, verbs: &[VerbDef]) {
    let mut seen: HashSet<String> = HashSet::new();
    for v in verbs {
        if !seen.insert(v.rust_name.clone()) {
            continue;
        }
        emit_doc(out, v.doc.as_deref(), v.added.as_deref());
        writeln!(
            out,
            "#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]"
        )
        .unwrap();
        writeln!(out, "pub struct {}Request {{", v.rust_name).unwrap();
        let mut field_seen: HashSet<String> = HashSet::new();
        for p in &v.params {
            let (ident, needs_rename) = rust_field_ident(&p.yaml_name);
            let bare = ident.trim_start_matches("r#").to_string();
            if !field_seen.insert(bare) {
                continue;
            }
            emit_indent_doc(out, "    ", p.doc.as_deref());
            if needs_rename {
                writeln!(out, "    #[serde(rename = \"{}\")]", p.yaml_name).unwrap();
            }
            writeln!(
                out,
                "    #[serde(default, skip_serializing_if = \"Option::is_none\")]"
            )
            .unwrap();
            writeln!(out, "    pub {ident}: Option<{}>,", p.rust_type).unwrap();
        }
        writeln!(out, "}}\n").unwrap();

        if let Some(ret) = &v.ret {
            writeln!(
                out,
                "/// Response payload for `{}`.\npub type {}Response = {ret};\n",
                v.yaml_name, v.rust_name
            )
            .unwrap();
        }
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("vdsm-schema must live two levels deep in the workspace");
    let schema_path = workspace_root.join("schema/vdsm-api.yml");

    println!("cargo::rerun-if-changed={}", schema_path.display());
    println!("cargo::rerun-if-changed=build.rs");

    let raw = fs::read_to_string(&schema_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", schema_path.display()));
    let prepped = strip_anchors_and_aliases(&raw);

    let mut loader = LenientLoader::default();
    let mut parser = Parser::new(prepped.chars());
    parser
        .load(&mut loader, true)
        .unwrap_or_else(|e| panic!("parse {}: {e}", schema_path.display()));
    let root = loader
        .docs
        .first()
        .unwrap_or_else(|| panic!("empty YAML in {}", schema_path.display()));
    let root_hash = root
        .as_hash()
        .unwrap_or_else(|| panic!("root of vdsm-api.yml is not a mapping"));

    let mut types: Vec<TypeDef> = Vec::new();
    let mut verbs: Vec<VerbDef> = Vec::new();
    let mut type_seen: HashSet<String> = HashSet::new();

    for (k, v) in root_hash {
        let Some(key) = k.as_str() else { continue };
        if key == "types" {
            if let Some(types_hash) = v.as_hash() {
                for (n, tdef) in types_hash {
                    let Some(yaml_key) = n.as_str() else { continue };
                    if let Some(td) = parse_type_entry(yaml_key, tdef) {
                        if type_seen.insert(td.name.clone()) {
                            types.push(td);
                        }
                    }
                }
            }
        } else if key.contains('.') {
            if let Some(vd) = parse_verb(key, v) {
                verbs.push(vd);
            }
        }
    }

    let mut out = String::with_capacity(1 << 20);
    writeln!(out, "// Auto-generated from schema/vdsm-api.yml. DO NOT EDIT.").unwrap();
    writeln!(out, "// Regenerate via `cargo build` after vendoring a new schema.").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "/// Number of named types emitted from vdsm-api.yml.").unwrap();
    writeln!(out, "pub const TYPE_COUNT: usize = {};", types.len()).unwrap();
    writeln!(out, "/// Number of verbs emitted from vdsm-api.yml.").unwrap();
    writeln!(out, "pub const VERB_COUNT: usize = {};", verbs.len()).unwrap();
    writeln!(out).unwrap();

    writeln!(out, "/// Sorted list of all type names from the schema.").unwrap();
    writeln!(out, "pub const TYPES: &[&str] = &[").unwrap();
    let mut sorted_types: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
    sorted_types.sort_unstable();
    for n in sorted_types {
        writeln!(out, "    \"{n}\",").unwrap();
    }
    writeln!(out, "];\n").unwrap();

    writeln!(out, "/// Sorted list of all verb names from the schema.").unwrap();
    writeln!(out, "pub const VERBS: &[&str] = &[").unwrap();
    let mut sorted_verbs: Vec<&str> = verbs.iter().map(|v| v.yaml_name.as_str()).collect();
    sorted_verbs.sort_unstable();
    for n in sorted_verbs {
        writeln!(out, "    \"{n}\",").unwrap();
    }
    writeln!(out, "];\n").unwrap();

    for t in &types {
        emit_type(&mut out, t);
    }

    emit_verb_table(&mut out, &verbs);
    emit_verb_request_structs(&mut out, &verbs);

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("generated.rs"), &out)
        .unwrap_or_else(|e| panic!("write generated.rs: {e}"));

    println!(
        "cargo::warning=vdsm-schema: emitted {} types, {} verbs",
        types.len(),
        verbs.len()
    );
}
