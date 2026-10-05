use serde_json::{Map, Value};

use super::cursor::{insert_finite, parse_leading_number};

pub fn label_value<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    line.strip_prefix(label)?.strip_prefix(':').map(str::trim)
}

/// Splits `text` on top-level commas (ignoring commas inside `()` and `[]`),
/// yielding trimmed `&str` slices without heap-allocating any intermediate strings.
pub fn balanced_parts(text: &str) -> impl Iterator<Item = &str> {
    BalancedSplit { text, start: 0, depth: 0 }
}

struct BalancedSplit<'a> {
    text: &'a str,
    start: usize,
    depth: i32,
}

impl<'a> Iterator for BalancedSplit<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.start > self.text.len() {
            return None;
        }
        for (i, ch) in self.text[self.start..].char_indices() {
            let abs = self.start + i;
            match ch {
                '(' | '[' => self.depth += 1,
                ')' | ']' => self.depth -= 1,
                ',' if self.depth == 0 => {
                    let slice = self.text[self.start..abs].trim();
                    self.start = abs + 1;
                    return Some(slice);
                }
                _ => {}
            }
        }
        let slice = self.text[self.start..].trim();
        self.start = self.text.len() + 1;
        Some(slice)
    }
}

// All 12 scope×kind combinations as static strings — no allocation per call.
fn buffer_key(scope: &str, kind: &str) -> &'static str {
    match (scope, kind) {
        ("Shared", "Hit")     => "Shared Hit Blocks",
        ("Shared", "Read")    => "Shared Read Blocks",
        ("Shared", "Dirtied") => "Shared Dirtied Blocks",
        ("Shared", "Written") => "Shared Written Blocks",
        ("Local",  "Hit")     => "Local Hit Blocks",
        ("Local",  "Read")    => "Local Read Blocks",
        ("Local",  "Dirtied") => "Local Dirtied Blocks",
        ("Local",  "Written") => "Local Written Blocks",
        ("Temp",   "Hit")     => "Temp Hit Blocks",
        ("Temp",   "Read")    => "Temp Read Blocks",
        ("Temp",   "Dirtied") => "Temp Dirtied Blocks",
        ("Temp",   "Written") => "Temp Written Blocks",
        _ => "",
    }
}

pub fn parse_buffers(info: &str, node: &mut Map<String, Value>) {
    for group in info.split(',') {
        let mut words = group.split_whitespace();
        let scope = match words.next() {
            Some("shared") => "Shared",
            Some("local") => "Local",
            Some("temp") => "Temp",
            _ => continue,
        };
        for kind in ["Hit", "Read", "Dirtied", "Written"] {
            insert_finite(node, buffer_key(scope, kind), 0.0);
        }
        for part in words {
            if let Some((key, amount)) = part.split_once('=') {
                let label = match key {
                    "hit"     => "Hit",
                    "read"    => "Read",
                    "written" => "Written",
                    "dirtied" => "Dirtied",
                    _ => continue,
                };
                if let Ok(n) = amount.parse::<f64>() {
                    insert_finite(node, buffer_key(scope, label), n);
                }
            }
        }
    }
}

pub fn parse_detail(line: &str, fields: &mut Map<String, Value>) {
    if let Some(rest) = label_value(line, "Buffers") {
        parse_buffers(rest, fields);
        return;
    }
    if let Some(rest) = label_value(line, "WAL") {
        for part in rest.split_whitespace() {
            if let Some((key, amount)) = part.split_once('=') {
                let field = match key {
                    "records" => "WAL Records",
                    "bytes"   => "WAL Bytes",
                    "fpi"     => "WAL FPI",
                    _ => continue,
                };
                if let Ok(n) = amount.parse::<f64>() {
                    insert_finite(fields, field, n);
                }
            }
        }
        return;
    }
    if let Some(rest) = label_value(line, "Sort Method") {
        for (kind, separator) in [("Memory", "  Memory:"), ("Disk", "  Disk:")] {
            if let Some((method, size)) = rest.rsplit_once(separator) {
                fields.insert("Sort Method".into(), Value::from(method.trim()));
                fields.insert("Sort Space Type".into(), Value::from(kind));
                if let Some(amount) = parse_leading_number(size) {
                    insert_finite(fields, "Sort Space Used", amount);
                }
                return;
            }
        }
    }
    if let Some(rest) = label_value(line, "I/O Timings") {
        let mut scope = "";
        for part in rest.split_whitespace() {
            match part.trim_end_matches(',') {
                "shared" => { scope = "Shared "; continue; }
                "local"  => { scope = "Local ";  continue; }
                "temp"   => { scope = "Temp ";   continue; }
                _ => {}
            }
            if let Some((op, amount)) = part.split_once('=') {
                let field = match (scope, op) {
                    ("",        "read")  => "I/O Read Time",
                    ("",        "write") => "I/O Write Time",
                    ("Shared ", "read")  => "Shared I/O Read Time",
                    ("Shared ", "write") => "Shared I/O Write Time",
                    ("Local ",  "read")  => "Local I/O Read Time",
                    ("Local ",  "write") => "Local I/O Write Time",
                    ("Temp ",   "read")  => "Temp I/O Read Time",
                    ("Temp ",   "write") => "Temp I/O Write Time",
                    _ => continue,
                };
                if let Ok(n) = amount.parse::<f64>() {
                    insert_finite(fields, field, n);
                }
            }
        }
        return;
    }
    if let Some(rest) = label_value(line, "Settings") {
        let mut map = Map::new();
        for entry in balanced_parts(rest) {
            if let Some((key, val)) = entry.split_once('=') {
                map.insert(
                    key.trim().to_owned(),
                    Value::from(val.trim().trim_matches('\'')),
                );
            }
        }
        fields.insert("Settings".into(), Value::Object(map));
        return;
    }
    if let Some((key, rest)) = line.split_once(':') {
        let raw = rest.trim().trim_end_matches(" ms");
        let property = match key.trim() {
            "Planning time" | "Planning Time" => "Planning Time",
            "Execution time" | "Execution Time" | "Total runtime" => "Execution Time",
            other => other,
        };
        if ["Sort Key", "Presorted Key", "Output", "Group Key", "Hash Key"].contains(&property) {
            fields.insert(
                property.to_owned(),
                Value::Array(balanced_parts(raw).map(Value::from).collect()),
            );
        } else if let Ok(n) = raw.parse::<f64>() {
            insert_finite(fields, property, n);
        } else {
            fields.insert(property.to_owned(), Value::from(rest.trim()));
        }
    }
}
