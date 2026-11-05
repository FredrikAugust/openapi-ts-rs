//! TypeScript text helpers, mirroring openapi-typescript 6.7.0's `utils.js`.

use serde_json::Value;

pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

fn js_number(n: &serde_json::Number) -> String {
    match n.as_f64() {
        Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e21 => format!("{}", f as i128),
        _ => n.to_string(),
    }
}

/// `String(value)` for the scalars specs put in titles, enums and the like.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => js_number(n),
        Value::Array(a) => {
            a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(",")
        }
        Value::Object(_) => "[object Object]".into(),
        other => other.to_string(),
    }
}

/// A TypeScript literal type for a JSON value.
pub fn literal(v: &Value) -> String {
    match v {
        Value::Number(n) => js_number(n),
        other => other.to_string(),
    }
}

/// A string literal type. Unlike 6.7.0 this escapes backslashes and keeps newlines as `\n`.
pub fn str_lit(s: &str) -> String {
    Value::String(s.into()).to_string()
}

pub fn key(k: &str) -> String {
    let b = k.as_bytes();
    let digits = !b.is_empty() && b.iter().all(u8::is_ascii_digit);
    let ident = !b.is_empty()
        && !b[0].is_ascii_digit()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$');
    if digits || ident { k.to_string() } else { str_lit(k) }
}

pub fn indent(s: &str, lv: usize) -> String {
    format!("{}{s}", "  ".repeat(lv))
}

pub fn comment(text: &str, lv: usize) -> String {
    let text = text.trim().replace("*/", "*\\/");
    if !text.contains('\n') {
        return format!("/** {text} */");
    }
    let star = indent(" *", lv);
    let body: Vec<String> = text
        .lines()
        .map(|l| if l.trim_end().is_empty() { star.clone() } else { format!("{star} {}", l.trim_end()) })
        .collect();
    format!("/**\n{}\n{}", body.join("\n"), indent(" */", lv))
}

pub fn schema_comment(v: &Value, lv: usize) -> Option<String> {
    let o = v.as_object()?;
    let mut out = vec![];
    for f in ["title", "summary"] {
        if truthy(o.get(f)) {
            out.push(js_string(&o[f]));
        }
    }
    if truthy(o.get("format")) {
        out.push(format!("Format: {}", js_string(&o["format"])));
    }
    if truthy(o.get("deprecated")) {
        out.push("@deprecated".into());
    }
    for f in ["description", "default", "example"] {
        match o.get(f) {
            None => {}
            Some(Value::String(s)) if s.is_empty() && f == "description" => {}
            Some(v @ (Value::Object(_) | Value::Array(_) | Value::Null)) => {
                out.push(format!("@{f} {}", serde_json::to_string_pretty(v).unwrap()))
            }
            Some(v) => out.push(format!("@{f} {}", js_string(v))),
        }
    }
    if o.contains_key("const") {
        out.push("@constant".into());
    }
    if truthy(o.get("enum")) {
        let ty = match o.get("type") {
            Some(Value::Array(a)) => a.iter().map(js_string).collect::<Vec<_>>().join("|"),
            Some(Value::String(s)) => s.clone(),
            _ => "unknown".into(),
        };
        let null = if truthy(o.get("nullable")) { "|null" } else { "" };
        out.push(format!("@enum {{{ty}{null}}}"));
    }
    (!out.is_empty()).then(|| comment(&out.join("\n"), lv))
}

fn paren(t: &str) -> String {
    if t.contains(['&', '|']) || t.starts_with("readonly ") { format!("({t})") } else { t.to_string() }
}

pub fn array_of(t: &str) -> String {
    format!("{}[]", paren(t))
}

pub fn intersection(types: Vec<String>) -> String {
    let types: Vec<String> = types.into_iter().filter(|t| t != "unknown").collect();
    match types.len() {
        0 => "unknown".into(),
        1 => types.into_iter().next().unwrap(),
        _ => types.iter().map(|t| paren(t)).collect::<Vec<_>>().join(" & "),
    }
}

pub fn union(types: Vec<String>) -> String {
    let mut members = indexmap::IndexSet::new();
    for t in types {
        if t == "unknown" {
            return t;
        }
        if t != "never" {
            members.insert(t);
        }
    }
    match members.len() {
        0 => "never".into(),
        1 => members.into_iter().next().unwrap(),
        _ => members.iter().map(|t| paren(t)).collect::<Vec<_>>().join(" | "),
    }
}
