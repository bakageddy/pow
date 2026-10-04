use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Insight {
    pub severity: &'static str,
    pub title: &'static str,
    pub detail: String,
    pub node_id: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricChange {
    pub label: &'static str,
    pub before: Option<f64>,
    pub after: Option<f64>,
    pub change_percent: Option<f64>,
    pub unit: &'static str,
    // "neutral" means no claim that a different estimated count is better.
    pub direction: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeCountChange {
    pub node_type: String,
    pub before: usize,
    pub after: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanComparison {
    pub metrics: Vec<MetricChange>,
    pub node_changes: Vec<NodeCountChange>,
}

fn number(node: &Value, key: &str) -> Option<f64> {
    node.get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
}

fn walk(node: &Value, visit: &mut impl FnMut(&Value)) {
    if !node.is_object() {
        return;
    }
    visit(node);
    if let Some(children) = node.get("Plans").and_then(Value::as_array) {
        for child in children {
            walk(child, visit);
        }
    }
}

fn each_node(plan: &Value, mut visit: impl FnMut(&Value)) {
    if let Some(root) = plan.pointer("/content/Plan") {
        walk(root, &mut visit);
    }
    if let Some(ctes) = plan.get("ctes").and_then(Value::as_array) {
        for cte in ctes {
            walk(cte, &mut visit);
        }
    }
}

pub fn insights(plan: &Value) -> Vec<Insight> {
    let mut found = Vec::new();
    each_node(plan, |node| {
        if found.len() >= 8 {
            return;
        }
        let node_id = node.get("nodeId").and_then(Value::as_i64);
        let kind = node.get("Node Type").and_then(Value::as_str).unwrap_or("");
        let relation = node
            .get("Relation Name")
            .and_then(Value::as_str)
            .unwrap_or("this relation");
        let rows = number(node, "Plan Rows").unwrap_or(0.0);
        if kind == "Seq Scan" && rows >= 10_000.0 {
            found.push(Insight {
                severity: "info", title: "Large sequential scan",
                detail: format!("{relation} has roughly {rows:.0} estimated rows. Check selectivity and indexes if this scan dominates your workload."), node_id,
            });
        }
        if node.get("Sort Space Type").and_then(Value::as_str) == Some("Disk")
            || node
                .get("Sort Method")
                .and_then(Value::as_str)
                .is_some_and(|method| method.contains("external"))
        {
            found.push(Insight {
                severity: "warning", title: "Sort spilled to disk",
                detail: "This sort used disk rather than memory. Review sort volume and work_mem for this session.".into(), node_id,
            });
        }
        let estimate_factor = number(node, "*Planner Row Estimate Factor").unwrap_or(0.0);
        if estimate_factor >= 10.0 && number(node, "Actual Rows").unwrap_or(0.0) >= 100.0 {
            found.push(Insight {
                severity: "warning", title: "Row estimate mismatch",
                detail: format!("Estimated vs. actual rows differ by about {estimate_factor:.0}×. Consider checking table statistics and filter selectivity."), node_id,
            });
        }
        if found.len() > 8 {
            found.truncate(8);
        }
    });
    found
}

fn metric(
    label: &'static str,
    before: Option<f64>,
    after: Option<f64>,
    unit: &'static str,
    assess: bool,
) -> MetricChange {
    let change_percent = match (before, after) {
        (Some(before), Some(after)) if before > 0.0 => Some((after - before) / before * 100.0),
        _ => None,
    };
    let direction = if assess {
        match (before, after) {
            (Some(b), Some(a)) if a < b => "improved",
            (Some(b), Some(a)) if a > b => "regressed",
            (Some(_), Some(_)) => "unchanged",
            _ => "unavailable",
        }
    } else {
        "neutral"
    };
    MetricChange {
        label,
        before,
        after,
        change_percent,
        unit,
        direction,
    }
}

fn counts(plan: &Value) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    each_node(plan, |node| {
        if let Some(kind) = node.get("Node Type").and_then(Value::as_str) {
            *result.entry(kind.to_owned()).or_default() += 1;
        }
    });
    result
}

pub fn compare_plans(before: &Value, after: &Value) -> PlanComparison {
    let old = &before["content"];
    let new = &after["content"];
    let metrics = vec![
        metric(
            "Execution",
            number(old, "Execution Time"),
            number(new, "Execution Time"),
            "ms",
            true,
        ),
        metric(
            "Planning",
            number(old, "Planning Time"),
            number(new, "Planning Time"),
            "ms",
            true,
        ),
        metric(
            "Estimated cost",
            number(old, "maxTotalCost"),
            number(new, "maxTotalCost"),
            "",
            true,
        ),
        metric(
            "Estimated rows",
            number(&old["Plan"], "Plan Rows"),
            number(&new["Plan"], "Plan Rows"),
            "",
            false,
        ),
    ];
    let old_counts = counts(before);
    let new_counts = counts(after);
    let node_changes = old_counts
        .keys()
        .chain(new_counts.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|kind| {
            let before = old_counts.get(kind).copied().unwrap_or(0);
            let after = new_counts.get(kind).copied().unwrap_or(0);
            (before != after).then(|| NodeCountChange {
                node_type: kind.clone(),
                before,
                after,
            })
        })
        .collect();
    PlanComparison {
        metrics,
        node_changes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn warns_only_for_relevant_nodes() {
        let plan = json!({"content": {"Plan": {"nodeId": 1, "Node Type": "Aggregate", "Plans": [
            {"nodeId": 2, "Node Type": "Seq Scan", "Relation Name": "orders", "Plan Rows": 30000, "Actual Rows": 500, "*Planner Row Estimate Factor": 60},
            {"nodeId": 3, "Node Type": "Sort", "Sort Space Type": "Disk"}
        ]}}});
        let found = insights(&plan);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].title, "Large sequential scan");
        assert_eq!(found[1].node_id, Some(2));
        assert_eq!(found[2].title, "Sort spilled to disk");
        assert!(insights(&json!({"content":{"Plan":{"Node Type":"Index Scan"}}})).is_empty());
    }

    #[test]
    fn compares_actuals_without_claiming_row_count_improvements() {
        let old = json!({"content":{"Execution Time": 100.0,"maxTotalCost": 100.0,"Plan":{"Node Type":"Seq Scan","Plan Rows":100}}});
        let new = json!({"content":{"Execution Time": 40.0,"maxTotalCost": 80.0,"Plan":{"Node Type":"Index Scan","Plan Rows":12}}});
        let result = compare_plans(&old, &new);
        assert_eq!(result.metrics[0].change_percent, Some(-60.0));
        assert_eq!(result.metrics[0].direction, "improved");
        assert_eq!(result.metrics[3].direction, "neutral");
        assert_eq!(result.node_changes.len(), 2);
        let estimated = json!({"content":{"maxTotalCost": 12.0,"Plan":{"Node Type":"Result"}}});
        assert_eq!(
            compare_plans(&old, &estimated).metrics[0].direction,
            "unavailable"
        );
    }
}
