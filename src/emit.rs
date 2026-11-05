//! Emits openapi-typescript 6.7.0's output shape. Each method mirrors a `transform/*.js` file;
//! behavior that changes the resulting types is kept, including 6.7.0's quirks.

use crate::load::{Kind, Spec, split_ref, walk};
use crate::ts::*;
use indexmap::IndexMap;
use rayon::prelude::*;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

type Obj = Map<String, Value>;
const METHODS: [&str; 8] = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];

struct Disc {
    property: String,
    mapping: Option<Obj>,
}

struct Discriminators {
    /// Keyed by the TS index of the schema that declares them.
    by_index: HashMap<String, Disc>,
    /// 6.7.0 adds `prop: "value"` to any schema whose `#/components/schemas/Name` path appears in a
    /// discriminator's `oneOf` as written, but only when the spec has external files.
    injected: Vec<(Vec<String>, String)>,
}

/// Emits one file at a time; `fork` gives each external file its own state so they can be
/// emitted in parallel.
pub struct Gen<'a> {
    spec: &'a Spec,
    file: RefCell<String>,
    discs: Arc<Discriminators>,
    operations: RefCell<IndexMap<String, (String, Option<String>)>>,
    ref_cache: RefCell<HashMap<(String, String), String>>,
    pub errors: RefCell<Vec<String>>,
    used_one_of: Cell<bool>,
    used_with_required: Cell<bool>,
}

/// openapi-typescript's `/^\s*\{?\s*\}?\s*$/`.
fn is_empty_object(s: &str) -> bool {
    let t = s.trim();
    let t = t.strip_prefix('{').unwrap_or(t).trim_start();
    t.strip_prefix('}').unwrap_or(t).trim().is_empty()
}

fn obj(v: &Value) -> Option<&Obj> {
    v.as_object()
}
fn is_ts_literal(s: &str) -> bool {
    matches!(s, "true" | "false" | "null")
        || (!s.is_empty()
            && s.parse::<f64>().is_ok()
            && !s.contains(|c: char| c.is_alphabetic() && c != 'e' && c != 'E'))
}

/// `allOf`/`anyOf`/`oneOf` items.
fn comp<'v>(o: &'v Obj, k: &str) -> Option<Vec<&'v Value>> {
    Some(o.get(k)?.as_array()?.iter().collect())
}

impl<'a> Gen<'a> {
    pub fn new(spec: &'a Spec) -> Self {
        let mut discs = HashMap::new();
        let mut injected = vec![];
        for (id, f) in &spec.files {
            walk(&f.doc, &mut vec![], &mut |o, path| {
                let Some(d) = o.get("discriminator").filter(|d| truthy(Some(d))) else {
                    return;
                };
                let property = d.get("propertyName").map(js_string).unwrap_or_else(|| "undefined".into());
                let mapping = d.get("mapping").and_then(obj).cloned();
                let refs: Vec<String> = o
                    .get("oneOf")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c.get("$ref")?.as_str().map(String::from))
                    .collect();
                if spec.files.len() > 1 && !refs.is_empty() {
                    injected.push((refs, format!("{}\u{0}{}", property, serde_json::to_string(&mapping).unwrap())));
                }
                // 6.7.0 keys these by the raw path, so a discriminator under `properties` is never found.
                discs.insert(ts_index_raw(id, path), Disc { property, mapping });
            });
        }
        Gen::with(spec, ".", Arc::new(Discriminators { by_index: discs, injected }))
    }

    fn with(spec: &'a Spec, file: &str, discs: Arc<Discriminators>) -> Self {
        Gen {
            spec,
            file: RefCell::new(file.into()),
            discs,
            operations: Default::default(),
            ref_cache: Default::default(),
            errors: Default::default(),
            used_one_of: Cell::new(false),
            used_with_required: Cell::new(false),
        }
    }

    fn ts_ref(&self, r: &str) -> String {
        let file = self.file.borrow();
        let cache_key = (file.clone(), r.to_string());
        if let Some(t) = self.ref_cache.borrow().get(&cache_key) {
            return t.clone();
        }
        let rf = self.spec.parse_ref(&file, r);
        if self.spec.resolve(&rf).is_none() {
            self.errors.borrow_mut().push(format!("{file}: $ref {r} does not resolve"));
        }
        let t = ts_index(&rf.file, &rf.pointer);
        self.ref_cache.borrow_mut().insert(cache_key, t.clone());
        t
    }

    fn disc_for(&self, item: &Value) -> Option<&Disc> {
        if self.discs.by_index.is_empty() {
            return None;
        }
        let r = item.get("$ref")?.as_str()?;
        self.discs.by_index.get(&self.ts_ref(r))
    }

    /// `prop: "value";` for a schema at `path` that a discriminator lists.
    fn disc_line(&self, path: &str, property: &str, mapping: Option<&Obj>) -> String {
        let mut value = path.rsplit('/').next().filter(|_| path.contains("#/")).map(String::from);
        if let Some(m) = mapping {
            let hit = m.iter().find(|(_, v)| {
                let v = v.as_str().unwrap_or_default();
                let last = if v.starts_with('#') {
                    split_ref(v).1.into_iter().rfind(|s| s != "properties")
                } else {
                    Some(v.to_string())
                };
                last == value
            });
            if let Some((k, _)) = hit {
                value = Some(k.clone());
            }
        }
        format!("{}: {};", key(property), value.map(|v| str_lit(&v)).unwrap_or_else(|| "undefined".into()))
    }

    // ---- schema-object.js ---------------------------------------------------------------

    pub fn schema(&self, v: &Value, path: &str, lv: usize) -> String {
        let o = match v {
            Value::Bool(b) => return if *b { "unknown" } else { "never" }.into(),
            Value::Object(o) => o,
            other => return js_string(other),
        };
        if let Some(Value::String(r)) = o.get("$ref") {
            return self.ts_ref(r);
        }
        if let Some(c) = o.get("const").filter(|c| !c.is_null()) {
            return match c {
                Value::String(s) => str_lit(s),
                c => literal(c),
            };
        }
        let ty = o.get("type");
        let ty_str = ty.and_then(Value::as_str);
        let nullable = truthy(o.get("nullable"));
        let with_null = |mut t: Vec<String>| {
            if nullable {
                t.push("null".into());
            }
            union(t)
        };

        if let Some(Value::Array(items)) = o.get("enum").filter(|_| ty_str != Some("object")) {
            let is_string = ty_str == Some("string")
                || ty.and_then(Value::as_array).is_some_and(|a| a.iter().any(|t| t == "string"));
            let lits = items
                .iter()
                .map(|t| match (ty, t) {
                    (None, t) if !truthy(Some(t)) => "\"\"".into(),
                    (None, Value::String(s)) => str_lit(s),
                    (Some(_), Value::String(s)) if is_string => str_lit(s),
                    // 6.7.0 emits these unquoted, which is invalid TS unless it reads as a literal.
                    (Some(_), Value::String(s)) if is_ts_literal(s) => s.clone(),
                    (Some(_), Value::String(s)) => str_lit(s),
                    (_, t) => literal(t),
                })
                .collect();
            return with_null(lits);
        }

        let one_of = if !truthy(o.get("discriminator")) && truthy(o.get("oneOf")) {
            comp(o, "oneOf")
        } else if truthy(o.get("enum")) {
            // 6.7.0 also reads an object schema's `enum` values as `oneOf` members.
            comp(o, "enum")
        } else {
            None
        };
        if let Some(one_of) = one_of.filter(|items| !items.iter().any(|t| self.disc_for(t).is_some())) {
            let mut types: Vec<String> = one_of.iter().map(|t| self.schema(t, path, lv)).collect();
            if nullable {
                types.push("null".into());
            }
            if let Some(Value::Array(tys)) = ty {
                for t in tys {
                    let mut c = o.clone();
                    c.remove("oneOf");
                    c.insert("type".into(), t.clone());
                    types.push(self.schema(&Value::Object(c), path, lv));
                }
                return union(types);
            }
            let one_of_type = if types.iter().any(|t| t.contains('{')) && (2..=5).contains(&types.len()) {
                self.used_one_of.set(true);
                format!("OneOf<[{}]>", types.join(", "))
            } else {
                union(types)
            };
            if ty_str == Some("object") && (truthy(o.get("properties")) || truthy(o.get("additionalProperties"))) {
                let mut c = o.clone();
                c.remove("oneOf");
                c.remove("enum");
                return intersection(vec![self.schema(&Value::Object(c), path, lv), one_of_type]);
            }
            return one_of_type;
        }

        match ty_str {
            Some("null") => return "null".into(),
            Some(t @ ("string" | "boolean")) => return with_null(vec![t.into()]),
            Some("number" | "integer") => return with_null(vec!["number".into()]),
            Some("array") => {
                let tuple = o
                    .get("prefixItems")
                    .filter(|p| truthy(Some(p)))
                    .or_else(|| o.get("items").filter(|i| i.is_array()));
                let mut t = match (tuple, o.get("items").filter(|i| truthy(Some(i)))) {
                    (Some(Value::Array(items)), _) => {
                        format!(
                            "[{}]",
                            items.iter().map(|i| self.schema(i, path, lv + 1)).collect::<Vec<_>>().join(", ")
                        )
                    }
                    (_, Some(items)) => array_of(&self.schema(items, path, lv + 1)),
                    _ => array_of("unknown"),
                };
                if truthy(o.get("readOnly")) {
                    t = format!("readonly {t}");
                }
                return with_null(vec![t]);
            }
            _ => {}
        }
        if let Some(Value::Array(tys)) = ty {
            let parts = tys
                .iter()
                .map(|t| {
                    let mut c = o.clone();
                    c.insert("type".into(), t.clone());
                    self.schema(&Value::Object(c), path, lv)
                })
                .collect();
            return union(parts);
        }

        let mut core: Vec<String> = vec![];
        let listed = || self.discs.injected.iter().find(|(refs, _)| refs.iter().any(|r| r == path)).map(|(_, d)| d);
        for k in ["oneOf", "allOf", "anyOf"] {
            let Some(items) = comp(o, k) else { continue };
            let found =
                items.iter().find(|t| t.get("$ref").is_some() && (self.disc_for(t).is_some() || listed().is_some()));
            if let Some(d) = found.and_then(|t| self.disc_for(t)) {
                core.insert(0, indent(&self.disc_line(path, &d.property, d.mapping.as_ref()), lv + 1));
                break;
            }
        }
        if let Some(d) = listed() {
            let (property, mapping) = d.split_once('\u{0}').unwrap();
            let mapping: Option<Obj> = serde_json::from_str(mapping).unwrap();
            core.insert(0, indent(&self.disc_line(path, property, mapping.as_ref()), lv + 1));
        }

        let props = o.get("properties").and_then(obj);
        let addl = o.get("additionalProperties").filter(|a| truthy(Some(a)));
        let defs = o.get("$defs").and_then(obj).filter(|d| !d.is_empty());
        if props.is_some_and(|p| !p.is_empty()) || addl.is_some() || truthy(o.get("$defs")) {
            let lv1 = lv + 1;
            let required = o.get("required").and_then(Value::as_array);
            for (k, v) in props.into_iter().flatten() {
                if let Some(c) = schema_comment(v, lv1) {
                    core.push(indent(&c, lv1));
                }
                let optional = if required.is_some_and(|r| r.iter().any(|x| x == k)) { "" } else { "?" };
                let ro = if truthy(o.get("readOnly")) { "readonly " } else { "" };
                // `properties: { $ref: x }` is a ref, not a property schema; 6.7.0 emits it as one.
                let t = match (k.as_str(), v) {
                    ("$ref", Value::String(r)) => self.ts_ref(r),
                    _ => self.schema(v, path, lv1),
                };
                core.push(indent(&format!("{ro}{}{optional}: {t};", key(k)), lv1));
            }
            if let Some(addl) = addl {
                let t = match addl {
                    Value::Object(a) if !a.is_empty() => self.schema(addl, path, lv1),
                    _ => "unknown".into(),
                };
                let n = props.map_or(0, |p| p.len());
                let undef = truthy(o.get("properties"))
                    && match o.get("required").filter(|r| truthy(Some(r))) {
                        None => n > 0,
                        Some(r) => Some(n) != r.as_array().map(Vec::len),
                    };
                let t = if undef { union(vec![t, "undefined".into()]) } else { t };
                core.push(indent(&format!("[key: string]: {t};"), lv1));
            }
            if let Some(defs) = defs {
                core.push(indent(&format!("$defs: {};", self.schema_map(defs, &format!("{path}$defs/"), lv1)), lv1));
            }
        }

        let mut out =
            if core.is_empty() { String::new() } else { format!("{{\n{}\n{}", core.join("\n"), indent("}", lv)) };
        let compose = |items: Vec<&Value>| -> Vec<String> {
            items
                .into_iter()
                .map(|i| {
                    let t = self.schema(i, path, lv);
                    match self.disc_for(i) {
                        Some(d) => format!("Omit<{t}, {}>", str_lit(&d.property)),
                        None => t,
                    }
                })
                .collect()
        };
        let with = |out: String, t: String| {
            if out.is_empty() { t } else { intersection(vec![out, t]) }
        };
        match comp(o, "oneOf").filter(|i| !i.is_empty()) {
            Some(items) => out = with(out, union(compose(items))),
            None => {
                if let Some(items) = comp(o, "allOf").filter(|i| !i.is_empty()) {
                    let mut parts = if out.is_empty() { vec![] } else { vec![out] };
                    parts.extend(compose(items));
                    out = intersection(parts);
                    if let Some(Value::Array(req)) = o.get("required") {
                        self.used_with_required.set(true);
                        out = format!("WithRequired<{out}, {}>", union(req.iter().map(literal).collect()));
                    }
                }
                if let Some(items) = comp(o, "anyOf").filter(|i| !i.is_empty()) {
                    out = with(out, union(compose(items)));
                }
            }
        }
        if nullable {
            out = union(vec![if out.is_empty() { "Record<string, unknown>".into() } else { out }, "null".into()]);
        }
        match (out.is_empty(), o.contains_key("type")) {
            (false, _) => out,
            (true, false) => "unknown".into(),
            (true, true) => "Record<string, never>".into(),
        }
    }

    // ---- schema-object-map.js -------------------------------------------------------------

    fn schema_map(&self, map: &Obj, path: &str, lv: usize) -> String {
        if map.get("type").is_some_and(Value::is_string)
            || ["allOf", "oneOf", "anyOf"].iter().any(|k| map.get(*k).is_some_and(Value::is_array))
        {
            return self.schema(&Value::Object(map.clone()), path, lv);
        }
        let lv1 = lv + 1;
        let mut out = vec!["{".to_string()];
        for (name, v) in map {
            let Some(o) = obj(v) else {
                if v.is_array() {
                    out.push(indent(
                        &format!("{}: {};", key(name), self.schema(v, &format!("{path}{name}"), lv1)),
                        lv1,
                    ));
                }
                continue;
            };
            if let Some(c) = schema_comment(v, lv1) {
                out.push(indent(&c, lv1));
            }
            let ro = if truthy(o.get("readOnly")) { "readonly " } else { "" };
            let p = format!("{path}{name}");
            let t = if !o.contains_key("type") && !o.contains_key("$ref") && METHODS.iter().any(|m| o.contains_key(*m))
            {
                self.path_item(o, &p, lv1)
            } else if o.contains_key("in") {
                self.parameter(v, &p, lv1)
            } else {
                self.schema(v, &p, lv1)
            };
            out.push(indent(&format!("{ro}{}: {t};", key(name)), lv1));
        }
        out.push(indent("}", lv));
        out.join("\n")
    }

    // ---- parameter / media type / header / request body / response ------------------------

    fn parameter(&self, p: &Value, path: &str, lv: usize) -> String {
        match p.get("schema").filter(|s| truthy(Some(s))) {
            Some(s) => self.schema(s, path, lv),
            None => "string".into(),
        }
    }

    fn media_type(&self, m: &Value, path: &str, lv: usize) -> String {
        match m.get("schema").filter(|s| truthy(Some(s))) {
            Some(s) => self.schema(s, path, lv),
            None => "unknown".into(),
        }
    }

    fn content_map(&self, content: &Obj, path: &str, lv: usize, comments: bool, refs: bool) -> Vec<String> {
        let mut out = vec![];
        for (ct, m) in content {
            if comments && let Some(c) = schema_comment(m, lv) {
                out.push(indent(&c, lv));
            }
            let p = format!("{path}/{ct}");
            let t = if refs && m.get("$ref").is_some() { self.schema(m, &p, lv) } else { self.media_type(m, &p, lv) };
            out.push(indent(&format!("{}: {t};", str_lit(ct)), lv));
        }
        out
    }

    fn header(&self, h: &Value, path: &str, lv: usize) -> String {
        if let Some(s) = h.get("schema").filter(|s| truthy(Some(s))) {
            return self.schema(s, path, lv);
        }
        match h.get("content").and_then(obj) {
            Some(content) => {
                let mut out = vec!["{".to_string()];
                out.extend(self.content_map(content, path, lv + 1, true, true));
                out.push(indent("}", lv));
                out.join("\n")
            }
            None => "unknown".into(),
        }
    }

    fn request_body(&self, rb: &Value, path: &str, lv: usize) -> String {
        let empty = Obj::new();
        let content = rb.get("content").and_then(obj).unwrap_or(&empty);
        let mut out = vec!["{".to_string(), indent("content: {", lv + 1)];
        if content.is_empty() {
            out.push(indent("\"*/*\": never;", lv + 2));
        }
        out.extend(self.content_map(content, path, lv + 2, true, true));
        out.push(indent("};", lv + 1));
        out.push(indent("}", lv));
        out.join("\n")
    }

    fn response(&self, r: &Value, path: &str, lv: usize) -> String {
        let mut out = vec!["{".to_string()];
        if let Some(headers) = r.get("headers").and_then(obj) {
            out.push(indent("headers: {", lv + 1));
            for (name, h) in headers {
                if let Some(c) = schema_comment(h, lv + 2) {
                    out.push(indent(&c, lv + 2));
                }
                let line = match h.get("$ref").and_then(Value::as_str) {
                    Some(r) => format!("{}: {};", key(name), self.ts_ref(r)),
                    None => {
                        let opt = if truthy(h.get("required")) { "" } else { "?" };
                        format!("{}{opt}: {};", key(name), self.header(h, &format!("{path}/headers/{name}"), lv + 2))
                    }
                };
                out.push(indent(&line, lv + 2));
            }
            out.push(indent("};", lv + 1));
        }
        match r.get("content").and_then(obj) {
            Some(content) => {
                out.push(indent("content: {", lv + 1));
                out.extend(self.content_map(content, &format!("{path}/content"), lv + 2, false, false));
                out.push(indent("};", lv + 1));
            }
            None => out.push(indent("content: never;", lv + 1)),
        }
        out.push(indent("}", lv));
        out.join("\n")
    }

    // ---- operation / path item / paths ---------------------------------------------------

    fn operation(&self, op: &Obj, params: &[&Value], path: &str, lv: usize, wrap: bool) -> String {
        let mut out: Vec<String> = if wrap { vec!["{".into()] } else { vec![] };
        let (lv1, lv2, lv3) = (lv + 1, lv + 2, lv + 3);
        let mut param_out = vec![];
        for loc in ["query", "header", "path", "cookie"] {
            let mut lines = vec![];
            let mut all_optional = true;
            for p in params {
                let r = p.get("$ref").and_then(Value::as_str);
                let node = match r {
                    Some(r) => self.spec.resolve(&self.spec.parse_ref(&self.file.borrow(), r)),
                    None => Some(*p),
                };
                let Some(node) = node.filter(|n| n.get("in").and_then(Value::as_str) == Some(loc)) else {
                    continue;
                };
                let required = loc == "path" || truthy(node.get("required"));
                all_optional &= !required;
                if let Some(c) = schema_comment(p, lv3) {
                    lines.push(indent(&c, lv3));
                }
                let name = node.get("name").map(js_string).unwrap_or_default();
                let t = match r {
                    Some(r) => self.ts_ref(r),
                    None => self.parameter(p, &format!("{path}/parameters/{name}"), lv3),
                };
                lines.push(indent(&format!("{}{}: {t};", key(&name), if required { "" } else { "?" }), lv3));
            }
            if !lines.is_empty() {
                param_out.push(indent(&format!("{loc}{}: {{", if all_optional { "?" } else { "" }), lv2));
                param_out.extend(lines);
                param_out.push(indent("};", lv2));
            }
        }
        if !param_out.is_empty() {
            out.push(indent("parameters: {", lv1));
            out.extend(param_out);
            out.push(indent("};", lv1));
        }
        if let Some(rb) = op.get("requestBody").filter(|v| truthy(Some(v))) {
            if let Some(c) = schema_comment(rb, lv1) {
                out.push(indent(&c, lv1));
            }
            let line = match rb.get("$ref").and_then(Value::as_str) {
                Some(r) => format!("requestBody: {};", self.ts_ref(r)),
                None => {
                    let opt = if truthy(rb.get("required")) { "" } else { "?" };
                    format!("requestBody{opt}: {};", self.request_body(rb, &format!("{path}/requestBody"), lv1))
                }
            };
            out.push(indent(&line, lv1));
        }
        if let Some(responses) = op.get("responses").and_then(obj) {
            out.push(indent("responses: {", lv1));
            for (code, r) in responses {
                if let Some(c) = schema_comment(r, lv2) {
                    out.push(indent(&c, lv2));
                }
                let t = match r.get("$ref").and_then(Value::as_str) {
                    Some(rf) => self.ts_ref(rf),
                    None => self.response(r, &format!("{path}/responses/{code}"), lv2),
                };
                out.push(indent(&format!("{}: {t};", key(code)), lv2));
            }
            out.push(indent("};", lv1));
        }
        if wrap {
            out.push(indent("}", lv));
        }
        out.join("\n")
    }

    fn path_item(&self, item: &Obj, path: &str, lv: usize) -> String {
        let mut out = vec!["{".to_string()];
        let lv1 = lv + 1;
        let shared: Vec<&Value> = item.get("parameters").and_then(Value::as_array).into_iter().flatten().collect();
        for m in METHODS {
            let Some(op) = item.get(m).and_then(obj) else {
                continue;
            };
            if let Some(c) = schema_comment(&item[m], lv1) {
                out.push(indent(&c, lv1));
            }
            if let Some(r) = op.get("$ref").and_then(Value::as_str) {
                out.push(indent(&format!("{m}: {}", self.ts_ref(r)), lv1));
                continue;
            }
            // Operation parameters override path-level ones with the same name or $ref.
            let mut keyed: IndexMap<String, &Value> = IndexMap::new();
            for p in shared.iter().copied().chain(op.get("parameters").and_then(Value::as_array).into_iter().flatten())
            {
                keyed.insert(p.get("$ref").or_else(|| p.get("name")).map(js_string).unwrap_or_default(), p);
            }
            let params: Vec<&Value> = keyed.into_values().collect();
            match op.get("operationId").filter(|i| truthy(Some(i))).map(js_string) {
                Some(id) => {
                    let t = self.operation(op, &params, path, 1, true);
                    self.operations.borrow_mut().insert(id.clone(), (t, schema_comment(&item[m], 1)));
                    out.push(indent(&format!("{m}: operations[{}];", str_lit(&id)), lv1));
                }
                None => out.push(indent(&format!("{m}: {};", self.operation(op, &params, path, lv1, true)), lv1)),
            }
        }
        if !shared.is_empty() {
            out.push(indent(self.operation(&Obj::new(), &shared, path, lv, false).trim(), lv1));
        }
        out.push(indent("}", lv));
        out.join("\n")
    }

    fn paths(&self, paths: &Obj, prefix: &str, lv: usize) -> String {
        let mut out = vec!["{".to_string()];
        for (url, item) in paths {
            let Some(io) = obj(item) else { continue };
            let t = match io.get("$ref").and_then(Value::as_str) {
                Some(r) => {
                    if let Some(c) = schema_comment(item, lv + 1) {
                        out.push(indent(&c, lv + 1));
                    }
                    self.ts_ref(r)
                }
                None => self.path_item(io, &format!("#/{prefix}/{url}"), lv + 1),
            };
            out.push(indent(&format!("{}: {t};", str_lit(url)), lv + 1));
        }
        out.push(indent("}", lv));
        out.join("\n")
    }

    // ---- components-object.js ------------------------------------------------------------

    fn components(&self, c: &Obj, lv: usize) -> String {
        let (lv1, lv2) = (lv + 1, lv + 2);
        let mut out = vec!["{".to_string()];
        let section = |name: &str, out: &mut Vec<String>, f: &dyn Fn(&str, &Value) -> (String, String)| {
            let Some(entries) = c.get(name).filter(|v| truthy(Some(v))) else {
                out.push(indent(&format!("{name}: never;"), lv1));
                return;
            };
            out.push(indent(&format!("{name}: {{"), lv1));
            for (k, v) in entries.as_object().into_iter().flatten() {
                if let Some(cm) = schema_comment(v, lv2) {
                    out.push(indent(&cm, lv2));
                }
                let (suffix, t) = match v.get("$ref").and_then(Value::as_str) {
                    Some(r) => (String::new(), self.ts_ref(r)),
                    None => f(k, v),
                };
                out.push(indent(&format!("{}{suffix}: {t};", key(k)), lv2));
            }
            out.push(indent("};", lv1));
        };
        match c.get("schemas").and_then(obj) {
            Some(s) => {
                out.push(indent(&format!("schemas: {};", self.schema_map(s, "#/components/schemas/", lv1)), lv1))
            }
            None => out.push(indent("schemas: never;", lv1)),
        }
        section("responses", &mut out, &|k, v| {
            (String::new(), self.response(v, &format!("#/components/responses/{k}"), lv2))
        });
        section("parameters", &mut out, &|k, v| {
            let opt = v.get("in").and_then(Value::as_str) != Some("path") && !truthy(v.get("required"));
            (if opt { "?" } else { "" }.into(), self.parameter(v, &format!("#/components/parameters/{k}"), lv2))
        });
        section("requestBodies", &mut out, &|k, v| {
            (
                if truthy(v.get("required")) { "" } else { "?" }.into(),
                self.request_body(v, &format!("#/components/requestBodies/{k}"), lv2),
            )
        });
        section("headers", &mut out, &|k, v| {
            (String::new(), self.header(v, &format!("#/components/headers/{k}"), lv2))
        });
        section("pathItems", &mut out, &|k, v| {
            let empty = Obj::new();
            (String::new(), self.path_item(obj(v).unwrap_or(&empty), &format!("#/components/pathItems/{k}"), lv2))
        });
        out.push(indent("}", lv));
        out.join("\n")
    }

    // ---- documents and the whole file ----------------------------------------------------

    fn document(&self, doc: &Value, lv: usize) -> Vec<(&'static str, String)> {
        let field = |k| doc.get(k).and_then(obj).filter(|o| !o.is_empty());
        vec![
            ("paths", field("paths").map(|p| self.paths(p, "paths", lv))),
            ("webhooks", field("webhooks").map(|w| self.paths(w, "webhooks", lv))),
            ("components", field("components").map(|c| self.components(c, lv))),
            ("$defs", field("$defs").map(|d| self.schema_map(d, "$defs/", lv))),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.unwrap_or_default()))
        .collect()
    }

    /// A `ParameterObject[]` file: a list or map of parameters keyed by name.
    fn parameter_list(&self, v: &Value, path: &str, lv: usize) -> String {
        if v.get("schema").is_some() || v.get("type").is_some() {
            return self.schema(v, path, lv);
        }
        let entries: Vec<(String, &Value)> = match v {
            Value::Array(a) => a.iter().map(|p| (p.get("name").map(js_string).unwrap_or_default(), p)).collect(),
            Value::Object(o) => o.iter().map(|(k, p)| (k.clone(), p)).collect(),
            _ => vec![],
        };
        let mut out = vec!["{".to_string()];
        for (name, p) in entries {
            let node = match p.get("$ref").and_then(Value::as_str) {
                Some(r) => self.spec.resolve(&self.spec.parse_ref(&self.file.borrow(), r)),
                None => Some(p),
            };
            let Some(node) = node else { continue };
            let opt = node.get("in").and_then(Value::as_str) != Some("path") && !truthy(node.get("required"));
            let t = self.parameter(node, &format!("{path}/{name}"), lv + 2);
            out.push(indent(&format!("{}{}: {t};", key(&name), if opt { "?" } else { "" }), lv + 1));
        }
        out.push(indent("}", lv));
        out.join("\n")
    }

    fn fragment(&self, kind: Kind, v: &Value, path: &str, lv: usize) -> (Option<String>, String) {
        let empty = Obj::new();
        let o = obj(v).unwrap_or(&empty);
        let t = match kind {
            Kind::Document => unreachable!(),
            Kind::SchemaMap => self.schema_map(o, path, lv),
            Kind::Schema => self.schema(v, path, lv),
            Kind::Parameter => self.parameter(v, path, lv),
            Kind::ParameterList => self.parameter_list(v, path, lv),
            Kind::RequestBody => self.request_body(v, path, lv),
            Kind::Response => self.response(v, path, lv),
            Kind::MediaType => self.media_type(v, path, lv),
            Kind::Operation => {
                let params: Vec<&Value> = o.get("parameters").and_then(Value::as_array).into_iter().flatten().collect();
                return (schema_comment(v, lv), self.operation(o, &params, path, lv, true));
            }
        };
        (None, t)
    }

    /// The `external` lines for one file.
    fn external_entry(&self, id: &str) -> Vec<String> {
        let f = &self.spec.files[id];
        let (comment, body) = if f.kind == Kind::Document {
            let mut lines = vec!["{".to_string()];
            for (k, v) in self.document(&f.doc, 2) {
                lines.push(indent(
                    &format!("{}: {};", key(k), if is_empty_object(&v) { "Record<string, never>" } else { &v }),
                    2,
                ));
            }
            lines.push(indent("}", 1));
            (None, lines.join("\n"))
        } else {
            self.fragment(f.kind, &f.doc, &format!("{id}#"), 1)
        };
        if is_empty_object(&body) {
            return vec![];
        }
        comment.map(|c| indent(&c, 1)).into_iter().chain([indent(&format!("{}: {body};", str_lit(id)), 1)]).collect()
    }

    pub fn generate(&self) -> String {
        let empty = is_empty_object;
        let mut out: Vec<String> = vec![];
        for (k, v) in self.document(&self.spec.files["."].doc, 0) {
            out.push(if empty(&v) {
                format!("export type {k} = Record<string, never>;\n")
            } else {
                format!("export interface {k} {v}\n")
            });
        }
        let mut ids: Vec<&String> = self.spec.files.keys().filter(|k| *k != ".").collect();
        ids.sort();
        let (spec, discs) = (self.spec, &self.discs);
        let parts: Vec<(Vec<String>, Gen)> = ids
            .par_iter()
            .map(|id| {
                let g = Gen::with(spec, id, discs.clone());
                (g.external_entry(id), g)
            })
            .collect();
        let mut ext = vec![];
        for (lines, g) in parts {
            ext.extend(lines);
            self.operations.borrow_mut().extend(g.operations.into_inner());
            self.errors.borrow_mut().extend(g.errors.into_inner());
            self.used_one_of.set(self.used_one_of.get() || g.used_one_of.get());
            self.used_with_required.set(self.used_with_required.get() || g.used_with_required.get());
        }
        out.push(if ext.is_empty() {
            "export type external = Record<string, never>;\n".into()
        } else {
            format!("export interface external {{\n{}\n}}\n", ext.join("\n"))
        });
        let ops = self.operations.borrow();
        if ops.is_empty() {
            out.push("export type operations = Record<string, never>;\n".into());
        } else {
            let mut lines = vec!["export interface operations {".to_string()];
            for (id, (t, c)) in ops.iter() {
                if let Some(c) = c {
                    lines.push(indent(c, 1));
                }
                lines.push(indent(&format!("{}: {t};", key(id)), 1));
            }
            lines.push("}\n".into());
            out.push(lines.join("\n"));
        }
        let mut header = vec![
            "/**\n * This file was auto-generated by openapi-ts-rs.\n * Do not make direct changes to the file.\n */\n"
                .to_string(),
        ];
        if self.used_with_required.get() {
            header.push("type WithRequired<T, K extends keyof T> = T & { [P in K]-?: T[P] };\n".into());
        }
        if self.used_one_of.get() {
            header.push(
                "type Without<T, U> = { [P in Exclude<keyof T, keyof U>]?: never };\n\
                 type XOR<T, U> = (T | U) extends object ? (Without<T, U> & U) | (Without<U, T> & T) : T | U;\n\
                 type OneOf<T extends any[]> = T extends [infer Only] ? Only : T extends [infer A, infer B, ...infer Rest] ? OneOf<[XOR<A, B>, ...Rest]> : never;\n"
                    .into(),
            );
        }
        header.extend(out);
        header.join("\n")
    }
}

/// The TS lookup for a location: `components["schemas"]["X"]` in the root file,
/// `external["file.yml"][...]` elsewhere. Like 6.7.0, `properties` segments are dropped so a
/// ref to a property becomes an indexed access on its parent.
pub fn ts_index(file: &str, pointer: &[String]) -> String {
    let segs: Vec<String> = pointer.iter().filter(|s| *s != "properties").cloned().collect();
    ts_index_raw(file, &segs)
}

fn ts_index_raw<S: AsRef<str>>(file: &str, segs: &[S]) -> String {
    let idx = |s: &[S]| s.iter().map(|s| format!("[{}]", str_lit(s.as_ref()))).collect::<String>();
    match (file, segs.split_first()) {
        (".", Some((head, rest))) => format!("{}{}", head.as_ref(), idx(rest)),
        (".", None) => "unknown".into(),
        (f, _) => format!("external[{}]{}", str_lit(f), idx(segs)),
    }
}
