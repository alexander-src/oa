# Showcase: `Python-Markdown/markdown` (1.92x convert / 1.53x CLI across 1,230 tests)

[Python-Markdown](https://github.com/Python-Markdown/markdown) (~4.1k stars, 25M+ PyPI downloads/month) is the reference Python Markdown renderer powering MkDocs and hundreds of static site generators and documentation pipelines.

We benchmarked `markdown.Markdown(extensions=["extra", "toc"])` against Python-Markdown's own 306 KB documentation corpus (`docs/**/*.md`, 32 real technical Markdown files containing tables, code fences, links, footnotes, and headings) while locking all 334 test files (1,230 upstream unit tests) under Cheat Guard.

`oa profile` exposed three major bottlenecks, including a real quadratic `''.join(c.itertext())` walk inside `TocTreeprocessor.replace_marker` that concatenated entire subtrees even though only leaf nodes (`len(c) == 0`) can match the TOC marker. Three kept patches cut pure conversion time by **1.92x** (`497.25 ms -> 258.76 ms` across 5 corpus passes / 1.53 MB) and end-to-end CLI wall time including Python cold start by **1.53x** (`138.37 ms -> 90.27 ms`, 95% CI `[1.52x, 1.53x]`, Mann-Whitney `p = 0.0000`), passing all **1,230/1,230** upstream tests with identical HTML SHA-256 (`a59d5b397b34040c`).

- Upstream commit: [`27c0141980`](https://github.com/Python-Markdown/markdown/commit/27c014198021f71442301319981a3ed366ae2ad1)
- Verified patch: [python-markdown.patch](python-markdown.patch)
- Upstream test suite: `python -m unittest discover tests -q` (1,230 ran, 0 failures, 52 skipped)
- Locked files: 334 files (all `tests/` fixtures and unit tests plus benchmark script)
- Host: Linux `x86_64` (AMD Ryzen 7 9800X3D)

## Head-to-head results

| Workload | Baseline | After `oa` | Gain | Notes |
| --- | ---: | ---: | ---: | --- |
| Full CLI (`docs/**/*.md`, 306 KB, cold Python start) | 138.37 ms | 90.27 ms | **1.53x** | 20 paired runs, 95% CI `[1.52x, 1.53x]`, `p = 0.0000` |
| Pure `md.convert` loop (5x `docs/**/*.md`, 1.53 MB) | 497.25 ms | 258.76 ms | **1.92x** | Excludes fixed interpreter spawn and import |

## What changed (experiment ledger)

| # | Status | Hypothesis | Files | Step gain | 95% CI | p-value | Median ms |
| ---: | --- | --- | --- | ---: | --- | ---: | ---: |
| 1 | KEPT | Index only `[` and `]` transitions with a C regex + `bisect_right` instead of scanning every character in `LinkInlineProcessor.getText` | `markdown/inlinepatterns.py` | 1.09x | [1.08x, 1.10x] | 0.0000 | 127.20 |
| 2 | KEPT | Pre-snapshot inline patterns with provable literal triggers, skip placeholder pass on plain text, add `STX` guard in `UnescapeTreeprocessor` | `markdown/treeprocessors.py` | 1.20x | [1.19x, 1.20x] | 0.0000 | 107.54 |
| 3 | KEPT | Remove O(N^2) `itertext` in `toc.replace_marker`, O(1) `is_block_level` shadow set, block/attr_list literal guards, fast HTML serializer | `markdown/core.py`, `markdown/blockparser.py`, `markdown/blockprocessors.py`, `markdown/serializers.py`, `markdown/extensions/{attr_list,def_list,footnotes,tables,toc}.py` | 1.18x | [1.18x, 1.19x] | 0.0000 | 90.21 |
| 4 | REVERTED | Hoist `RawHtmlPostprocessor` regex and fast-path leaf headings in `toc.render_inner_html` | `markdown/extensions/toc.py`, `markdown/postprocessors.py` | 1.01x | [1.00x, 1.02x] | 0.1572 | 90.11 |

During experiment #3, an overly strict `'\n' in block` guard on `DefListProcessor.test` broke `tests.test_legacy.TestExtensions.test_attr_list` (single-line continuation `<dd>` blocks have no newline). `oa evaluate` returned `TestsFail` immediately before benchmarking, isolating the bug in under a second. Experiment #4 then came in at `1.01x` (`p = 0.1572`) and was automatically rejected as `NoiseReject`.

## Profile diff (`oa profile --diff-from baseline`)

```text
Diff vs baseline: 1529.41 ms -> 849.17 ms (-44.5% profiled CPU)
   -146.72 ms (-100.0%)  _findBracketEnds (markdown/inlinepatterns.py)
   -101.00 ms (-100.0%)  __applyPattern (markdown/treeprocessors.py)
    -65.64 ms ( -55.3%)  <method 'append' of 'list' objects>
    -58.58 ms (-100.0%)  <method 'finditer' of 're.Pattern' objects>
    -55.36 ms ( -41.3%)  _serialize_html (markdown/serializers.py)
    -52.03 ms (-100.0%)  <method 'lower' of 'str' objects>
```

## Reproduce

```bash
git clone https://github.com/Python-Markdown/markdown.git
cd markdown
git checkout 27c014198021f71442301319981a3ed366ae2ad1
uv sync --extra testing
cat << 'EOF' > bench_oa.py
import glob, hashlib, markdown

docs = [open(p, encoding="utf-8").read() for p in sorted(glob.glob("docs/**/*.md", recursive=True))]
md = markdown.Markdown(extensions=["extra", "toc"])
h = hashlib.sha256()
total_bytes = 0
for text in docs:
    md.reset()
    b = md.convert(text).encode("utf-8")
    total_bytes += len(b)
    h.update(b)
print(f"files={len(docs)} bytes={total_bytes} sha256={h.hexdigest()[:16]}")
EOF
git add bench_oa.py && git commit -m "add bench"
oa init --bench ".venv/bin/python bench_oa.py" --test ".venv/bin/python -m unittest discover tests -q" --lock bench_oa.py --pairs 20
git apply /path/to/oa/docs/showcases/python-markdown.patch
oa evaluate --against baseline --pairs 20
```
