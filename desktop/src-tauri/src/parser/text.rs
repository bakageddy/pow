use serde_json::{Map, Value};

use super::cursor::{insert_finite, parse_leading_number, Cursor};
use super::normalize::{label_value, parse_detail};

#[derive(Default)]
pub struct RawNode {
    pub fields: Map<String, Value>,
    pub children: Vec<Self>,
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

fn node_prefix(line: &str) -> Option<(&str, &str)> {
    let text = line
        .trim_start()
        .strip_prefix("->")
        .unwrap_or_else(|| line.trim_start())
        .trim_start();
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
                Value::from(if scan.is_some() { "Backward" } else { "Forward" }),
            );
            if !relation.is_empty() {
                set_relation(relation, fields);
            }
            name = scan.unwrap_or_else(|| scan_name(name));
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
    name.split_once(" using ").map_or(name, |(scan, _)| scan)
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

    let has_cost = if cursor.eat(b"(cost=") {
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
        insert_finite(&mut node.fields, "Startup Cost", startup);
        insert_finite(&mut node.fields, "Total Cost", total);
        insert_finite(&mut node.fields, "Plan Rows", rows);
        insert_finite(&mut node.fields, "Plan Width", width);
        true
    } else {
        false
    };

    cursor.skip_space();
    let has_actual = if cursor.eat(b"(actual ") {
        if cursor.eat(b"time=") {
            let startup = cursor.number()?;
            if !cursor.eat(b"..") {
                return None;
            }
            let total = cursor.number()?;
            insert_finite(&mut node.fields, "Actual Startup Time", startup);
            insert_finite(&mut node.fields, "Actual Total Time", total);
        }
        let rows = cursor.field(b"rows=")?;
        let loops = cursor.field(b"loops=")?;
        cursor.skip_space();
        if !cursor.eat(b")") {
            return None;
        }
        insert_finite(&mut node.fields, "Actual Rows", rows);
        insert_finite(&mut node.fields, "Actual Loops", loops);
        if rows.fract() != 0.0 {
            node.fields.insert("*Actual Rows Is Fractional".into(), Value::Bool(true));
        }
        true
    } else if cursor.eat(b"(never executed)") {
        insert_finite(&mut node.fields, "Actual Rows", 0.0);
        insert_finite(&mut node.fields, "Actual Loops", 0.0);
        true
    } else {
        false
    };

    if !(has_cost || has_actual) {
        return None;
    }
    normalize_type(name, &mut node.fields);
    Some(node)
}

fn at_path<'a>(mut node: &'a mut RawNode, path: &[usize]) -> &'a mut RawNode {
    for &idx in path {
        node = &mut node.children[idx];
    }
    node
}

fn count_indent(line: &str) -> usize {
    line.bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .map(|b| if b == b'\t' { 4 } else { 1 })
        .sum()
}

fn is_noise(trimmed: &str) -> bool {
    trimmed.is_empty()
        || trimmed == "QUERY PLAN"
        || trimmed.starts_with("----")
        || trimmed.starts_with("(1 row")
}

fn try_query_text(trimmed: &str, global: &mut Map<String, Value>, phase: &mut Phase) -> bool {
    let Some(query) = label_value(trimmed, "Query Text") else { return false };
    global.insert("Query Text".into(), Value::from(query));
    *phase = Phase::QueryText;
    true
}

// Tries to parse a plan node from the current line and attach it to the tree.
// Returns Ok(true) when the line was consumed, Err when the plan is structurally invalid.
fn try_node_line(
    line: &str,
    indent: usize,
    root: &mut Option<RawNode>,
    stack: &mut Vec<Frame>,
    subplan: &mut Option<Subplan>,
    phase: &mut Phase,
) -> Result<bool, String> {
    let Some((name, trailer)) = node_prefix(line) else { return Ok(false) };
    let Some(mut next) = parse_node(name, trailer) else { return Ok(false) };

    if let Some(marker) = subplan.as_mut() {
        if indent <= marker.indent {
            *subplan = None;
        } else if marker.child_indent.is_none() || marker.child_indent == Some(indent) {
            marker.child_indent = Some(indent);
            next.fields
                .insert("Parent Relationship".into(), Value::from(marker.relationship));
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
        stack.push(Frame { indent, path: own_path });
    } else if root.is_none() {
        *root = Some(next);
        stack.push(Frame { indent, path: vec![] });
    } else {
        return Err("Multiple root nodes in text plan".into());
    }
    *phase = Phase::Plan;
    Ok(true)
}

// Continues accumulating a multi-line Query Text value before the plan begins.
fn try_query_text_continuation(
    trimmed: &str,
    phase: Phase,
    no_root: bool,
    global: &mut Map<String, Value>,
) -> bool {
    if phase != Phase::QueryText || !no_root {
        return false;
    }
    if let Some(text) = global.get("Query Text").and_then(Value::as_str).map(str::to_owned) {
        global.insert("Query Text".into(), Value::from(format!("{text}\n{trimmed}")));
    }
    true
}

// Detects CTE / InitPlan / SubPlan section headers and opens a subplan context.
fn try_subplan_header(
    trimmed: &str,
    indent: usize,
    stack: &mut Vec<Frame>,
    subplan: &mut Option<Subplan>,
) -> bool {
    if let Some(name) = trimmed.strip_prefix("CTE ") {
        while stack.last().is_some_and(|f| f.indent >= indent) {
            stack.pop();
        }
        *subplan = Some(Subplan {
            indent,
            relationship: "InitPlan",
            name: format!("CTE {name}"),
            child_indent: None,
        });
        return true;
    }
    if trimmed == "InitPlan"
        || trimmed.starts_with("InitPlan ")
        || trimmed == "SubPlan"
        || trimmed.starts_with("SubPlan ")
    {
        while stack.last().is_some_and(|f| f.indent >= indent) {
            stack.pop();
        }
        *subplan = Some(Subplan {
            indent,
            relationship: if trimmed.starts_with("Init") { "InitPlan" } else { "SubPlan" },
            name: trimmed.to_owned(),
            child_indent: None,
        });
        return true;
    }
    false
}

fn try_trigger(trimmed: &str, global: &mut Map<String, Value>) -> bool {
    if !trimmed.starts_with("Trigger ") {
        return false;
    }
    let entry = trimmed.strip_prefix("Trigger ").unwrap_or(trimmed);
    if let Some((name, rest)) = entry.split_once(':') {
        let time = rest
            .split_whitespace()
            .find_map(|t| t.strip_prefix("time="))
            .and_then(parse_leading_number)
            .unwrap_or(0.0);
        let calls = rest
            .split_whitespace()
            .find_map(|t| t.strip_prefix("calls="))
            .and_then(parse_leading_number)
            .unwrap_or(0.0);
        global
            .entry("Triggers")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("array")
            .push(serde_json::json!({"Trigger Name": name, "Time": time, "Calls": calls.to_string()}));
    }
    true
}

// Handles plan-level summary lines and the Planning: section header.
fn try_global_detail(
    trimmed: &str,
    global: &mut Map<String, Value>,
    phase: &mut Phase,
) -> bool {
    if ["Execution Time:", "Planning Time:", "Total runtime:", "Settings:"]
        .iter()
        .any(|s| trimmed.starts_with(s))
    {
        parse_detail(trimmed, global);
        return true;
    }
    if trimmed == "Planning:" {
        *phase = Phase::Planning;
        global.insert("Planning".into(), Value::Object(Map::new()));
        return true;
    }
    if *phase == Phase::Planning {
        if let Some(planning) = global.get_mut("Planning").and_then(Value::as_object_mut) {
            parse_detail(trimmed, planning);
        }
        return true;
    }
    false
}

// Falls back to attaching the line as a detail field on the current node.
fn apply_node_detail(trimmed: &str, root: &mut Option<RawNode>, stack: &[Frame]) {
    if let Some(frame) = stack.last() {
        let fields = &mut at_path(root.as_mut().expect("root"), &frame.path).fields;
        parse_detail(trimmed, fields);
    }
}

pub fn parse_text(source: &str) -> Result<Value, String> {
    let mut phase = Phase::BeforePlan;
    let mut root = None::<RawNode>;
    let mut global = Map::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut subplan: Option<Subplan> = None;

    for original in source.lines() {
        let line = original.trim_end_matches('\r').trim_matches(['\'', '"']);
        let indent = count_indent(line);
        let trimmed = line.trim();

        if is_noise(trimmed) { continue; }
        if try_query_text(trimmed, &mut global, &mut phase) { continue; }
        if try_node_line(line, indent, &mut root, &mut stack, &mut subplan, &mut phase)? { continue; }
        if try_query_text_continuation(trimmed, phase, root.is_none(), &mut global) { continue; }
        if try_subplan_header(trimmed, indent, &mut stack, &mut subplan) { continue; }
        if try_trigger(trimmed, &mut global) { continue; }
        if try_global_detail(trimmed, &mut global, &mut phase) { continue; }
        apply_node_detail(trimmed, &mut root, &stack);
    }

    let root = root.ok_or("Unable to parse plan")?;
    global.insert("Plan".into(), raw_value(root));
    Ok(Value::Object(global))
}

pub fn raw_value(node: RawNode) -> Value {
    let mut fields = node.fields;
    if !node.children.is_empty() {
        fields.insert(
            "Plans".into(),
            Value::Array(node.children.into_iter().map(raw_value).collect()),
        );
    }
    Value::Object(fields)
}
