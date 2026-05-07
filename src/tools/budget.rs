//! Output-size budgeting for MCP tool responses.
//!
//! Some pollard tools (`call_tree`, `folded_stacks`, `top_*`,
//! `compare_profiles`, `stacks_containing`) can produce results large
//! enough to overflow the caller harness's MCP token cap. The harness
//! then drops the payload and the LLM gets nothing useful — see #101.
//!
//! Rather than re-running the whole query with progressively tightened
//! pruning knobs (the original proposal), we build the result once and
//! then trim it down to fit. Each tool plugs in its own
//! "drop the lowest-priority element" strategy via [`fit_to_budget`];
//! the helper repeatedly serializes the (mutated) output and asks for
//! another drop until the JSON length is within the budget.
//!
//! That trades a quadratic-ish trim loop for the one-pass guarantee
//! that the response is shaped by the original args, not by retried
//! pruning knobs the caller never asked for. In practice the trim
//! loop converges in a few hundred steps even on large profiles
//! because the per-row size (~80–300 bytes) is small compared to the
//! ~25 KB budget.
//!
//! Configurable via `POLLARD_MAX_OUTPUT_BYTES`; the default
//! ([`DEFAULT_BUDGET_BYTES`]) is well under typical MCP caps.

use schemars::JsonSchema;
use serde::Serialize;

/// Default byte budget for a single MCP tool response. Sized to leave
/// headroom under the ~32 KB MCP tool-output limit observed in
/// practice (the truncations in #101 hit at 71k and 139k chars), with
/// some margin for the LLM's own framing.
pub const DEFAULT_BUDGET_BYTES: usize = 25_000;

/// Resolve the active output byte budget. Reads
/// `POLLARD_MAX_OUTPUT_BYTES` if set and parsable; otherwise returns
/// [`DEFAULT_BUDGET_BYTES`]. Set the env var to `0` to disable
/// trimming entirely (useful for tests / debugging).
pub fn output_budget_bytes() -> usize {
    std::env::var("POLLARD_MAX_OUTPUT_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_BUDGET_BYTES)
}

/// Trimming summary attached to any response we had to shrink. The
/// `dropped_pct` field is opportunistic — set when the per-tool drop
/// strategy can attribute a percentage to the dropped element (e.g.
/// `self_pct` on a row, `total_pct` on a tree node), `None` otherwise.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Truncated {
    /// Number of items that were removed from the response to fit the
    /// byte budget.
    pub dropped: usize,
    /// Sum of priorities (typically `pct`) of dropped items, when the
    /// per-tool strategy supplies them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_pct: Option<f32>,
    /// Active byte budget at the time of trimming. Echoed so the
    /// caller can see what cap was applied.
    pub budget_bytes: usize,
    /// Final serialized size of the (trimmed) response, in bytes.
    pub final_bytes: usize,
    /// Whether the trim loop bottomed out before fitting (i.e. the
    /// per-tool strategy reported it could not shrink further). When
    /// `true` the response may still exceed `budget_bytes`; the caller
    /// should re-query with tighter knobs.
    pub still_over_budget: bool,
}

/// Result of a single drop call from a per-tool strategy.
pub enum DropOutcome {
    /// One item was removed. The optional `f32` is its priority weight
    /// (e.g. `self_pct`) for the rolled-up `dropped_pct` summary.
    Dropped(Option<f32>),
    /// Nothing left to drop without producing an empty/meaningless
    /// response. The trim loop stops and `still_over_budget` is set.
    Exhausted,
}

/// Serialize `output`, and while it exceeds `budget`, ask `drop_one`
/// to remove the lowest-priority element. Returns `None` if the first
/// serialization already fit, otherwise a [`Truncated`] summary.
///
/// `drop_one` mutates `output` in place. It must be cheap relative to
/// the per-iteration re-serialization (which dominates the loop) and
/// should avoid recomputing aggregates — the whole point of the
/// budget-down approach is to shape the response without re-running
/// the underlying query.
pub fn fit_to_budget<T, F>(output: &mut T, budget: usize, mut drop_one: F) -> Option<Truncated>
where
    T: Serialize,
    F: FnMut(&mut T) -> DropOutcome,
{
    if budget == 0 {
        return None;
    }
    let mut size = serialized_size(output);
    if size <= budget {
        return None;
    }
    let mut dropped: usize = 0;
    let mut dropped_pct: f32 = 0.0;
    let mut had_pct = false;
    let mut exhausted = false;
    while size > budget {
        match drop_one(output) {
            DropOutcome::Dropped(pct) => {
                dropped += 1;
                if let Some(p) = pct {
                    dropped_pct += p;
                    had_pct = true;
                }
            }
            DropOutcome::Exhausted => {
                exhausted = true;
                break;
            }
        }
        size = serialized_size(output);
    }
    if dropped == 0 && !exhausted {
        return None;
    }
    Some(Truncated {
        dropped,
        dropped_pct: had_pct.then_some(dropped_pct),
        budget_bytes: budget,
        final_bytes: size,
        still_over_budget: exhausted,
    })
}

fn serialized_size<T: Serialize>(output: &T) -> usize {
    // Errors here would mean the response is unserializable, which the
    // outer rmcp layer would also reject — fall through with 0 so the
    // trim loop terminates rather than spinning.
    serde_json::to_vec(output).map(|v| v.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Bag {
        items: Vec<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        truncated: Option<Truncated>,
    }

    #[test]
    fn no_trim_when_already_under_budget() {
        let mut bag = Bag {
            items: vec![1, 2, 3],
            truncated: None,
        };
        let out = fit_to_budget(&mut bag, 10_000, |b| {
            b.items.pop();
            DropOutcome::Dropped(None)
        });
        assert!(out.is_none());
        assert_eq!(bag.items.len(), 3);
    }

    #[test]
    fn trims_until_fits() {
        let items: Vec<u32> = (0..500).collect();
        let mut bag = Bag {
            items,
            truncated: None,
        };
        let out = fit_to_budget(&mut bag, 200, |b| {
            if b.items.is_empty() {
                DropOutcome::Exhausted
            } else {
                b.items.pop();
                DropOutcome::Dropped(Some(1.0))
            }
        })
        .expect("expected truncation");
        assert!(out.dropped > 0);
        assert!(out.final_bytes <= 200);
        assert!(!out.still_over_budget);
        let dropped_pct = out.dropped_pct.expect("pct rolled up");
        assert!((dropped_pct - out.dropped as f32).abs() < 1e-3);
    }

    #[test]
    fn exhausted_when_strategy_gives_up() {
        let items: Vec<u32> = (0..100).collect();
        let mut bag = Bag {
            items,
            truncated: None,
        };
        // Strategy refuses to drop — budget is too small to ever fit.
        let out = fit_to_budget(&mut bag, 10, |_| DropOutcome::Exhausted)
            .expect("exhausted strategy still surfaces a Truncated record");
        assert!(out.still_over_budget);
        assert_eq!(out.dropped, 0);
    }

    #[test]
    fn budget_zero_disables_trimming() {
        let items: Vec<u32> = (0..1_000).collect();
        let mut bag = Bag {
            items,
            truncated: None,
        };
        let out = fit_to_budget(&mut bag, 0, |_| {
            panic!("drop_one must not be called when budget==0")
        });
        assert!(out.is_none());
    }
}
