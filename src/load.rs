//! Loads a spec and every local file it references, and decides how each external file is emitted.

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// How an external file is rendered under `external[...]`: openapi-typescript 6.7.0's "hint".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// An OpenAPI document (or fragment of one) rendered as `paths`/`webhooks`/`components`/`$defs`.
    Document,
    /// A map of schemas (`file.yml#/Name` refs), rendered entry by entry.
    SchemaMap,
    Schema,
    Parameter,
    ParameterList,
    RequestBody,
    Response,
    MediaType,
    Operation,
}

pub struct File {
    pub doc: Value,
    pub kind: Kind,
    pub path: PathBuf,
}

/// Parsed files by path, or the error that reading or parsing them produced.
pub type Obj = serde_json::Map<String, Value>;

type Parsed = HashMap<PathBuf, Result<Value, String>>;

pub struct Spec {
    pub root: PathBuf,
    /// Every file reachable from the root, parsed in parallel ahead of the ordered pass.
    parsed: Parsed,
    /// Keyed by path relative to the root file's directory; the root itself is `"."`.
    /// Ordered depth-first in reference order, like openapi-typescript's loader.
    pub files: IndexMap<String, File>,
}

pub struct Ref {
    /// Target file id (`"."` for the root).
    pub file: String,
    /// Decoded JSON pointer segments.
    pub pointer: Vec<String>,
}

pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

fn relative(from_dir: &Path, to: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_string(); from.len() - common];
    parts.extend(to[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(n) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(n);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits `file.yml#/a/b~1c` into the file part and decoded pointer segments.
pub fn split_ref(r: &str) -> (&str, Vec<String>) {
    let (file, frag) = r.split_once('#').unwrap_or((r, ""));
    let pointer = frag
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.contains(['%', '~']) { percent_decode(s).replace("~1", "/").replace("~0", "~") } else { s.to_string() }
        })
        .collect();
    (file, pointer)
}

/// A location segment; array indices matter to the hint rules.
#[derive(Clone)]
enum Seg {
    K(String),
    I,
}

fn seg(p: &[Seg], i: usize) -> &str {
    match p.get(i) {
        Some(Seg::K(k)) => k,
        _ => "",
    }
}

/// Port of 6.7.0's `getHint`: the kind of a file referenced from location `p` (which ends with
/// the ref's file name and pointer) inside a file of kind `start`.
fn get_hint(p: &[Seg], start: Kind) -> Kind {
    let t = |n: usize| p.get(n..).unwrap_or(&[]);
    fn schema(p: &[Seg]) -> Kind {
        match seg(p, 0) {
            "allOf" | "anyOf" | "oneOf" => Kind::SchemaMap,
            _ if p.len() >= 2 => Kind::SchemaMap,
            _ => Kind::Schema,
        }
    }
    fn media(p: &[Seg]) -> Kind {
        if seg(p, 0) == "schema" { schema(&p[1..]) } else { Kind::MediaType }
    }
    fn tail(p: &[Seg], n: usize) -> &[Seg] {
        p.get(n..).unwrap_or(&[])
    }
    fn body(p: &[Seg]) -> Kind {
        if seg(p, 0) == "content" { media(tail(p, 2)) } else { Kind::RequestBody }
    }
    fn response(p: &[Seg]) -> Kind {
        match seg(p, 0) {
            "headers" => schema(tail(p, 2)),
            "content" => media(tail(p, 2)),
            _ => Kind::Response,
        }
    }
    fn operation(p: &[Seg]) -> Kind {
        match seg(p, 0) {
            "parameters" => Kind::ParameterList,
            "requestBody" => body(tail(p, 1)),
            "responses" => response(tail(p, 2)),
            _ => Kind::Operation,
        }
    }
    fn parameter(p: &[Seg]) -> Kind {
        match seg(p, 0) {
            "content" => media(tail(p, 2)),
            "schema" => schema(tail(p, 1)),
            _ => Kind::Parameter,
        }
    }
    fn path_item(p: &[Seg]) -> Kind {
        match (seg(p, 0), p.get(1)) {
            ("parameters", Some(Seg::I)) => Kind::ParameterList,
            ("parameters", _) => parameter(tail(p, 1)),
            _ => operation(tail(p, 1)),
        }
    }
    match start {
        Kind::Document => {}
        Kind::Operation => return operation(p),
        Kind::RequestBody => return body(p),
        Kind::Response => return response(p),
        Kind::SchemaMap => return Kind::Schema,
        k => return k,
    }
    match seg(p, 0) {
        "paths" => {
            let s2 = seg(p, 2).to_ascii_lowercase();
            if [".yaml", ".yml", ".json"].iter().any(|e| s2.contains(e)) { Kind::SchemaMap } else { path_item(t(2)) }
        }
        "components" => match seg(p, 1) {
            "schemas" | "headers" => schema(t(3)),
            "parameters" => parameter(t(3)),
            "responses" => response(t(3)),
            "requestBodies" => body(t(3)),
            "pathItems" => path_item(t(3)),
            _ => Kind::Schema,
        },
        _ => Kind::Document,
    }
}

impl Spec {
    pub fn load(input: &Path) -> Result<Spec> {
        let root = normalize(&std::env::current_dir()?.join(input));
        let mut spec = Spec { root: root.clone(), parsed: parse_all(&root), files: IndexMap::new() };
        spec.load_file(&root, Kind::Document)?;
        Ok(spec)
    }

    pub fn id_of(&self, path: &Path) -> String {
        if path == self.root { ".".into() } else { relative(self.root.parent().unwrap(), path) }
    }

    fn load_file(&mut self, path: &Path, kind: Kind) -> Result<()> {
        let id = self.id_of(path);
        if self.files.contains_key(&id) {
            return Ok(());
        }
        let mut doc = match self.parsed.remove(path) {
            Some(Ok(doc)) => doc,
            Some(Err(e)) => bail!(e),
            None => bail!("{} was not loaded", path.display()),
        };
        if kind == Kind::Document
            && let Some(Value::Object(c)) = doc.get_mut("components")
        {
            c.shift_remove("examples");
        }
        let mut refs = vec![];
        collect_refs(&doc, &mut vec![], &mut Default::default(), &mut refs);
        self.files.insert(id, File { doc: Value::Null, kind, path: path.to_path_buf() });
        for (at, r) in &refs {
            let (file, pointer) = split_ref(r);
            if file.is_empty() {
                continue;
            }
            if file.contains("://") || file.starts_with("//") {
                bail!("{}: remote $ref {r} is not supported", path.display());
            }
            let target = normalize(&path.parent().unwrap().join(percent_decode(file)));
            let child = match pointer.first().map(String::as_str) {
                Some("paths" | "components") => Kind::Document,
                _ => {
                    let mut p = at.clone();
                    p.push(Seg::K(file.to_string()));
                    p.extend(pointer.iter().map(|s| Seg::K(s.clone())));
                    get_hint(&p, kind)
                }
            };
            self.load_file(&target, child).with_context(|| format!("$ref {r} in {}", path.display()))?;
        }
        let id = self.id_of(path);
        self.files[&id].doc = doc;
        Ok(())
    }

    /// Resolves a `$ref` string found in file `from`.
    pub fn parse_ref(&self, from: &str, r: &str) -> Ref {
        let (file, pointer) = split_ref(r);
        let file = if file.is_empty() {
            from.to_string()
        } else {
            let base = &self.files[from].path;
            self.id_of(&normalize(&base.parent().unwrap().join(percent_decode(file))))
        };
        Ref { file, pointer }
    }

    pub fn resolve(&self, r: &Ref) -> Option<&Value> {
        let mut v = &self.files.get(&r.file)?.doc;
        for seg in &r.pointer {
            v = match v {
                Value::Object(o) => o.get(seg)?,
                Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(v)
    }
}

fn ref_target(from: &Path, r: &str) -> Option<PathBuf> {
    let (file, _) = r.split_once('#').unwrap_or((r, ""));
    if file.is_empty() || file.contains("://") || file.starts_with("//") {
        return None;
    }
    Some(normalize(&from.parent().unwrap().join(percent_decode(file))))
}

fn read_and_parse(path: &Path) -> Result<Value, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut doc = crate::yaml::parse(&src).map_err(|e| format!("parsing {}: {e:#}", path.display()))?;
    strip_x_refs(&mut doc);
    Ok(doc)
}

/// Distinct file parts of the external `$ref`s in `v`.
fn ref_files<'a>(v: &'a Value, out: &mut indexmap::IndexSet<&'a str>) {
    match v {
        Value::Object(o) => {
            if let Some(r) = o.get("$ref").and_then(Value::as_str) {
                let file = r.split_once('#').map_or(r, |(f, _)| f);
                if !file.is_empty() {
                    out.insert(file);
                }
            }
            o.values().for_each(|x| ref_files(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| ref_files(x, out)),
        _ => {}
    }
}

/// Reads and parses `root` and every file it references, transitively, in parallel.
fn parse_all(root: &Path) -> Parsed {
    fn visit<'s>(scope: &rayon::Scope<'s>, path: PathBuf, out: &'s Mutex<Parsed>) {
        let doc = read_and_parse(&path);
        let mut targets = vec![];
        if let Ok(d) = &doc {
            let mut files = indexmap::IndexSet::new();
            ref_files(d, &mut files);
            targets.extend(files.into_iter().filter_map(|f| ref_target(&path, f)));
        }
        let mut seen = out.lock().unwrap();
        seen.insert(path.clone(), doc);
        for t in targets {
            {
                if !seen.contains_key(&t) {
                    seen.insert(t.clone(), Err(String::new()));
                    scope.spawn(move |s| visit(s, t, out));
                }
            }
        }
    }
    let out = Mutex::new(HashMap::new());
    out.lock().unwrap().insert(root.to_path_buf(), Err(String::new()));
    rayon::scope(|s| visit(s, root.to_path_buf(), &out));
    out.into_inner().unwrap()
}

fn is_x_ref(r: &str) -> bool {
    r.contains("/x-") && split_ref(r).1.iter().any(|p| p.starts_with("x-"))
}

/// 6.7.0 drops refs into `x-` extensions from compositions, and deletes other such refs that
/// point into another file, leaving the rest of the object.
fn strip_x_refs(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for k in ["allOf", "anyOf", "oneOf"] {
                if let Some(Value::Array(items)) = o.get_mut(k) {
                    items.retain(|i| !matches!(i.get("$ref"), Some(Value::String(r)) if is_x_ref(r)));
                }
            }
            if matches!(o.get("$ref"), Some(Value::String(r)) if is_x_ref(r) && !r.starts_with('#')) {
                o.shift_remove("$ref");
            }
            o.values_mut().for_each(strip_x_refs);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_x_refs),
        _ => {}
    }
}

/// The first `$ref` to each other file, with its location. Only the first matters: it decides
/// the file's kind, and later refs to the same file find it already loaded.
fn collect_refs<'a>(
    v: &'a Value,
    path: &mut Vec<Seg>,
    seen: &mut std::collections::HashSet<&'a str>,
    out: &mut Vec<(Vec<Seg>, String)>,
) {
    match v {
        Value::Object(o) => {
            if let Some(r) = o.get("$ref").and_then(Value::as_str) {
                let file = r.split_once('#').map_or(r, |(f, _)| f);
                if !file.is_empty() && seen.insert(file) {
                    out.push((path.clone(), r.to_string()));
                }
            }
            for (k, x) in o {
                path.push(Seg::K(k.clone()));
                collect_refs(x, path, seen, out);
                path.pop();
            }
        }
        Value::Array(a) => {
            for x in a {
                path.push(Seg::I);
                collect_refs(x, path, seen, out);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Visits every object in `v` with its path.
pub fn walk<'a>(v: &'a Value, path: &mut Vec<String>, f: &mut dyn FnMut(&'a Obj, &[String])) {
    match v {
        Value::Object(o) => {
            f(o, path);
            for (k, x) in o {
                path.push(k.clone());
                walk(x, path, f);
                path.pop();
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(i.to_string());
                walk(x, path, f);
                path.pop();
            }
        }
        _ => {}
    }
}
