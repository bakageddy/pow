//! Native PostgreSQL EXPLAIN parser. PEV2's fixtures are the compatibility
//! reference; no JS engine or regular expressions run in the backend.
//! JSON EXPLAIN uses serde_json; text EXPLAIN uses the byte-oriented FSM below.
use serde_json::{json, Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct RawNode {
    fields: Map<String, Value>,
    children: Vec<RawNode>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    BeforePlan,
    QueryText,
    Plan,
    Planning,
}

struct Frame {
    indent: usize,
    path: Vec<usize>,
}

struct Subplan {
    indent: usize,
    relationship: &'static str,
    name: String,
    child_indent: Option<usize>,
}

// Cursor transitions are driven by bytes; no regex construction/matching.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            at: 0,
        }
    }
    fn skip_space(&mut self) {
        while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }
    fn eat(&mut self, token: &[u8]) -> bool {
        if self
            .bytes
            .get(self.at..)
            .is_some_and(|rest| rest.starts_with(token))
        {
            self.at += token.len();
            true
        } else {
            false
        }
    }
    fn number(&mut self) -> Option<f64> {
        let start = self.at;
        if self
            .bytes
            .get(self.at)
            .is_some_and(|b| *b == b'-' || *b == b'+')
        {
            self.at += 1;
        }
        let mut digits = 0;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
            digits += 1;
        }
        if self.bytes.get(self.at) == Some(&b'.') && self.bytes.get(self.at + 1) != Some(&b'.') {
            self.at += 1;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            self.at = start;
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()?
            .parse()
            .ok()
    }
    fn field(&mut self, name: &[u8]) -> Option<f64> {
        self.skip_space();
        if !self.eat(name) {
            return None;
        }
        self.number()
    }
}
fn value(n: f64) -> Value {
    if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= u64::MAX as f64 {
        Value::from(n as u64)
    } else {
        json!(n)
    }
}
fn put(map: &mut Map<String, Value>, key: &str, n: f64) {
    if n.is_finite() {
        map.insert(key.to_owned(), value(n));
    }
}
fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}
fn meta_number(v: &str) -> Option<f64> {
    let mut cursor = Cursor::new(v.trim_start());
    cursor.number()
}

fn node_prefix(line: &str) -> Option<(&str, &str)> {
    let text = line
        .trim_start()
        .strip_prefix("->")
        .unwrap_or(line.trim_start())
        .trim_start();
    // States: operator name -> cost/actual marker -> numeric fields. A match
    // requires a complete numeric group, not just a parenthesis in SQL text.
    let marker = ["(cost=", "(actual ", "(never executed)"]
        .iter()
        .filter_map(|m| text.find(m))
        .min()?;
    let name = text[..marker].trim();
    if name.is_empty() {
        None
    } else {
        Some((name, &text[marker..]))
    }
}
fn normalize_type(name: &str, fields: &mut Map<String, Value>) {
    let mut name = name.trim();
    for mode in ["Finalize", "Partial", "Simple"] {
        if let Some(rest) = name.strip_prefix(mode).and_then(|s| s.strip_prefix(' ')) {
            fields.insert("Partial Mode".into(), Value::from(mode));
            name = rest;
            break;
        }
    }
    if let Some(rest) = name.strip_prefix("Parallel ") {
        fields.insert("Parallel Aware".into(), Value::Bool(true));
        name = rest;
    }
    if let Some(rest) = name.strip_prefix("Bitmap Index Scan on ") {
        fields.insert("Index Name".into(), Value::from(rest));
        fields.insert("Node Type".into(), Value::from("Bitmap Index Scan"));
        return;
    }
    if let Some(rest) = name.strip_prefix("CTE Scan on ") {
        fields.insert("CTE Name".into(), Value::from(rest));
        fields.insert("Node Type".into(), Value::from("CTE Scan"));
        return;
    }
    if let Some(rest) = name.strip_prefix("Subquery Scan on ") {
        fields.insert("Alias".into(), Value::from(rest));
        fields.insert("Node Type".into(), Value::from("Subquery Scan"));
        return;
    }
    if let Some((scan, rest)) = name.split_once(" using ") {
        if scan.contains("Index") {
            let (index, relation) = rest.split_once(" on ").unwrap_or((rest, ""));
            fields.insert("Index Name".into(), Value::from(index.trim()));
            let scan = scan.strip_suffix(" Backward");
            fields.insert(
                "Scan Direction".into(),
                Value::from(if scan.is_some() {
                    "Backward"
                } else {
                    "Forward"
                }),
            );
            if !relation.is_empty() {
                set_relation(relation, fields);
            }
            name = scan.unwrap_or(scan_name(name));
        }
    } else if let Some((scan, relation)) = name.split_once(" on ") {
        if scan.contains("Scan") || ["Insert", "Update", "Delete", "Merge"].contains(&scan) {
            set_relation(relation, fields);
            name = scan;
        }
    }
    if let Some((kind, modifier)) = name.strip_suffix(" Join").and_then(|s| s.rsplit_once(' ')) {
        if ["Left", "Right", "Full", "Anti", "Semi"].contains(&modifier) {
            fields.insert("Join Type".into(), Value::from(modifier));
            fields.insert("Node Type".into(), Value::from(format!("{kind} Join")));
            return;
        }
    }
    fields.insert("Node Type".into(), Value::from(name));
}
fn scan_name(name: &str) -> &str {
    name.split_once(" using ")
        .map(|(scan, _)| scan)
        .unwrap_or(name)
}
fn set_relation(text: &str, fields: &mut Map<String, Value>) {
    let mut words = text.split_whitespace();
    if let Some(relation) = words.next() {
        fields.insert("Relation Name".into(), Value::from(relation));
        if let Some(alias) = words.next() {
            fields.insert("Alias".into(), Value::from(alias));
        }
    }
}
fn parse_node(name: &str, trailer: &str) -> Option<RawNode> {
    let mut node = RawNode::default();
    let mut cursor = Cursor::new(trailer);
    let mut group_found = false;
    if cursor.eat(b"(cost=") {
        let startup = cursor.number()?;
        if !cursor.eat(b"..") {
            return None;
        }
        let total = cursor.number()?;
        let rows = cursor.field(b"rows=")?;
        let width = cursor.field(b"width=")?;
        if !cursor.eat(b")") {
            return None;
        }
        put(&mut node.fields, "Startup Cost", startup);
        put(&mut node.fields, "Total Cost", total);
        put(&mut node.fields, "Plan Rows", rows);
        put(&mut node.fields, "Plan Width", width);
        group_found = true;
    }
    cursor.skip_space();
    if cursor.eat(b"(actual ") {
        if cursor.eat(b"time=") {
            let startup = cursor.number()?;
            if !cursor.eat(b"..") {
                return None;
            }
            let total = cursor.number()?;
            put(&mut node.fields, "Actual Startup Time", startup);
            put(&mut node.fields, "Actual Total Time", total);
        }
        let rows = cursor.field(b"rows=")?;
        let loops = cursor.field(b"loops=")?;
        cursor.skip_space();
        if !cursor.eat(b")") {
            return None;
        }
        put(&mut node.fields, "Actual Rows", rows);
        put(&mut node.fields, "Actual Loops", loops);
        if rows.fract() != 0.0 {
            node.fields
                .insert("*Actual Rows Is Fractional".into(), Value::Bool(true));
        }
        group_found = true;
    } else if cursor.eat(b"(never executed)") {
        put(&mut node.fields, "Actual Rows", 0.0);
        put(&mut node.fields, "Actual Loops", 0.0);
        group_found = true;
    }
    if !group_found {
        return None;
    }
    normalize_type(name, &mut node.fields);
    Some(node)
}

fn at_path<'a>(root: &'a mut RawNode, path: &[usize]) -> &'a mut RawNode {
    if let Some((&first, rest)) = path.split_first() {
        at_path(&mut root.children[first], rest)
    } else {
        root
    }
}
fn balanced_list(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    for (i, ch) in text.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                result.push(text[start..i].trim().to_owned());
                start = i + 1;
            }
            _ => {}
        }
    }
    result.push(text[start..].trim().to_owned());
    result
}
fn label_value<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    line.strip_prefix(label)?.strip_prefix(':').map(str::trim)
}
fn parse_buffers(info: &str, node: &mut Map<String, Value>) {
    for group in info.split(',') {
        let mut words = group.split_whitespace();
        let scope = match words.next() {
            Some("shared") => "Shared",
            Some("local") => "Local",
            Some("temp") => "Temp",
            _ => continue,
        };
        for kind in ["Hit", "Read", "Dirtied", "Written"] {
            put(node, &format!("{scope} {kind} Blocks"), 0.0);
        }
        for part in words {
            if let Some((key, amount)) = part.split_once('=') {
                let label = match key {
                    "hit" => "Hit",
                    "read" => "Read",
                    "written" => "Written",
                    "dirtied" => "Dirtied",
                    _ => continue,
                };
                if let Ok(n) = amount.parse::<f64>() {
                    put(node, &format!("{scope} {label} Blocks"), n);
                }
            }
        }
    }
}
fn parse_detail(line: &str, fields: &mut Map<String, Value>) {
    if let Some(rest) = label_value(line, "Buffers") {
        parse_buffers(rest, fields);
        return;
    }
    if let Some(rest) = label_value(line, "WAL") {
        for part in rest.split_whitespace() {
            if let Some((key, amount)) = part.split_once('=') {
                let label = match key {
                    "records" => "Records",
                    "bytes" => "Bytes",
                    "fpi" => "FPI",
                    _ => continue,
                };
                if let Ok(n) = amount.parse::<f64>() {
                    put(fields, &format!("WAL {label}"), n);
                }
            }
        }
        return;
    }
    if let Some(rest) = label_value(line, "Sort Method") {
        for kind in ["Memory", "Disk"] {
            if let Some((method, size)) = rest.rsplit_once(&format!("  {kind}:")) {
                fields.insert("Sort Method".into(), Value::from(method.trim()));
                fields.insert("Sort Space Type".into(), Value::from(kind));
                if let Some(amount) = meta_number(size) {
                    put(fields, "Sort Space Used", amount);
                }
                return;
            }
        }
    }
    if let Some(rest) = label_value(line, "I/O Timings") {
        let mut scope = "";
        for part in rest.split_whitespace() {
            match part.trim_end_matches(',') {
                "shared" => {
                    scope = "Shared ";
                    continue;
                }
                "local" => {
                    scope = "Local ";
                    continue;
                }
                "temp" => {
                    scope = "Temp ";
                    continue;
                }
                _ => {}
            }
            if let Some((op, amount)) = part.split_once('=') {
                let suffix = match op {
                    "read" => "Read Time",
                    "write" => "Write Time",
                    _ => continue,
                };
                if let Ok(n) = amount.parse::<f64>() {
                    put(fields, &format!("{scope}I/O {suffix}"), n);
                }
            }
        }
        return;
    }
    if let Some(rest) = label_value(line, "Settings") {
        let mut map = Map::new();
        for entry in balanced_list(rest) {
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
        if [
            "Sort Key",
            "Presorted Key",
            "Output",
            "Group Key",
            "Hash Key",
        ]
        .contains(&property)
        {
            fields.insert(
                property.to_owned(),
                Value::Array(balanced_list(raw).into_iter().map(Value::from).collect()),
            );
        } else if let Ok(n) = raw.parse::<f64>() {
            put(fields, property, n);
        } else {
            fields.insert(property.to_owned(), Value::from(rest.trim()));
        }
    }
}
fn count_indent(line: &str) -> usize {
    line.bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .map(|b| if b == b'\t' { 4 } else { 1 })
        .sum()
}
fn parse_text(source: &str) -> Result<Value, String> {
    let mut phase = Phase::BeforePlan;
    let mut root = None::<RawNode>;
    let mut global = Map::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut subplan: Option<Subplan> = None;
    for original in source.lines() {
        let line = original.trim_end_matches('\r').trim_matches(['\'', '"']);
        let indent = count_indent(line);
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed == "QUERY PLAN"
            || trimmed.starts_with("----")
            || trimmed.starts_with("(1 row")
        {
            continue;
        }
        if let Some(query) = label_value(trimmed, "Query Text") {
            global.insert("Query Text".into(), Value::from(query));
            phase = Phase::QueryText;
            continue;
        }
        if let Some((name, trailer)) = node_prefix(line) {
            if let Some(mut next) = parse_node(name, trailer) {
                if let Some(marker) = subplan.as_mut() {
                    if indent <= marker.indent {
                        subplan = None;
                    } else if marker.child_indent.is_none() || marker.child_indent == Some(indent) {
                        marker.child_indent = Some(indent);
                        next.fields.insert(
                            "Parent Relationship".into(),
                            Value::from(marker.relationship),
                        );
                        next.fields
                            .insert("Subplan Name".into(), Value::from(marker.name.clone()));
                    }
                }
                while stack.last().is_some_and(|f| f.indent >= indent) {
                    stack.pop();
                }
                if let Some(parent) = stack.last() {
                    let path = parent.path.clone();
                    let parent_node = at_path(root.as_mut().ok_or("Plan has no root node")?, &path);
                    let index = parent_node.children.len();
                    parent_node.children.push(next);
                    let mut own_path = path;
                    own_path.push(index);
                    stack.push(Frame {
                        indent,
                        path: own_path,
                    });
                } else if root.is_none() {
                    root = Some(next);
                    stack.push(Frame {
                        indent,
                        path: vec![],
                    });
                } else {
                    return Err("Multiple root nodes in text plan".into());
                }
                phase = Phase::Plan;
                continue;
            }
        }
        if phase == Phase::QueryText && root.is_none() {
            if let Some(text) = global
                .get("Query Text")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                global.insert(
                    "Query Text".into(),
                    Value::from(format!("{text}\n{trimmed}")),
                );
            }
            continue;
        }
        if let Some(name) = trimmed.strip_prefix("CTE ") {
            while stack.last().is_some_and(|f| f.indent >= indent) {
                stack.pop();
            }
            subplan = Some(Subplan {
                indent,
                relationship: "InitPlan",
                name: format!("CTE {name}"),
                child_indent: None,
            });
            continue;
        }
        if trimmed == "InitPlan"
            || trimmed.starts_with("InitPlan ")
            || trimmed == "SubPlan"
            || trimmed.starts_with("SubPlan ")
        {
            while stack.last().is_some_and(|f| f.indent >= indent) {
                stack.pop();
            }
            subplan = Some(Subplan {
                indent,
                relationship: if trimmed.starts_with("Init") {
                    "InitPlan"
                } else {
                    "SubPlan"
                },
                name: trimmed.to_owned(),
                child_indent: None,
            });
            continue;
        }
        if trimmed.starts_with("Trigger ") {
            let entry = trimmed.strip_prefix("Trigger ").unwrap_or(trimmed);
            if let Some((name, rest)) = entry.split_once(":") {
                let time = rest
                    .split_whitespace()
                    .find_map(|t| t.strip_prefix("time="))
                    .and_then(meta_number)
                    .unwrap_or(0.0);
                let calls = rest
                    .split_whitespace()
                    .find_map(|t| t.strip_prefix("calls="))
                    .and_then(meta_number)
                    .unwrap_or(0.0);
                global
                    .entry("Triggers")
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"Trigger Name":name,"Time":time,"Calls":calls.to_string()}));
            }
            continue;
        }
        if [
            "Execution Time:",
            "Planning Time:",
            "Total runtime:",
            "Settings:",
        ]
        .iter()
        .any(|s| trimmed.starts_with(s))
        {
            parse_detail(trimmed, &mut global);
            continue;
        }
        if trimmed == "Planning:" {
            phase = Phase::Planning;
            global.insert("Planning".into(), Value::Object(Map::new()));
            continue;
        }
        if phase == Phase::Planning {
            if let Some(planning) = global.get_mut("Planning").and_then(Value::as_object_mut) {
                parse_detail(trimmed, planning);
            }
            continue;
        }
        if let Some(frame) = stack.last() {
            let fields = &mut at_path(root.as_mut().expect("root"), &frame.path).fields;
            parse_detail(trimmed, fields);
        }
    }
    let root = root.ok_or("Unable to parse plan")?;
    global.insert("Plan".into(), raw_value(root));
    Ok(Value::Object(global))
}
fn raw_value(node: RawNode) -> Value {
    let mut fields = node.fields;
    if !node.children.is_empty() {
        fields.insert(
            "Plans".into(),
            Value::Array(node.children.into_iter().map(raw_value).collect()),
        );
    }
    Value::Object(fields)
}

fn normalize_json_node(node: &mut Value) {
    if let Some(strategy) = node
        .get("Strategy")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        if node.get("Node Type").and_then(Value::as_str) == Some("Aggregate") {
            let prefix = match strategy.as_str() {
                "Sorted" => "Group",
                "Hashed" => "Hash",
                _ => "",
            };
            node["Node Type"] = Value::from(format!("{prefix}Aggregate"));
        }
    }
    if node.get("Node Type").and_then(Value::as_str) == Some("ModifyTable") {
        if let Some(op) = node
            .get("Operation")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            node["Node Type"] = Value::from(op);
        }
    }
}
fn process_node(
    node: &mut Value,
    next_id: &mut u64,
    ctes: &mut Vec<Value>,
    inherited_workers: f64,
) {
    if !node.is_object() {
        return;
    }
    node["nodeId"] = Value::from(*next_id);
    *next_id += 1;
    normalize_json_node(node);
    if let (Some(actual), Some(estimate)) = (num(node, "Actual Rows"), num(node, "Plan Rows")) {
        let (direction, factor) = if actual > estimate {
            (2, actual / estimate)
        } else if actual < estimate {
            (1, estimate / actual)
        } else {
            (3, 1.0)
        };
        node["*Planner Row Estimate Direction"] = Value::from(direction);
        if factor.is_finite() {
            node["*Planner Row Estimate Factor"] = value(factor);
        }
    }
    let planned = num(node, "Workers Launched").unwrap_or(inherited_workers);
    let children = node.as_object_mut().and_then(|o| o.remove("Plans"));
    let mut kept = Vec::new();
    if let Some(Value::Array(children)) = children {
        for mut child in children {
            process_node(&mut child, next_id, ctes, planned);
            if child.get("Parent Relationship").and_then(Value::as_str) == Some("InitPlan")
                && child
                    .get("Subplan Name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.starts_with("CTE"))
            {
                ctes.push(child);
            } else {
                kept.push(child);
            }
        }
    }
    if !kept.is_empty() {
        node["Plans"] = Value::Array(kept);
    }
    let children = node
        .get("Plans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(total) = num(node, "Actual Total Time") {
        let loops = num(node, "Actual Loops").unwrap_or(1.0);
        let revised = total * loops / (inherited_workers + 1.0);
        put(
            node.as_object_mut().expect("node"),
            "Actual Total Time",
            revised,
        );
        if let Some(start) = num(node, "Actual Startup Time") {
            put(
                node.as_object_mut().expect("node"),
                "Actual Startup Time",
                start * loops / (inherited_workers + 1.0),
            );
        }
        let child_time: f64 = children
            .iter()
            .filter(|child| {
                child.get("Parent Relationship").and_then(Value::as_str) != Some("InitPlan")
                    || node.get("Node Type").and_then(Value::as_str) == Some("Result")
            })
            .map(|child| num(child, "Actual Total Time").unwrap_or(0.0))
            .sum();
        put(
            node.as_object_mut().expect("node"),
            "*Duration (exclusive)",
            (revised - child_time).max(0.0),
        );
    }
    if let Some(total) = num(node, "Total Cost") {
        let child_cost: f64 = children
            .iter()
            .map(|child| num(child, "Total Cost").unwrap_or(0.0))
            .sum();
        put(
            node.as_object_mut().expect("node"),
            "*Cost (exclusive)",
            (total - child_cost).max(0.0),
        );
    }
    let loops = num(node, "Actual Loops")
        .filter(|l| *l > 0.0)
        .unwrap_or(1.0);
    for (source, target) in [
        ("Actual Rows", "*Actual Rows Revised"),
        ("Plan Rows", "*Plan Rows Revised"),
        ("Rows Removed by Filter", "*Rows Removed by Filter"),
        (
            "Rows Removed by Join Filter",
            "*Rows Removed by Join Filter",
        ),
        (
            "Rows Removed by Index Recheck",
            "*Rows Removed by Index Recheck",
        ),
    ] {
        if let Some(n) = num(node, source) {
            put(node.as_object_mut().expect("node"), target, n * loops);
        }
    }
    for (source, target) in [
        ("Shared Hit Blocks", "*Shared Hit Blocks (exclusive)"),
        ("Shared Read Blocks", "*Shared Read Blocks (exclusive)"),
        (
            "Shared Dirtied Blocks",
            "*Shared Dirtied Blocks (exclusive)",
        ),
        (
            "Shared Written Blocks",
            "*Shared Written Blocks (exclusive)",
        ),
        ("Temp Read Blocks", "*Temp Read Blocks (exclusive)"),
        ("Temp Written Blocks", "*Temp Written Blocks (exclusive)"),
        ("Local Hit Blocks", "*Local Hit Blocks (exclusive)"),
        ("Local Read Blocks", "*Local Read Blocks (exclusive)"),
        ("Local Dirtied Blocks", "*Local Dirtied Blocks (exclusive)"),
        ("Local Written Blocks", "*Local Written Blocks (exclusive)"),
    ] {
        if let Some(inclusive) = num(node, source) {
            let descendants: f64 = children
                .iter()
                .filter(|c| c.get("Subplan Name").is_none())
                .map(|c| num(c, source).unwrap_or(0.0))
                .sum();
            put(
                node.as_object_mut().expect("node"),
                target,
                (inclusive - descendants).max(0.0),
            );
        }
    }
}
fn walk(node: &Value, visit: &mut impl FnMut(&Value)) {
    visit(node);
    if let Some(children) = node.get("Plans").and_then(Value::as_array) {
        for child in children {
            walk(child, visit);
        }
    }
}
fn from_source(source: &str) -> Result<Value, String> {
    let trimmed = source.trim().trim_start_matches('\u{feff}');
    if let Ok(mut json) = serde_json::from_str::<Value>(trimmed) {
        if let Some(first) = json.as_array_mut().and_then(|list| list.first_mut()) {
            json = first.take();
        }
        if json.get("Plan").is_some() {
            return Ok(json);
        }
    }
    parse_text(source)
}
pub fn parse_plan(source: &str, sql: &str) -> Result<String, String> {
    let mut content = from_source(source)?;
    let mut ctes = Vec::new();
    let mut next_id = 1;
    let root = content.get_mut("Plan").ok_or("Missing Plan node")?;
    let analyzed = root.get("Actual Rows").is_some();
    process_node(root, &mut next_id, &mut ctes, 0.0);
    let root = content.get("Plan").ok_or("Missing Plan node")?;
    let mut max_rows: f64 = 0.0;
    let mut max_cost: f64 = 0.0;
    let mut max_total_cost: f64 = 0.0;
    let mut max_duration: f64 = 0.0;
    let mut max_factor: f64 = 0.0;
    let mut visit = |node: &Value| {
        max_rows = max_rows.max(num(node, "*Actual Rows Revised").unwrap_or(0.0));
        max_cost = max_cost.max(num(node, "*Cost (exclusive)").unwrap_or(0.0));
        max_total_cost = max_total_cost.max(num(node, "Total Cost").unwrap_or(0.0));
        max_duration = max_duration.max(num(node, "*Duration (exclusive)").unwrap_or(0.0));
        max_factor = max_factor.max(num(node, "*Planner Row Estimate Factor").unwrap_or(0.0));
    };
    walk(root, &mut visit);
    for cte in &ctes {
        walk(cte, &mut visit);
    }
    let map = content.as_object_mut().ok_or("Invalid plan container")?;
    put(map, "maxRows", max_rows);
    put(map, "maxCost", max_cost);
    put(map, "maxTotalCost", max_total_cost);
    put(map, "maxDuration", max_duration);
    put(map, "maxEstimateFactor", (max_factor * 2.0).max(1.0));
    let created = chrono::Utc::now().to_rfc3339();
    let serial = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    serde_json::to_string(&json!({
        "id":format!("plan_{serial}"), "name":"PostgreSQL plan", "query": sql,
        "createdOn":created, "content":content, "ctes":ctes,
        "isAnalyze":analyzed, "isVerbose":false,
        "planStats":{"maxRows":max_rows,"maxCost":max_cost,"maxDuration":max_duration,"maxBlocks":{},"maxIo":0,"maxEstimateFactor":(max_factor * 2.0).max(1.0)}
    })).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_existing_pev2_text_fixture_without_js() {
        let input = include_str!("../../../src/services/__tests__/from-text/01-plan");
        let plan: Value =
            serde_json::from_str(&parse_plan(input, "SELECT * FROM tenk1").unwrap()).unwrap();
        let expected: Value = serde_json::from_str(include_str!(
            "../../../src/services/__tests__/from-text/01-expect"
        ))
        .unwrap();
        for key in [
            "Node Type",
            "Relation Name",
            "Startup Cost",
            "Total Cost",
            "Plan Rows",
            "Plan Width",
        ] {
            assert_eq!(plan["content"]["Plan"][key], expected["Plan"][key], "{key}");
        }
        assert_eq!(plan["content"]["Plan"]["nodeId"], 1);
        assert_eq!(plan["content"]["maxTotalCost"], 333);
    }
    fn compare_tree(before: &Value, after: &Value, path: &str, errors: &mut Vec<String>) {
        for key in [
            "Node Type",
            "Relation Name",
            "Index Name",
            "Plan Rows",
            "Total Cost",
            "Sort Method",
            "Sort Space Type",
            "Shared Read Blocks",
            "Shared Hit Blocks",
            "Filter",
            "Join Type",
            "Scan Direction",
            "Parent Relationship",
            "Subplan Name",
        ] {
            if !before[key].is_null() && before[key] != after[key] {
                errors.push(format!(
                    "{path} {key}: expected {} got {}",
                    before[key], after[key]
                ));
            }
        }
        let expected = before
            .get("Plans")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let actual = after
            .get("Plans")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if expected.len() != actual.len() {
            errors.push(format!(
                "{path} children: expected {} got {}",
                expected.len(),
                actual.len()
            ));
        }
        for (i, (left, right)) in expected.iter().zip(actual).enumerate() {
            compare_tree(left, right, &format!("{path}/{i}"), errors);
        }
    }

    #[test]
    fn pev2_text_fixture_structure_compatibility() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../src/services/__tests__/from-text");
        let mut checked = 0;
        let mut mismatches = Vec::new();
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(prefix) = name.strip_suffix("-plan") else {
                continue;
            };
            let expected_path = path.with_file_name(format!("{prefix}-expect"));
            if !expected_path.is_file() {
                continue;
            }
            let expected: Value =
                serde_json::from_str(&std::fs::read_to_string(expected_path).unwrap()).unwrap();
            let source = std::fs::read_to_string(&path).unwrap();
            match parse_plan(&source, "") {
                Ok(text) => {
                    let actual: Value = serde_json::from_str(&text).unwrap();
                    compare_tree(
                        &expected["Plan"],
                        &actual["content"]["Plan"],
                        name,
                        &mut mismatches,
                    );
                }
                Err(reason) => mismatches.push(format!("{name}: {reason}")),
            }
            checked += 1;
        }
        assert!(checked >= 35, "Expected the upstream text fixtures");
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn multiline_auto_explain_pair_and_disk_sort() {
        let raw = "Query Text: SELECT * FROM t\nLimit  (cost=2.00..4.00 rows=10 width=8) (actual time=0.010..7.000 rows=4 loops=1)\n  Buffers: shared hit=2 read=5\n  ->  Sort  (cost=2.00..3.00 rows=10 width=8) (actual time=0.003..6.000 rows=4 loops=1)\n        Sort Method: external merge  Disk: 20kB\n        ->  Seq Scan on t  (cost=0.00..2.00 rows=10 width=8) (actual time=0.001..3.000 rows=4 loops=1)\nPlanning Time: 0.25 ms\nExecution Time: 7.01 ms";
        let plan: Value =
            serde_json::from_str(&parse_plan(raw, "SELECT * FROM t").unwrap()).unwrap();
        assert_eq!(plan["content"]["Plan"]["Shared Read Blocks"], 5);
        assert_eq!(
            plan["content"]["Plan"]["Plans"][0]["Sort Space Type"],
            "Disk"
        );
        assert_eq!(
            plan["content"]["Plan"]["Plans"][0]["Plans"][0]["Relation Name"],
            "t"
        );
        assert_eq!(plan["content"]["Execution Time"], 7.01);
        assert_eq!(plan["content"]["Plan"]["*Duration (exclusive)"], 1);
    }
    #[test]
    fn parses_first_auto_explain_records_from_supplied_sample_when_present() {
        use std::io::{BufRead, BufReader};
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let path = std::path::PathBuf::from(home).join("pgsql_Wed.log");
        if !path.is_file() {
            return;
        }
        let reader = BufReader::new(std::fs::File::open(path).unwrap());
        let mut block = String::new();
        let mut captured = 0;
        for line in reader.lines() {
            let line = line.unwrap();
            if line.as_bytes().first().is_some_and(u8::is_ascii_digit) && line.contains("LOG:") {
                if !block.is_empty() {
                    let result = parse_plan(&block, "");
                    assert!(
                        result.is_ok(),
                        "Auto-explain record {captured}: {}",
                        result.unwrap_err()
                    );
                    captured += 1;
                    block.clear();
                    if captured == 10 {
                        break;
                    }
                }
                if line.contains(" plan:") {
                    block.push_str("\n");
                }
            } else if !block.is_empty() {
                block.push_str(&line);
                block.push('\n');
            }
        }
        assert_eq!(captured, 10, "Sample should contain auto_explain records");
    }

    #[test]
    fn parses_json_directly_with_persisted_metrics() {
        let raw = r#"[{"Plan":{"Node Type":"Aggregate","Total Cost":20,"Plan Rows":1,"Plans":[{"Node Type":"Seq Scan","Relation Name":"items","Total Cost":18,"Plan Rows":200}]}}]"#;
        let plan: Value =
            serde_json::from_str(&parse_plan(raw, "SELECT count(*) FROM items").unwrap()).unwrap();
        assert_eq!(plan["content"]["Plan"]["Plans"][0]["nodeId"], 2);
        assert_eq!(plan["content"]["maxTotalCost"], 20);
        assert_eq!(plan["content"]["Plan"]["*Cost (exclusive)"], 2);
        assert!(parse_plan("not a plan", "SELECT 1").is_err());
    }
}
