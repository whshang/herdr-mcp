//! Minimum readable pane width for agent panes (#684).
//!
//! TUI agents hard-wrap their own layout, so a degenerate pane records the
//! wrapping as real line breaks. Herdr's `recent_unwrapped` read source can only
//! rejoin terminal soft-wraps, so a two-to-four column pane yields excerpts such
//! as `cb` / `60` / `4c` / `6f` instead of `cb604c6f` no matter which read source
//! the excerpt uses. The only durable fix is to stop herdr-mcp from creating
//! agent panes narrower than a readable width, and to refuse publishing an
//! excerpt that can only be wrapping noise.

use serde_json::{Value, json};

/// Below this many columns a TUI agent wraps its own output hard enough that a
/// captured excerpt is unreadable, so herdr-mcp neither creates such a pane
/// through a horizontal split nor publishes an excerpt taken from one.
pub(crate) const MIN_READABLE_PANE_COLS: u16 = 40;

/// Socket method that narrows a pane horizontally.
pub(crate) const SPLIT_METHOD: &str = "pane.split";

/// Outcome of checking a `pane.split` request against the readable-width floor.
pub(crate) enum SplitPlan {
    /// Forward to Herdr; the ratio may have been clamped to the readable window.
    Forward(Value),
    /// A horizontal split cannot leave a readable child in this tab.
    Refuse(Value),
}

/// Returns the readable-width decision for a `pane.split` request.
///
/// Only `direction: "right"` narrows a pane, and only when the parent pane's
/// width is known from the live snapshot. Everything else is forwarded exactly
/// as the caller sent it, so a missing layout entry never blocks a split.
pub(crate) fn guard_split(snapshot: &Value, params: &Value) -> SplitPlan {
    if params.get("direction").and_then(Value::as_str) != Some("right") {
        return SplitPlan::Forward(params.clone());
    }
    let Some(parent) = split_parent_pane(snapshot, params) else {
        return SplitPlan::Forward(params.clone());
    };
    let Some(cols) = pane_columns(snapshot, &parent) else {
        return SplitPlan::Forward(params.clone());
    };
    let floor = f64::from(MIN_READABLE_PANE_COLS);
    let cols_f = f64::from(cols);
    let lowest_readable_ratio = floor / cols_f;
    let highest_readable_ratio = 1.0 - floor / cols_f;
    if highest_readable_ratio < lowest_readable_ratio {
        return SplitPlan::Refuse(too_narrow(&parent, cols));
    }
    let Some(requested) = params.get("ratio").and_then(Value::as_f64) else {
        // Herdr's default split is even, and an even split of a pane that is at
        // least twice the floor leaves both sides readable.
        return SplitPlan::Forward(params.clone());
    };
    let clamped = requested.clamp(lowest_readable_ratio, highest_readable_ratio);
    if (clamped - requested).abs() < f64::EPSILON {
        return SplitPlan::Forward(params.clone());
    }
    let mut adjusted = params.clone();
    if let Some(object) = adjusted.as_object_mut() {
        object.insert("ratio".to_owned(), json!(round_ratio(clamped)));
    }
    SplitPlan::Forward(adjusted)
}

/// Rounds a clamped ratio so the forwarded request stays byte-stable.
fn round_ratio(ratio: f64) -> f64 {
    (ratio * 10_000.0).round() / 10_000.0
}

fn too_narrow(parent: &str, cols: u16) -> Value {
    json!({
        "ok": false,
        "code": "pane_split_too_narrow",
        "message": format!(
            "pane {parent} is {cols} columns wide; a horizontal split cannot leave a \
             readable child pane (minimum {MIN_READABLE_PANE_COLS} columns per side). \
             A narrower agent pane makes final_excerpt return hard-wrapped viewport \
             fragments instead of the agent's logical output."
        ),
        "parent_pane_id": parent,
        "parent_columns": cols,
        "minimum_readable_columns": MIN_READABLE_PANE_COLS,
        "requested_direction": "right",
        "next": {
            "action": "create_a_wide_enough_agent_pane",
            "guidance": [
                "open the agent in a new tab so the pane gets the tab's full width",
                "or reuse a verified task-owned free pane that is at least the minimum readable width",
                "or split down, which preserves the pane's current width"
            ],
            "do_not": "do not close or resize other panes in the tab to make room",
        },
    })
}

/// Resolves the pane a split targets: an explicit target, the focused pane of a
/// named workspace, or the focused pane of the session.
fn split_parent_pane(snapshot: &Value, params: &Value) -> Option<String> {
    if let Some(target) = params.get("target_pane_id").and_then(Value::as_str) {
        return Some(target.to_owned());
    }
    let workspace = params.get("workspace_id").and_then(Value::as_str);
    let layouts = snapshot.get("layouts").and_then(Value::as_array)?;
    layouts
        .iter()
        .filter(|layout| match workspace {
            Some(workspace) => {
                layout.get("workspace_id").and_then(Value::as_str) == Some(workspace)
            }
            None => true,
        })
        .find_map(|layout| {
            layout
                .get("focused_pane_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

/// Column width of a pane from the live snapshot layout tree.
pub(crate) fn pane_columns(snapshot: &Value, pane_id: &str) -> Option<u16> {
    let layouts = snapshot.get("layouts").and_then(Value::as_array)?;
    for layout in layouts {
        if layout.get("zoomed").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(panes) = layout.get("panes").and_then(Value::as_array) else {
            continue;
        };
        for pane in panes {
            if pane.get("pane_id").and_then(Value::as_str) != Some(pane_id) {
                continue;
            }
            let width = pane
                .get("rect")
                .and_then(|rect| rect.get("width"))
                .and_then(Value::as_u64)?;
            return u16::try_from(width).ok();
        }
    }
    None
}

/// Column width of a pane from a `pane.layout` response, falling back to the
/// session snapshot shape when the response embeds the whole snapshot.
pub(crate) fn pane_columns_from_layout_response(response: &Value, pane_id: &str) -> Option<u16> {
    let layout = response.get("layout").unwrap_or(response);
    let snapshot = json!({"layouts": [layout]});
    if let Some(cols) = pane_columns(&snapshot, pane_id) {
        return Some(cols);
    }
    pane_columns(response, pane_id)
}

/// True when a captured excerpt from this pane can carry logical output instead
/// of hard-wrapped viewport fragments.
pub(crate) fn excerpt_is_readable(cols: u16) -> bool {
    cols >= MIN_READABLE_PANE_COLS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_snapshot(workspace: &str, panes: &[(&str, u16)]) -> Value {
        let panes: Vec<Value> = panes
            .iter()
            .map(|(pane_id, width)| {
                json!({
                    "pane_id": pane_id,
                    "focused": false,
                    "rect": {"x": 0, "y": 0, "width": width, "height": 30},
                })
            })
            .collect();
        json!({
            "layouts": [{
                "workspace_id": workspace,
                "tab_id": format!("{workspace}:t1"),
                "focused_pane_id": panes[0]["pane_id"],
                "zoomed": false,
                "area": {"x": 0, "y": 0, "width": 200, "height": 30},
                "panes": panes,
                "splits": [],
            }]
        })
    }

    /// user parent | Given a crowded tab whose panes are narrower than two
    /// readable agent panes | When a horizontal split is requested | Then the
    /// split is refused with actionable guidance instead of creating a pane
    /// whose final_excerpt would be wrapping noise.
    #[test]
    fn horizontal_split_is_refused_when_no_readable_child_fits() {
        let snapshot = layout_snapshot("w1", &[("w1:p1", 20)]);
        let plan = guard_split(
            &snapshot,
            &json!({"direction": "right", "target_pane_id": "w1:p1"}),
        );
        let SplitPlan::Refuse(result) = plan else {
            panic!("a 20-column parent cannot host a readable horizontal split");
        };
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "pane_split_too_narrow");
        assert_eq!(result["parent_pane_id"], "w1:p1");
        assert_eq!(result["parent_columns"], 20);
        assert_eq!(result["minimum_readable_columns"], MIN_READABLE_PANE_COLS);
        assert!(
            result["next"]["guidance"]
                .as_array()
                .is_some_and(|guidance| guidance
                    .iter()
                    .any(|line| line.as_str().is_some_and(|line| line.contains("new tab")))),
            "refusal must name the fallback the caller should use"
        );
    }

    /// user parent | Given a parent pane wide enough for two readable children |
    /// When a horizontal split asks for a degenerate ratio | Then the forwarded
    /// request keeps both sides at or above the readable width.
    #[test]
    fn horizontal_split_ratio_is_clamped_to_the_readable_window() {
        let snapshot = layout_snapshot("w1", &[("w1:p1", 100)]);
        let plan = guard_split(
            &snapshot,
            &json!({"direction": "right", "target_pane_id": "w1:p1", "ratio": 0.97}),
        );
        let SplitPlan::Forward(params) = plan else {
            panic!("a 100-column parent must allow a horizontal split");
        };
        let ratio = params["ratio"].as_f64().expect("ratio stays a number");
        assert!(
            (ratio - 0.6).abs() < 0.0001,
            "ratio {ratio} must leave 40 columns for the new pane"
        );
        assert_eq!(params["target_pane_id"], "w1:p1");
        assert_eq!(params["direction"], "right");
    }

    /// user parent | Given a caller ratio that already leaves both sides
    /// readable | When the split is checked | Then the request is forwarded
    /// unchanged so the runtime never rewrites a valid layout request.
    #[test]
    fn readable_split_ratio_is_forwarded_unchanged() {
        let snapshot = layout_snapshot("w1", &[("w1:p1", 120)]);
        let requested = json!({"direction": "right", "target_pane_id": "w1:p1", "ratio": 0.5_f64});
        let SplitPlan::Forward(params) = guard_split(&snapshot, &requested) else {
            panic!("an even split of a 120-column pane is readable");
        };
        assert_eq!(params, requested);
    }

    /// user parent | Given a vertical split or an unknown pane | When the split
    /// is checked | Then it is forwarded unchanged because neither narrows width.
    #[test]
    fn non_narrowing_and_unknown_splits_are_forwarded_unchanged() {
        let snapshot = layout_snapshot("w1", &[("w1:p1", 20)]);
        let vertical = json!({"direction": "down", "target_pane_id": "w1:p1"});
        let SplitPlan::Forward(forwarded) = guard_split(&snapshot, &vertical) else {
            panic!("a down split preserves pane width");
        };
        assert_eq!(forwarded, vertical);

        let unknown = json!({"direction": "right", "target_pane_id": "w9:p9"});
        let SplitPlan::Forward(forwarded) = guard_split(&snapshot, &unknown) else {
            panic!("an unknown pane must not block the split");
        };
        assert_eq!(forwarded, unknown);
    }

    #[test]
    fn excerpt_is_readable_only_at_or_above_the_floor() {
        assert!(!excerpt_is_readable(MIN_READABLE_PANE_COLS - 1));
        assert!(excerpt_is_readable(MIN_READABLE_PANE_COLS));
        assert!(excerpt_is_readable(200));
    }
}
