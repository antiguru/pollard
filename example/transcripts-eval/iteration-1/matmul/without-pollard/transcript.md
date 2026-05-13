# matmul performance investigation (without pollard)

> I have a program `matmul` and noticed it's slow. Please profile and find the root cause.

## Inputs

* Profile: `/tmp/claude-1000/pf/matmul-slow.json.gz` (samply, Firefox JSON, cycles @ 1 kHz)
* Binary: `/home/moritz/dev/repos/pollard/target/demo/matmul` (Rust, debug info, `demo` profile)
* Source: `/home/moritz/dev/repos/pollard/example/src/bin/matmul.rs`

## Step 1 — Inspect the profile shape

I can't use pollard MCP tools, so I parse the Firefox JSON directly.

```text
top-level keys: ['meta', 'libs', 'threads', 'pages', 'profilerOverhead', 'counters']
threads: 2  (samply overhead + matmul)
matmul thread: 2841 samples
```

## Step 2 — Self-time per leaf frame

Walked `samples.stack` -> `stackTable.frame` -> `frameTable.func` -> `funcTable.name` for each sample. Names come back as bare hex strings (samply did not symbolicate in this profile, only kept raw addresses as names):

```text
=== top self-time addresses (matmul thread, 2841 samples) ===
   372 (13.1%)  0xb7eb
   368 (13.0%)  0xb7d8
   366 (12.9%)  0xb7c0
   365 (12.8%)  0xb7ac
   356 (12.5%)  0xb7b7
   355 (12.5%)  0xb7c4
   310 (10.9%)  0xb7d4
   307 (10.8%)  0xb7e7
     9 ( 0.3%)  0xb7ff
     8 ( 0.3%)  0xb55a
```

Eight addresses in a 64-byte span (`0xb7ac..0xb7eb`) hold ~98% of the work.

## Step 3 — Resolve those addresses

```text
$ printf "0xb7eb\n0xb7d8\n0xb7c0\n0xb7ac\n0xb7b7\n0xb7c4\n0xb7d4\n0xb7e7\n" \
  | addr2line -e target/demo/matmul -fipC
<usize as core::cmp::PartialOrd>::lt at .../core/src/cmp.rs:1916
 (inlined by) Range::next at .../range.rs:857
 (inlined by) matmul::matmul_slow at example/src/bin/matmul.rs:21
 (inlined by) matmul::main at example/src/bin/matmul.rs:59
matmul::matmul_slow at example/src/bin/matmul.rs:22  (x7)
 (inlined by) matmul::main at example/src/bin/matmul.rs:59
```

Everything is inlined into `main`. Hot self-time is concentrated on `matmul.rs:22`, the inner-k expression `s += a[i * N + k] * b[k * N + j]`.

## Step 4 — Look at the inner loop in source

`example/src/bin/matmul.rs`:

```rust
const N: usize = 1280;
fn matmul_slow(a: &[f32], b: &[f32], c: &mut [f32]) {
    for i in 0..N {
        for j in 0..N {
            let mut s = 0.0f32;
            for k in 0..N {
                s += a[i * N + k] * b[k * N + j];   // line 22
            }
            c[i * N + j] = s;
        }
    }
}
```

The inner-k loop walks `b[k * N + j]` — stepping `k` advances by a full row of B per iteration. With `N=1280` and `f32`, that is a `1280 * 4 = 5120`-byte (= `0x1400`) stride — guaranteed cache miss per load once `b` exceeds L2.

## Step 5 — Confirm with disassembly around the hot block

```text
$ objdump -d --start-address=0xb780 --stop-address=0xb820 target/demo/matmul
b790: lea    0x1(%rdi),%r8
b794: xorps  %xmm0,%xmm0
b797: mov    $0x3,%r9d
b79d: mov    %rsi,%r10
b7a0: movss  -0xc(%rcx,%r9,4),%xmm1        ; a[i*N+k]
b7a7: mulss  (%r10),%xmm1                  ; b[(k+0)*N + j]
b7ac: movss  -0x8(%rcx,%r9,4),%xmm2        ; a[i*N+k+1]
b7b3: addss  %xmm0,%xmm1
b7b7: mulss  0x1400(%r10),%xmm2            ; b[(k+1)*N + j]   <-- +5120
b7c0: addss  %xmm1,%xmm2
b7c4: movss  -0x4(%rcx,%r9,4),%xmm1
b7cb: mulss  0x2800(%r10),%xmm1            ; b[(k+2)*N + j]   <-- +10240
b7d4: addss  %xmm2,%xmm1
b7d8: movss  (%rcx,%r9,4),%xmm0
b7de: mulss  0x3c00(%r10),%xmm0            ; b[(k+3)*N + j]   <-- +15360
b7e7: addss  %xmm1,%xmm0
b7eb: add    $0x4,%r9
b7ef: add    $0x5000,%r10                  ; advance 4 rows of B
b7f6: cmp    $0x503,%r9
b7fd: jne    b7a0
```

The compiler 4x-unrolled the k-loop. The four loads from B are scalar `movss` at strides of `0x0, 0x1400, 0x2800, 0x3c00` — exactly one B row apart (`0x1400 = 5120 = 1280 * 4`). Per outer-k step, `%r10 += 0x5000` = four rows. B is `1280 * 1280 * 4 = 6.25 MiB`, well past L1/L2 — every B load is a cache miss. A is loaded sequentially (4 consecutive f32 from `(%rcx,%r9,4)`) so A stays in cache.

The eight hottest instructions (each 10–13% self) are exactly the four `mulss <stride>(%r10),%xmm?` loads from B and the four `addss` ops dependent on them. Cycles attribute to the instruction retiring after the stalled load, which is consistent with what we see.

## Diagnosis

Cache-unfriendly access pattern. The textbook ijk loop in `matmul_slow` strides B by one column per inner-loop iteration (`b[k * N + j]`), so each iteration takes a fresh cache line and the L1/L2 hit rate collapses. ~98% of cycles fall inside the eight unrolled-inner-loop instructions, all loading/consuming B.

I would have liked programmatic source-and-asm interleaving (pollard `source_for_function` / `asm_for_function`), and a quick way to see per-line self-time without hand-scripting addr2line. `samply load` would also give the Firefox UI for browsing, but it can't run in this sandbox (no GUI, no port forward).

## Fix

Swap the inner two loops to ikj order. The math is identical (associative-ish for f32; tiny rounding differences expected); the access pattern becomes row-major on B, so each cache line is reused N times. This is exactly `matmul_fast` already in the file:

```rust
fn matmul_fast(a: &[f32], b: &[f32], c: &mut [f32]) {
    for i in 0..N {
        for k in 0..N {
            let a_ik = a[i * N + k];     // hoisted, row-stride on A
            for j in 0..N {
                c[i * N + j] += a_ik * b[k * N + j];   // row-stride on B and C
            }
        }
    }
}
```

So the fix is to either delete `matmul_slow` and call `matmul_fast`, or — if `matmul_slow` is the canonical implementation — replace its body with the ikj form above. The j-loop is then trivially vectorizable and B is read sequentially.
