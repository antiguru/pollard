//! `compare_functions`: side-by-side asm diff between two functions.
//!
//! Aligns the two disassemblies row-by-row using LCS over a normalized
//! instruction key (registers → `R`, numeric immediates → `IMM`,
//! mnemonic + operand shape preserved). Without normalization, register
//! renames and differing displacements would split every nominally-equal
//! instruction into two unaligned rows and the per-instruction sample
//! columns would no longer line up — defeating the whole point.
//!
//! Both sides may live in the same loaded profile (`profile_b == profile_a`)
//! or in different profiles (e.g. before/after a refactor). The caller
//! controls scope via `profile_id_b` at the tool layer.
//!
//! The displayed asm text is unchanged; normalization is alignment-only.

use crate::error::ToolError;
use crate::profile::Profile;
use crate::query::asm::{self, AsmInstruction};
use regex::Regex;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::OnceLock;

#[derive(Debug, Default)]
pub struct Args {
    pub function_a: String,
    pub module_a: Option<String>,
    pub function_b: String,
    pub module_b: Option<String>,
    pub with_samples: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub function_a: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_a: Option<String>,
    pub function_b: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_b: Option<String>,
    /// Architecture string from the disassembler. Both sides must agree;
    /// a mismatch (e.g. comparing an x86_64 binary to an aarch64 one)
    /// returns an error rather than producing a meaningless alignment.
    pub arch: String,
    pub total_samples_a: u64,
    pub total_samples_b: u64,
    pub rows: Vec<AlignedRow>,
}

/// One row of the side-by-side rendering.
///
/// * Both sides populated → matched pair (normalized keys equal).
/// * Only one side populated → instruction present on that side, no
///   structural counterpart on the other (a "gap" in LCS terms).
#[derive(Debug, Default, Serialize, JsonSchema, PartialEq)]
pub struct AlignedRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_a: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asm_a: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples_a: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_b: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asm_b: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples_b: Option<u64>,
}

pub async fn compare_functions(
    profile_a: &Profile,
    profile_b: &Profile,
    args: &Args,
) -> Result<Output, ToolError> {
    let listing_a = asm::asm_for_function(
        profile_a,
        &asm::Args {
            function: args.function_a.clone(),
            module: args.module_a.clone(),
            with_samples: args.with_samples,
        },
    )
    .await?;
    let listing_b = asm::asm_for_function(
        profile_b,
        &asm::Args {
            function: args.function_b.clone(),
            module: args.module_b.clone(),
            with_samples: args.with_samples,
        },
    )
    .await?;

    if listing_a.arch != listing_b.arch {
        return Err(ToolError::Internal {
            message: format!(
                "arch mismatch between sides: a={} b={}",
                listing_a.arch, listing_b.arch
            ),
        });
    }

    let total_samples_a = listing_a.instructions.iter().map(|i| i.samples).sum();
    let total_samples_b = listing_b.instructions.iter().map(|i| i.samples).sum();

    let key_a: Vec<String> = listing_a
        .instructions
        .iter()
        .map(|i| normalize(&i.asm))
        .collect();
    let key_b: Vec<String> = listing_b
        .instructions
        .iter()
        .map(|i| normalize(&i.asm))
        .collect();

    let rows = align(
        &listing_a.instructions,
        &listing_b.instructions,
        &key_a,
        &key_b,
    );

    Ok(Output {
        function_a: listing_a.function,
        module_a: listing_a.module,
        function_b: listing_b.function,
        module_b: listing_b.module,
        arch: listing_a.arch,
        total_samples_a,
        total_samples_b,
        rows,
    })
}

// ─── normalization ──────────────────────────────────────────────────────────

/// Tokens we collapse for alignment. Order matters: hex literals come
/// before generic decimals so `0x...` doesn't get half-eaten by the
/// `\d+` arm. Register alternatives are listed long-first within each
/// architecture cluster to defeat regex engine ambiguity.
fn normalize_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            // x86 SIMD / x87
            r"\b(?:[xy]mm[0-9]+|st\([0-9]\)|st[0-9])\b",
            // x86 GPRs (long names first within each width)
            r"|\b(?:r[abcd]x|e[abcd]x|[abcd]x|[abcd][lh]",
            r"|rsp|esp|spl|sp",
            r"|rbp|ebp|bpl|bp",
            r"|rsi|esi|sil|si",
            r"|rdi|edi|dil|di",
            r"|r(?:8|9|1[0-5])[bdwl]?",
            r"|rip|eip|ip)\b",
            // aarch64 GPRs / SIMD
            r"|\b(?:[xw](?:3[01]|[12][0-9]|[0-9])",
            r"|sp|wsp|[xw]zr",
            r"|[vqdshb][0-9]+)\b",
            // hex / decimal immediates (hex first)
            r"|0x[0-9a-fA-F]+",
            r"|\b\d+\b",
        ))
        .unwrap()
    })
}

/// Collapse a raw asm string into an alignment key. The displayed asm
/// in the output is the raw text — this is only for LCS pairing.
pub(crate) fn normalize(asm: &str) -> String {
    normalize_re()
        .replace_all(asm, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap().as_str();
            let first = m.as_bytes().first().copied().unwrap_or(0);
            if m.starts_with("0x") || first.is_ascii_digit() {
                "IMM"
            } else {
                "R"
            }
        })
        .into_owned()
}

// ─── LCS alignment ──────────────────────────────────────────────────────────

/// Suffix-LCS DP + greedy backtrack. `dp[i][j]` is the LCS of
/// `key_a[i..]` and `key_b[j..]`; tied gaps prefer advancing `a` (matches
/// the conventional left-side bias of unified diffs and keeps the row
/// order stable for repeat keys).
pub(crate) fn align(
    a: &[AsmInstruction],
    b: &[AsmInstruction],
    key_a: &[String],
    key_b: &[String],
) -> Vec<AlignedRow> {
    let n = a.len();
    let m = b.len();
    debug_assert_eq!(n, key_a.len());
    debug_assert_eq!(m, key_b.len());

    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if key_a[i] == key_b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut rows = Vec::with_capacity(n + m);
    let mut i = 0;
    let mut j = 0;
    while i < n && j < m {
        if key_a[i] == key_b[j] {
            rows.push(matched_row(&a[i], &b[j]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            rows.push(only_a_row(&a[i]));
            i += 1;
        } else {
            rows.push(only_b_row(&b[j]));
            j += 1;
        }
    }
    while i < n {
        rows.push(only_a_row(&a[i]));
        i += 1;
    }
    while j < m {
        rows.push(only_b_row(&b[j]));
        j += 1;
    }
    rows
}

fn matched_row(ai: &AsmInstruction, bi: &AsmInstruction) -> AlignedRow {
    AlignedRow {
        offset_a: Some(ai.offset),
        asm_a: Some(ai.asm.clone()),
        samples_a: Some(ai.samples),
        offset_b: Some(bi.offset),
        asm_b: Some(bi.asm.clone()),
        samples_b: Some(bi.samples),
    }
}

fn only_a_row(ai: &AsmInstruction) -> AlignedRow {
    AlignedRow {
        offset_a: Some(ai.offset),
        asm_a: Some(ai.asm.clone()),
        samples_a: Some(ai.samples),
        ..Default::default()
    }
}

fn only_b_row(bi: &AsmInstruction) -> AlignedRow {
    AlignedRow {
        offset_b: Some(bi.offset),
        asm_b: Some(bi.asm.clone()),
        samples_b: Some(bi.samples),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(offset: u32, asm: &str, samples: u64) -> AsmInstruction {
        AsmInstruction {
            offset,
            asm: asm.to_owned(),
            samples,
        }
    }

    #[test]
    fn normalize_collapses_x86_registers_and_immediates() {
        assert_eq!(
            normalize("addsd xmm0, qword [rdi + rcx * 8 - 0x38]"),
            "addsd R, qword [R + R * IMM - IMM]"
        );
        assert_eq!(
            normalize("addsd xmm0, qword [rdi + rcx * 1 - 0x6000]"),
            "addsd R, qword [R + R * IMM - IMM]"
        );
        assert_eq!(normalize("xor eax, eax"), "xor R, R");
        assert_eq!(normalize("xorpd xmm0, xmm0"), "xorpd R, R");
        assert_eq!(normalize("add rcx, 0x8000"), "add R, IMM");
        assert_eq!(normalize("inc rax"), "inc R");
        assert_eq!(normalize("ret"), "ret");
    }

    #[test]
    fn normalize_handles_aarch64() {
        assert_eq!(normalize("ldr x0, [sp, #16]"), "ldr R, [R, #IMM]");
        assert_eq!(
            normalize("fadd v0.2d, v1.2d, v2.2d"),
            "fadd R.2d, R.2d, R.2d"
        );
    }

    #[test]
    fn normalize_keeps_mnemonic_text() {
        // Mnemonic must not be mistaken for a register or immediate.
        assert_eq!(normalize("call 0x1234"), "call IMM");
        assert_eq!(normalize("jnz 0x13ba0"), "jnz IMM");
    }

    #[test]
    fn align_pairs_equal_normalized_rows() {
        // Two addsds on A, one on B — same addressing shape so they
        // normalize equal. LCS pairs the first addsd, leaves the second
        // as only-A.
        let a = vec![
            ins(0, "xor eax, eax", 0),
            ins(2, "addsd xmm0, [rdi - 0x10]", 100),
            ins(8, "addsd xmm0, [rdi - 0x8]", 100),
            ins(14, "ret", 0),
        ];
        let b = vec![
            ins(0, "xor ecx, ecx", 0),
            ins(2, "addsd xmm0, [rdi - 0x20]", 200),
            ins(8, "ret", 0),
        ];
        let key_a: Vec<String> = a.iter().map(|i| normalize(&i.asm)).collect();
        let key_b: Vec<String> = b.iter().map(|i| normalize(&i.asm)).collect();
        let rows = align(&a, &b, &key_a, &key_b);

        // Expected: xor↔xor, addsd↔addsd, only-A addsd, ret↔ret
        assert_eq!(rows.len(), 4);
        assert!(rows[0].asm_a.is_some() && rows[0].asm_b.is_some());
        assert_eq!(rows[0].asm_a.as_deref(), Some("xor eax, eax"));
        assert_eq!(rows[0].asm_b.as_deref(), Some("xor ecx, ecx"));

        assert!(rows[1].asm_a.is_some() && rows[1].asm_b.is_some());
        assert_eq!(rows[1].samples_a, Some(100));
        assert_eq!(rows[1].samples_b, Some(200));

        assert!(rows[2].asm_a.is_some() && rows[2].asm_b.is_none());
        assert_eq!(rows[2].asm_a.as_deref(), Some("addsd xmm0, [rdi - 0x8]"));

        assert!(rows[3].asm_a.is_some() && rows[3].asm_b.is_some());
        assert_eq!(rows[3].asm_a.as_deref(), Some("ret"));
    }

    #[test]
    fn align_disjoint_sequences_are_concatenated() {
        let a = vec![ins(0, "mov rax, 1", 5)];
        let b = vec![ins(0, "addsd xmm0, xmm1", 5)];
        let key_a: Vec<String> = a.iter().map(|i| normalize(&i.asm)).collect();
        let key_b: Vec<String> = b.iter().map(|i| normalize(&i.asm)).collect();
        let rows = align(&a, &b, &key_a, &key_b);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].asm_a.is_some() && rows[0].asm_b.is_none());
        assert!(rows[1].asm_a.is_none() && rows[1].asm_b.is_some());
    }

    #[test]
    fn align_identical_sequences_pair_one_to_one() {
        let a = vec![
            ins(0, "mov rax, 1", 1),
            ins(7, "add rax, 2", 2),
            ins(14, "ret", 3),
        ];
        let b = vec![
            ins(0, "mov rax, 1", 1),
            ins(7, "add rax, 2", 2),
            ins(14, "ret", 3),
        ];
        let key_a: Vec<String> = a.iter().map(|i| normalize(&i.asm)).collect();
        let key_b: Vec<String> = b.iter().map(|i| normalize(&i.asm)).collect();
        let rows = align(&a, &b, &key_a, &key_b);
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert!(row.asm_a.is_some() && row.asm_b.is_some());
            assert_eq!(row.samples_a, row.samples_b);
        }
    }

    #[test]
    fn align_unrolled_loop_pairs_first_n_addsds() {
        // Demo case: 8 addsd in A, 4 in B. LCS pairs the first four,
        // leaving 4 only-A rows. This is exactly the sum_rows vs
        // sum_cols shape from issue #8.
        let mut a = vec![ins(0, "xorpd xmm0, xmm0", 0)];
        for k in 0..8 {
            a.push(ins(2 + k * 6, "addsd xmm0, [rdi]", 200));
        }
        a.push(ins(50, "ret", 0));

        let mut b = vec![ins(0, "xorpd xmm0, xmm0", 0)];
        for k in 0..4 {
            b.push(ins(2 + k * 9, "addsd xmm0, [rdi]", 3500));
        }
        b.push(ins(38, "ret", 0));

        let key_a: Vec<String> = a.iter().map(|i| normalize(&i.asm)).collect();
        let key_b: Vec<String> = b.iter().map(|i| normalize(&i.asm)).collect();
        let rows = align(&a, &b, &key_a, &key_b);

        let paired = rows
            .iter()
            .filter(|r| r.asm_a.is_some() && r.asm_b.is_some())
            .count();
        let only_a = rows
            .iter()
            .filter(|r| r.asm_a.is_some() && r.asm_b.is_none())
            .count();
        let only_b = rows
            .iter()
            .filter(|r| r.asm_a.is_none() && r.asm_b.is_some())
            .count();

        // 1 xorpd + 4 addsd + 1 ret = 6 paired rows.
        assert_eq!(paired, 6);
        // 4 surplus addsd rows on A.
        assert_eq!(only_a, 4);
        assert_eq!(only_b, 0);
    }
}
