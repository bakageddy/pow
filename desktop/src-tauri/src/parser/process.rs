use serde_json::{json, Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use super::cursor::{f64_to_json, get_f64, insert_finite};
use super::text::parse_text;

// Reads a finite f64 from a map entry — mirrors get_f64 but takes &Map directly.
fn map_f64(map: &Map<String, Value>, key: &str) -> Option<f64> {
    map.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

// Translates JSON-mode node types that differ from their text-mode equivalents.
fn normalize_json_node(map: &mut Map<String, Value>) {
    if let Some(strategy) = map.get("Strategy").and_then(Value::as_str).map(str::to_owned) {
        if map.get("Node Type").and_then(Value::as_str) == Some("Aggregate") {
            let prefix = match strategy.as_str() {
                "Sorted" => "Group",
                "Hashed" => "Hash",
                _ => "",
            };
            map.insert("Node Type".into(), Value::from(format!("{prefix}Aggregate")));
        }
    }
    if map.get("Node Type").and_then(Value::as_str) == Some("ModifyTable") {
        if let Some(op) = map.get("Operation").and_then(Value::as_str).map(str::to_owned) {
            map.insert("Node Type".into(), Value::from(op));
        }
    }
}

// Records which direction the planner's row estimate was off and by how much.
fn assign_row_estimate_stats(map: &mut Map<String, Value>) {
    let Some(actual) = map_f64(map, "Actual Rows") else { return };
    let Some(estimate) = map_f64(map, "Plan Rows") else { return };
    let (direction, factor) = if actual > estimate {
        (2, actual / estimate)
    } else if actual < estimate {
        (1, estimate / actual)
    } else {
        (3, 1.0)
    };
    map.insert("*Planner Row Estimate Direction".into(), Value::from(direction));
    if factor.is_finite() {
        map.insert("*Planner Row Estimate Factor".into(), f64_to_json(factor));
    }
}

// Scales raw timing values by loops/workers and derives exclusive duration.
fn scale_timing(map: &mut Map<String, Value>, children: &[Value], inherited_workers: f64) {
    let Some(total) = map_f64(map, "Actual Total Time") else { return };
    let loops = map_f64(map, "Actual Loops").unwrap_or(1.0);
    let divisor = inherited_workers + 1.0;
    let revised = total * loops / divisor;
    insert_finite(map, "Actual Total Time", revised);
    if let Some(start) = map_f64(map, "Actual Startup Time") {
        insert_finite(map, "Actual Startup Time", start * loops / divisor);
    }
    let is_result = map.get("Node Type").and_then(Value::as_str) == Some("Result");
    let child_time: f64 = children
        .iter()
        .filter(|child| {
            child.get("Parent Relationship").and_then(Value::as_str) != Some("InitPlan")
                || is_result
        })
        .map(|child| get_f64(child, "Actual Total Time").unwrap_or(0.0))
        .sum();
    insert_finite(map, "*Duration (exclusive)", (revised - child_time).max(0.0));
}

// Subtracts children's total cost to find what this node alone costs.
fn compute_exclusive_cost(map: &mut Map<String, Value>, children: &[Value]) {
    let Some(total) = map_f64(map, "Total Cost") else { return };
    let child_cost: f64 = children
        .iter()
        .map(|child| get_f64(child, "Total Cost").unwrap_or(0.0))
        .sum();
    insert_finite(map, "*Cost (exclusive)", (total - child_cost).max(0.0));
}

// Multiplies per-loop row counts by the actual loop count so callers see totals.
fn scale_row_counts(map: &mut Map<String, Value>) {
    let loops = map_f64(map, "Actual Loops").filter(|l| *l > 0.0).unwrap_or(1.0);
    for (source, target) in [
        ("Actual Rows",                   "*Actual Rows Revised"),
        ("Plan Rows",                     "*Plan Rows Revised"),
        ("Rows Removed by Filter",        "*Rows Removed by Filter"),
        ("Rows Removed by Join Filter",   "*Rows Removed by Join Filter"),
        ("Rows Removed by Index Recheck", "*Rows Removed by Index Recheck"),
    ] {
        if let Some(n) = map_f64(map, source) {
            insert_finite(map, target, n * loops);
        }
    }
}

// Subtracts non-subplan children's block counts to produce per-node exclusive values.
fn compute_exclusive_block_stats(map: &mut Map<String, Value>, children: &[Value]) {
    for (source, target) in [
        ("Shared Hit Blocks",     "*Shared Hit Blocks (exclusive)"),
        ("Shared Read Blocks",    "*Shared Read Blocks (exclusive)"),
        ("Shared Dirtied Blocks", "*Shared Dirtied Blocks (exclusive)"),
        ("Shared Written Blocks", "*Shared Written Blocks (exclusive)"),
        ("Temp Read Blocks",      "*Temp Read Blocks (exclusive)"),
        ("Temp Written Blocks",   "*Temp Written Blocks (exclusive)"),
        ("Local Hit Blocks",      "*Local Hit Blocks (exclusive)"),
        ("Local Read Blocks",     "*Local Read Blocks (exclusive)"),
        ("Local Dirtied Blocks",  "*Local Dirtied Blocks (exclusive)"),
        ("Local Written Blocks",  "*Local Written Blocks (exclusive)"),
    ] {
        let Some(inclusive) = map_f64(map, source) else { continue };
        let descendants: f64 = children
            .iter()
            .filter(|c| c.get("Subplan Name").is_none())
            .map(|c| get_f64(c, source).unwrap_or(0.0))
            .sum();
        insert_finite(map, target, (inclusive - descendants).max(0.0));
    }
}

fn process_node(
    node: &mut Value,
    next_id: &mut u64,
    ctes: &mut Vec<Value>,
    inherited_workers: f64,
) {
    let Some(map) = node.as_object_mut() else { return };

    map.insert("nodeId".into(), Value::from(*next_id));
    *next_id += 1;
    normalize_json_node(map);
    assign_row_estimate_stats(map);

    let planned = map_f64(map, "Workers Launched").unwrap_or(inherited_workers);

    // Recursively process children, separating CTE init-plans into the side-channel.
    let mut kept = Vec::new();
    if let Some(Value::Array(raw_children)) = map.remove("Plans") {
        for mut child in raw_children {
            process_node(&mut child, next_id, ctes, planned);
            let is_cte_init = child.get("Parent Relationship").and_then(Value::as_str)
                == Some("InitPlan")
                && child
                    .get("Subplan Name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.starts_with("CTE"));
            if is_cte_init {
                ctes.push(child);
            } else {
                kept.push(child);
            }
        }
    }
    if !kept.is_empty() {
        map.insert("Plans".into(), Value::Array(kept));
    }

    // Clone children for the metric sub-functions (they only read).
    let children: Vec<Value> = map
        .get("Plans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    scale_timing(map, &children, inherited_workers);
    compute_exclusive_cost(map, &children);
    scale_row_counts(map);
    compute_exclusive_block_stats(map, &children);
}

fn walk(root: &Value, visit: &mut impl FnMut(&Value)) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        visit(node);
        if let Some(children) = node.get("Plans").and_then(Value::as_array) {
            // Push in reverse so the leftmost child is processed first.
            stack.extend(children.iter().rev());
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
        max_rows = max_rows.max(get_f64(node, "*Actual Rows Revised").unwrap_or(0.0));
        max_cost = max_cost.max(get_f64(node, "*Cost (exclusive)").unwrap_or(0.0));
        max_total_cost = max_total_cost.max(get_f64(node, "Total Cost").unwrap_or(0.0));
        max_duration = max_duration.max(get_f64(node, "*Duration (exclusive)").unwrap_or(0.0));
        max_factor = max_factor.max(get_f64(node, "*Planner Row Estimate Factor").unwrap_or(0.0));
    };
    walk(root, &mut visit);
    for cte in &ctes {
        walk(cte, &mut visit);
    }
    let map = content.as_object_mut().ok_or("Invalid plan container")?;
    insert_finite(map, "maxRows", max_rows);
    insert_finite(map, "maxCost", max_cost);
    insert_finite(map, "maxTotalCost", max_total_cost);
    insert_finite(map, "maxDuration", max_duration);
    insert_finite(map, "maxEstimateFactor", (max_factor * 2.0).max(1.0));
    let created = chrono::Utc::now().to_rfc3339();
    let serial = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    serde_json::to_string(&json!({
        "id": format!("plan_{serial}"), "name": "PostgreSQL plan", "query": sql,
        "createdOn": created, "content": content, "ctes": ctes,
        "isAnalyze": analyzed, "isVerbose": false,
        "planStats": {
            "maxRows": max_rows, "maxCost": max_cost, "maxDuration": max_duration,
            "maxBlocks": {}, "maxIo": 0,
            "maxEstimateFactor": (max_factor * 2.0).max(1.0)
        }
    }))
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_existing_pev2_text_fixture_without_js() {
        let input = include_str!("../../../../src/services/__tests__/from-text/01-plan");
        let plan: Value =
            serde_json::from_str(&parse_plan(input, "SELECT * FROM tenk1").unwrap()).unwrap();
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../src/services/__tests__/from-text/01-expect"
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
                    block.push('\n');
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
