# Showcase: `andialbrecht/sqlparse` (2.55x end-to-end, up to 10.5x on upstream benchmarks)

[sqlparse](https://github.com/andialbrecht/sqlparse) is a widely used pure-Python SQL parser and formatter (~4k stars, 30M+ PyPI downloads/month) backing Django Debug Toolbar, pgcli, and SQL tooling across the Python ecosystem.

Using `oa` with Python `cProfile+tracemalloc` and the full 537-test upstream pytest suite locked under Cheat Guard, 10 experiments (9 kept, 1 regression automatically rejected) sped up end-to-end `sqlparse.format(..., reindent=True, keyword_case="upper", strip_comments=True)` by **2.55x** (`253.94 ms -> 99.82 ms`, 95% CI `[2.53x, 2.55x]`, Mann-Whitney `p = 0.0000`, `-7.9%` peak RSS) while producing byte-identical output (`sha256: 3b449d533e65a917`). On sqlparse's own official `benchmarks/*.py` suite, the same patch runs **4.3x to 10.5x faster**.

- Upstream commit: [`0151463c5c`](https://github.com/andialbrecht/sqlparse/commit/0151463c5cc727532c8e20e8e3434131ba1d4816)
- Verified patch: [sqlparse.patch](sqlparse.patch)
- Upstream test suite: `uv run pytest -q` (537 passed, 2 xfailed, 1 xpassed; wall time dropped from `0.91s` to `0.29s`)
- Locked files: 37 files (entire `tests/` directory plus benchmark script)
- Host: Linux `x86_64` (AMD Ryzen 7 9800X3D)

## Head-to-head results (20 balanced interleaved pairs)

| Benchmark | Baseline | After `oa` | Gain | Notes |
| --- | ---: | ---: | ---: | --- |
| `sqlparse.format` (150 multi-join queries) | 253.94 ms | 99.82 ms | **2.55x** | 95% CI `[2.53x, 2.55x]`, `p = 0.0000`, `-7.9%` RSS |
| Upstream `bench_grouping.py` (nested parens, `n=2000`) | 68.1 ms | 6.5 ms | **10.5x** | `sqlparse.parse` official suite |
| Upstream `bench_grouping.py` (wide columns, `n=4000`) | 124.0 ms | 15.1 ms | **8.2x** | `sqlparse.parse` official suite |
| Upstream `bench_grouping.py` (nested `CASE`, `n=400`) | 58.3 ms | 9.3 ms | **6.3x** | `sqlparse.parse` official suite |
| Upstream `bench_lexer_delimiters.py` (`n=8000`, 278.9 kB) | 789.9 ms | 124.3 ms | **6.4x** | `sqlparse.parse` official suite |
| Upstream `bench_group_comments.py` (`n=8000`, 40.0 kB) | 58.1 ms | 13.5 ms | **4.3x** | `sqlparse.parse` official suite |

## What changed (experiment ledger)

| # | Status | Hypothesis | Files | Step gain | 95% CI | p-value | Median ms |
| ---: | --- | --- | --- | ---: | --- | ---: | ---: |
| 1 | KEPT | Combine 47 per-position lexer regexes into one master regex and flatten keyword lookup dict | `sqlparse/lexer.py` | 1.06x | [1.05x, 1.09x] | 0.0002 | 241.30 |
| 2 | KEPT | Cache `_TokenType._ancestors` frozensets and inline `token_next_by`, `token_next`, `token_first`, `Token.match` | `sqlparse/sql.py`, `sqlparse/tokens.py`, `sqlparse/utils.py` | 1.39x | [1.38x, 1.39x] | 0.0002 | 172.91 |
| 3 | KEPT | Skip absent AST grouping passes via single-pass token inventory and inline `_group_matching` | `sqlparse/engine/grouping.py` | 1.15x | [1.15x, 1.16x] | 0.0002 | 149.66 |
| 4 | KEPT | Precompute `_TokenType` boolean flags, precompile `ReindentFilter` keyword regex, fast-path `token_next_by(i, t)` | `sqlparse/engine/grouping.py`, `sqlparse/filters/reindent.py`, `sqlparse/sql.py`, `sqlparse/tokens.py` | 1.09x | [1.09x, 1.10x] | 0.0002 | 137.31 |
| 5 | KEPT | Single-char lexer fast path, zero-copy `_group`, inline `group_identifier`/`group_aliased`/`group_order` | `sqlparse/engine/grouping.py`, `sqlparse/lexer.py`, `sqlparse/sql.py` | 1.06x | [1.06x, 1.07x] | 0.0000 | 128.24 |
| 6 | KEPT | Zero-copy `_group` loop with `match_ttype` fast guard | `sqlparse/engine/grouping.py` | 1.04x | [1.04x, 1.04x] | 0.0005 | 123.55 |
| 7 | KEPT | Inline `StatementSplitter` fast path, `ReindentFilter` dict dispatch/cache, skip 1-token leaves in `_group` | `sqlparse/engine/grouping.py`, `sqlparse/engine/statement_splitter.py`, `sqlparse/filters/reindent.py`, `sqlparse/sql.py` | 1.06x | [1.06x, 1.07x] | 0.0000 | 114.79 |
| 8 | KEPT | ASCII fast-word lexer, dual-case keyword dict, lazy `_SQL_REGEX`, iterative `TokenList.__str__` | `sqlparse/lexer.py`, `sqlparse/sql.py` | 1.11x | [1.10x, 1.12x] | 0.0000 | 104.12 |
| 9 | REVERTED | Replace fast-path lexer with C `re.finditer` over 46-branch master regex | `sqlparse/engine/filter_stack.py`, `sqlparse/filters/tokens.py`, `sqlparse/lexer.py` | 0.96x | [0.96x, 0.96x] | 0.0000 | 108.88 |
| 10 | KEPT | Module-level frozenset grouping predicates, `match_val` fast path, inline `group_where`/`group_functions`/`group_over`, fast `KeywordCaseFilter` | `sqlparse/engine/grouping.py`, `sqlparse/filters/tokens.py` | 1.05x | [1.04x, 1.05x] | 0.0000 | 99.12 |

Experiment #9 looked appealing on paper (moving the scan loop into C via `re.finditer`), yet `oa evaluate` caught a `0.96x` regression (`p = 0.0000`, `+6.7%` peak RSS) because backtracking across 46 regex branches on every space and identifier was slower than the single-char and ASCII word fast paths. `oa checkpoint --revert` restored the tree and logged the dead end.

## Profile diff (`oa profile --diff-from baseline`)

```text
Diff vs baseline: 1866.23 ms -> 742.24 ms (-60.2% profiled CPU)
   -263.41 ms ( -87.5%)  <method 'match' of 're.Pattern' objects>
   -170.03 ms (-100.0%)  imt (sqlparse/utils.py)
   -150.49 ms (-100.0%)  _group_matching (sqlparse/engine/grouping.py)
   -108.31 ms (-100.0%)  _token_matching (sqlparse/sql.py)
    -67.61 ms ( -43.9%)  get_tokens (sqlparse/lexer.py)
    -65.16 ms ( -44.8%)  _group (sqlparse/engine/grouping.py)
```

## Reproduce

```bash
git clone https://github.com/andialbrecht/sqlparse.git
cd sqlparse
git checkout 0151463c5cc727532c8e20e8e3434131ba1d4816
cat << 'EOF' > bench_oa.py
import hashlib, sqlparse

SQL = "\n".join(
    f"SELECT u.id, u.email, COUNT(o.id) AS total_orders, SUM(o.amount) "
    f"FROM users u LEFT JOIN orders o ON o.user_id = u.id "
    f"WHERE u.status = {i % 5} AND o.created_at > '2026-01-01' "
    f"GROUP BY u.id, u.email HAVING COUNT(o.id) > 2 "
    f"ORDER BY total_orders DESC LIMIT 50;"
    for i in range(150)
)

out = sqlparse.format(SQL, reindent=True, keyword_case="upper", strip_comments=True)
print(len(out), hashlib.sha256(out.encode()).hexdigest())
EOF
git add bench_oa.py && git commit -m "add bench"
oa init --bench "python3 bench_oa.py" --test "pytest -q" --lock bench_oa.py --pairs 20
git apply /path/to/oa/docs/showcases/sqlparse.patch
oa evaluate --against baseline --pairs 20
```
