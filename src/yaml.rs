//! Builds a `serde_json::Value` straight from granit-parser events, resolving scalars the way
//! js-yaml's default schema does (YAML 1.2 core booleans and nulls, large integers as floats)
//! and applying `<<` merge keys. About 3x faster than going through serde.

use anyhow::{Result, bail};
use granit_parser::{Event, Parser, ScalarStyle};
use serde_json::{Map, Number, Value};

fn number(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 {
        Value::Number(Number::from(f as i64))
    } else {
        Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
}

fn digits(s: &str, radix: u32) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_digit(radix))
}

/// js-yaml 4's `resolveYamlFloat` pattern.
fn is_float(s: &str) -> bool {
    let exp_ok = |e: &str| {
        e.is_empty()
            || e.strip_prefix(['e', 'E']).is_some_and(|e| {
                digits(e.trim_start_matches(['+', '-']), 10) && e.len() - e.trim_start_matches(['+', '-']).len() <= 1
            })
    };
    if let Some(rest) = s.strip_prefix('.') {
        let end = rest.find(['e', 'E']).unwrap_or(rest.len());
        return digits(&rest[..end], 10) && exp_ok(&rest[end..]);
    }
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let int_end = body.find(|c: char| !c.is_ascii_digit()).unwrap_or(body.len());
    if int_end == 0 {
        return false;
    }
    let rest = &body[int_end..];
    let rest = match rest.strip_prefix('.') {
        Some(r) => r.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => rest,
    };
    exp_ok(rest)
}

/// Resolves a plain scalar like js-yaml 4's default schema: no `_` in numbers, no YAML 1.1
/// booleans, and no timestamps (6.7.0 turns those into `Date`s; they stay strings here).
fn plain(s: &str) -> Value {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        // Infinity and NaN, which JSON (and so 6.7.0's output) renders as null.
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" | "-.inf" | "-.Inf" | "-.INF" | ".nan" | ".NaN"
        | ".NAN" => {
            return Value::Null;
        }
        _ => {}
    }
    let (sign, body) = match s.as_bytes()[0] {
        b'-' => (-1.0, &s[1..]),
        b'+' => (1.0, &s[1..]),
        _ => (1.0, s),
    };
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(d) = body.strip_prefix(prefix).filter(|d| digits(d, radix)) {
            return u128::from_str_radix(d, radix).map_or(Value::String(s.into()), |n| number(sign * n as f64));
        }
    }
    if let Some(f) = s.parse::<f64>().ok().filter(|f| is_float(s) && f.is_finite()) {
        return number(f);
    }
    Value::String(s.into())
}

enum Frame {
    Seq(Vec<Value>, usize),
    Map(Map<String, Value>, Option<String>, usize, Vec<String>),
}

fn key_string(v: Value) -> String {
    match v {
        Value::String(s) => s,
        Value::Null => "null".into(),
        other => crate::ts::js_string(&other),
    }
}

/// js-yaml accepts C1 control characters that YAML forbids, so they are carried through the
/// parser as private-use characters and mapped back in scalars.
const C1_SHIFT: u32 = 0x10_FF00;

fn is_c1(c: char) -> bool {
    ('\u{80}'..='\u{9f}').contains(&c) && c != '\u{85}'
}

fn restore_c1(s: &str) -> String {
    let range = C1_SHIFT + 0x80..=C1_SHIFT + 0x9f;
    s.chars()
        .map(|c| if range.contains(&(c as u32)) { char::from_u32(c as u32 - C1_SHIFT).unwrap() } else { c })
        .collect()
}

pub fn parse(src: &str) -> Result<Value> {
    if !src.contains(is_c1) {
        return parse_events(src, false);
    }
    let shifted: String =
        src.chars().map(|c| if is_c1(c) { char::from_u32(c as u32 + C1_SHIFT).unwrap() } else { c }).collect();
    parse_events(&shifted, true)
}

fn parse_events(src: &str, c1: bool) -> Result<Value> {
    let mut stack: Vec<Frame> = vec![];
    let mut anchors: Vec<Value> = vec![];
    let mut root = None;
    for ev in Parser::new_from_str(src) {
        let (ev, span) = ev?;
        let (value, anchor) = match ev {
            Event::Scalar(s, style, anchor, tag) => {
                let s = if c1 { restore_c1(&s).into() } else { s };
                let as_str = style != ScalarStyle::Plain || tag.is_some_and(|t| t.suffix() == "str");
                (if as_str { Value::String(s.into_owned()) } else { plain(&s) }, anchor)
            }
            Event::SequenceStart(_, anchor, _) => {
                stack.push(Frame::Seq(vec![], anchor));
                continue;
            }
            Event::MappingStart(_, anchor, _) => {
                stack.push(Frame::Map(Map::new(), None, anchor, vec![]));
                continue;
            }
            Event::SequenceEnd => match stack.pop() {
                Some(Frame::Seq(a, anchor)) => (Value::Array(a), anchor),
                _ => unreachable!(),
            },
            Event::MappingEnd => match stack.pop() {
                Some(Frame::Map(o, _, anchor, _)) => (Value::Object(o), anchor),
                _ => unreachable!(),
            },
            Event::Alias(id) => match anchors.get(id) {
                Some(v) => (v.clone(), 0),
                None => bail!("line {}: unknown alias", span.start.line()),
            },
            _ => continue,
        };
        if anchor > 0 {
            if anchors.len() <= anchor {
                anchors.resize(anchor + 1, Value::Null);
            }
            anchors[anchor] = value.clone();
        }
        match stack.last_mut() {
            None => root = Some(value),
            Some(Frame::Seq(a, _)) => a.push(value),
            Some(Frame::Map(o, pending, _, merged)) => match pending.take() {
                None => *pending = Some(key_string(value)),
                Some(k) if k == "<<" => {
                    let sources = match value {
                        Value::Array(a) => a,
                        v => vec![v],
                    };
                    for src in sources {
                        let Value::Object(m) = src else {
                            bail!("line {}: merge value is not a mapping", span.start.line())
                        };
                        for (k, v) in m {
                            if !o.contains_key(&k) {
                                merged.push(k.clone());
                                o.insert(k, v);
                            }
                        }
                    }
                }
                Some(k) => {
                    // Keys written after `<<` override merged ones.
                    if o.contains_key(&k) && !merged.contains(&k) {
                        bail!("line {}: duplicated mapping key {k:?}", span.start.line());
                    }
                    o.insert(k, value);
                }
            },
        }
    }
    Ok(root.unwrap_or(Value::Null))
}
