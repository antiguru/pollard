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
//! "drop the lowest-priority element" strategy via [`fit_to_budget`].
//!
//! We serialize the unmodified output once to seed a running byte
//! counter, then ask the per-tool strategy to drop one element at a
//! time and report the bytes it freed. Subtracting from the running
//! counter avoids re-serializing the whole response on every drop —
//! the loop is `O(n)` in items dropped rather than `O(n²)`. The
//! reported `final_bytes` comes from one last serialization once the
//! running counter says we should fit, so the wire-truth number the
//! caller sees is accurate even if individual drop estimates were
//! slightly off.
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

/// Resolve the operator-side output byte ceiling. Reads
/// `POLLARD_MAX_OUTPUT_BYTES` if set and parsable; otherwise returns
/// [`DEFAULT_BUDGET_BYTES`]. Set the env var to `0` to disable
/// trimming entirely (useful for tests / debugging).
///
/// This is the server-wide ceiling — the operator launching the MCP
/// server picks it based on the harness's actual cap. Per-call
/// `max_output_bytes` args narrow this further but cannot raise it,
/// since the harness will reject anything above the ceiling
/// regardless of what the caller asked for.
pub fn output_budget_bytes() -> usize {
    std::env::var("POLLARD_MAX_OUTPUT_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_BUDGET_BYTES)
}

/// Combine an operator-side ceiling with an optional per-call request.
/// `request=Some(n)` clamps the budget to `min(n, ceiling)`; `None`
/// uses the ceiling as-is. `request=Some(0)` disables trimming for
/// the call (consistent with the env-var "0 disables" knob).
pub fn resolve_budget(ceiling: usize, request: Option<usize>) -> usize {
    match request {
        Some(0) => 0,
        Some(n) => n.min(ceiling),
        None => ceiling,
    }
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
    /// One item was removed.
    ///
    /// `pct` is its priority weight (e.g. `self_pct`) for the
    /// rolled-up `dropped_pct` summary, or `None` when the strategy
    /// can't attribute one.
    ///
    /// `bytes` is a best-effort estimate of the JSON bytes freed by
    /// the drop. Slight over-estimates cost an extra trim iteration
    /// (we'll loop one more time when we already fit) but never an
    /// unsafe one — the final size is re-measured before we report
    /// `final_bytes`. Strings serialize 1:1 (modulo escapes); structs
    /// add a near-constant per-field overhead, so for most rows
    /// `serde_json::to_vec(&row).len() + 1` is the cheap exact answer.
    Dropped { pct: Option<f32>, bytes: usize },
    /// Nothing left to drop without producing an empty/meaningless
    /// response. The trim loop stops and `still_over_budget` is set.
    Exhausted,
}

/// Serialize `output` once to seed a running byte counter, then while
/// the running estimate exceeds `budget`, ask `drop_one` to remove the
/// lowest-priority element and report how many bytes it freed.
/// Returns `None` if the first serialization already fit, otherwise a
/// [`Truncated`] summary.
///
/// `drop_one` mutates `output` in place and must report `bytes`
/// freed so we don't have to re-serialize the whole response on every
/// drop — that re-serialization is the difference between an `O(n)`
/// and an `O(n²)` trim loop on a tool with hundreds of rows.
pub fn fit_to_budget<T, F>(output: &mut T, budget: usize, mut drop_one: F) -> Option<Truncated>
where
    T: Serialize,
    F: FnMut(&mut T) -> DropOutcome,
{
    if budget == 0 {
        return None;
    }
    let initial = serialized_size(output);
    if initial <= budget {
        return None;
    }
    let mut running = initial;
    let mut dropped: usize = 0;
    let mut dropped_pct: f32 = 0.0;
    let mut had_pct = false;
    let mut exhausted = false;
    while running > budget {
        match drop_one(output) {
            DropOutcome::Dropped { pct, bytes } => {
                dropped += 1;
                if let Some(p) = pct {
                    dropped_pct += p;
                    had_pct = true;
                }
                running = running.saturating_sub(bytes);
            }
            DropOutcome::Exhausted => {
                exhausted = true;
                break;
            }
        }
    }
    if dropped == 0 && !exhausted {
        return None;
    }
    // One precise measurement so the reported size is wire-truth even
    // when individual drop estimates were rough.
    let final_bytes = serialized_size(output);
    Some(Truncated {
        dropped,
        dropped_pct: had_pct.then_some(dropped_pct),
        budget_bytes: budget,
        final_bytes,
        still_over_budget: exhausted || final_bytes > budget,
    })
}

fn serialized_size<T: Serialize>(output: &T) -> usize {
    // Errors are silently treated as 0 — an unserializable output
    // would also fail at the rmcp layer, and 0 makes the trim loop
    // terminate rather than spin.
    crate::serde_util::serialized_byte_count(output)
}

/// Approximate the serialized size of `value` for use as the `bytes`
/// field on [`DropOutcome::Dropped`]. Adds a `+ 1` to cover the
/// trailing comma between array elements (slight overestimate when
/// dropping the last item; harmless). Counts bytes through a
/// non-allocating `io::Write` sink rather than materializing a
/// `Vec<u8>` per call — see [`crate::serde_util::serialized_byte_count`].
pub fn estimated_bytes<T: Serialize>(value: &T) -> usize {
    crate::serde_util::serialized_byte_count(value) + 1
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
            DropOutcome::Dropped {
                pct: None,
                bytes: 0,
            }
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
        let out = fit_to_budget(&mut bag, 200, |b| match b.items.pop() {
            Some(v) => DropOutcome::Dropped {
                pct: Some(1.0),
                bytes: estimated_bytes(&v),
            },
            None => DropOutcome::Exhausted,
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

    /// The whole point of the `bytes` field on `DropOutcome::Dropped`
    /// is to keep the trim loop linear. Verify that the helper only
    /// re-serializes once at the end (not on every drop) by counting
    /// how many times the strategy is called against an oracle.
    #[test]
    fn drop_count_matches_byte_freed_arithmetic() {
        // Each item is "0..N" — average serialized cost roughly 4
        // bytes (digits + comma). 500 items + envelope ≈ 2500 bytes.
        // Dropping ~ (initial - budget) / per_item items should fit.
        let items: Vec<u32> = (0..500).collect();
        let initial_size = serialized_size(&Bag {
            items: items.clone(),
            truncated: None,
        });
        let budget = 200;
        let mut bag = Bag {
            items,
            truncated: None,
        };

        let mut call_count = 0usize;
        let out = fit_to_budget(&mut bag, budget, |b| {
            call_count += 1;
            match b.items.pop() {
                Some(v) => DropOutcome::Dropped {
                    pct: None,
                    bytes: estimated_bytes(&v),
                },
                None => DropOutcome::Exhausted,
            }
        })
        .expect("trimmed");

        // Sanity: we trimmed enough to fit, in the same number of
        // drops as the running counter would predict.
        let approx_per_item = initial_size as f32 / 500.0;
        let needed_drops = ((initial_size - budget) as f32 / approx_per_item).ceil() as usize;
        assert!(
            call_count <= needed_drops + 2,
            "drops {call_count} far exceeds prediction {needed_drops}"
        );
        assert!(out.final_bytes <= budget);
    }

    #[test]
    fn resolve_budget_none_uses_ceiling() {
        assert_eq!(resolve_budget(25_000, None), 25_000);
    }

    #[test]
    fn resolve_budget_request_below_ceiling_passes_through() {
        assert_eq!(resolve_budget(25_000, Some(8_000)), 8_000);
    }

    #[test]
    fn resolve_budget_request_above_ceiling_clamps_to_ceiling() {
        // The harness's hard cap is what the operator-side ceiling
        // represents — callers can't escape it by asking for more.
        assert_eq!(resolve_budget(25_000, Some(1_000_000)), 25_000);
    }

    #[test]
    fn resolve_budget_zero_disables_trimming_regardless_of_ceiling() {
        // Mirrors the env-var "0 disables" semantic.
        assert_eq!(resolve_budget(25_000, Some(0)), 0);
    }
}
