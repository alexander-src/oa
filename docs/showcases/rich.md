# Showcase: `Textualize/rich` (3.69x table render / 2.49x CLI across 954 tests)

[Rich](https://github.com/Textualize/rich) (52k+ stars) is the standard Python library for styled terminal tables, progress bars, syntax highlighting, and CLI output across `pip`, `pytest`, FastAPI, Typer, and thousands of developer tools.

We benchmarked rendering a 400-row, 6-column styled `Table` with Rich markup, mixed justification, status badges, and truecolor ANSI output while locking all 251 test files (954 upstream pytest tests) under Cheat Guard.

`oa profile` showed that `Table` parsed every string cell through `markup.render` and `_emoji_replace` up to three times per render (column width measurement, overflow re-measurement, and cell rendering), ran `_emoji_replace` regexes on plain strings with no colons, recreated identical `ConsoleOptions` 2,400 times inside the row loop, and double-ran `Console.render_lines` + `Segment.split_and_crop_lines` through `Padding` wrappers on every cell.

Three kept patches across 4 files sped up pure `Table` rendering by **3.69x** (`1,047.83 ms -> 283.63 ms` over 10 renders) and full CLI wall time including Python cold start and `import rich` by **2.49x** (`131.11 ms -> 52.63 ms`, 95% CI `[2.48x, 2.51x]`, Mann-Whitney `p = 0.0000`, `-20.3%` peak RSS), passing all **954/954** upstream tests with byte-identical ANSI escape output (`sha256: 00f0f9ad33b11b70`).

- Upstream commit: [`3a109ccdffe5`](https://github.com/Textualize/rich/commit/3a109ccdffe5b2d0e30684a304fbd388e99ce57b)
- Verified patch: [rich.patch](rich.patch)
- Upstream test suite: `pytest -q --ignore=tests/test_card.py` (954 passed, 26 skipped, 0 failures)
- Locked files: 251 files (entire `tests/` directory plus benchmark script)
- Host: Linux `x86_64` (AMD Ryzen 7 9800X3D)

## Head-to-head results

| Workload | Baseline | After `oa` | Gain | Notes |
| --- | ---: | ---: | ---: | --- |
| Pure `Console.print(table)` loop (10x 400-row tables) | 1,047.83 ms | 283.63 ms | **3.69x** | Excludes interpreter spawn and `import rich` |
| Full CLI (400-row styled `Table`, cold Python start) | 131.11 ms | 52.63 ms | **2.49x** | 20 interleaved pairs, 95% CI `[2.48x, 2.51x]`, `p = 0.0000`, `-20.3%` RSS |

## What changed (experiment ledger)

| # | Status | Hypothesis | Files | Step gain | 95% CI | p-value | Median ms |
| ---: | --- | --- | --- | ---: | --- | ---: | ---: |
| 1 | KEPT | Short-circuit `_emoji_replace` when `':'` absent, ASCII fast path in `cell_len`, 1-line fast path in `Text.__rich_console__` | `rich/_emoji_replace.py`, `rich/cells.py`, `rich/text.py` | 1.22x | [1.22x, 1.23x] | 0.0000 | 106.92 |
| 2 | KEPT | Per-render `str_cache` in `Table`, precomputed column `ConsoleOptions`, single-pass `Padding` unwrap, skip no-op `set_shape` | `rich/table.py` | 1.37x | [1.36x, 1.39x] | 0.0000 | 79.88 |
| 3 | KEPT | Single-build column cells, `Text` measurement cache, direct plain-cell segment fast path, and rendered-line cache in `Table._render` | `rich/table.py` | 1.48x | [1.47x, 1.49x] | 0.0000 | 53.10 |

During experiment #3, merging left/right cell padding into a single `Segment` caused `tests/test_console.py::test_log` to fail because Rich emits separate ANSI reset boundaries between cell content and padding spaces. `oa evaluate` caught the failure (`TestsFail`) before benchmarking; keeping the cached padding `Segment`s separate restored exact byte-for-byte ANSI parity across all 954 tests.

## Profile diff (`oa profile --diff-from baseline`)

```text
Diff vs baseline: 895.28 ms -> 469.13 ms (-47.6% profiled CPU)
    -54.20 ms ( -76.2%)  render (rich/markup.py)
    -36.94 ms (-100.0%)  <built-in method __new__>
    -20.89 ms (-100.0%)  _emoji_replace (rich/_emoji_replace.py)
    -20.43 ms (-100.0%)  _parse (rich/markup.py)
```

## Reproduce

```bash
git clone https://github.com/Textualize/rich.git
cd rich
git checkout 3a109ccdffe5b2d0e30684a304fbd388e99ce57b
uv venv && uv pip install pytest attrs typing_extensions markdown-it-py pygments
cat << 'EOF' > bench_oa.py
import hashlib, io
from rich.console import Console
from rich.table import Table

table = Table(title="[bold cyan]Service Latency Report[/bold cyan]", show_lines=False)
for name, j, st in [
    ("ID", "right", "dim"), ("Service", "left", "bold magenta"),
    ("Status", "center", None), ("Region", "left", "cyan"),
    ("Latency", "right", "green"), ("Detail", "left", None),
]:
    table.add_column(name, justify=j, style=st)

services = ("auth-api", "billing-worker", "catalog-edge", "checkout-svc", "search-index", "storage-node")
statuses = ("[bold green]OK[/bold green]", "[bold green]OK[/bold green]", "[yellow]WARN[/yellow]", "[bold red]ERR[/bold red]")
regions = ("eu-west-1", "us-east-1", "ap-south-1")

for i in range(400):
    table.add_row(
        str(i), services[i % 6], statuses[(i * 3) % 4], regions[i % 3],
        f"{12.4 + (i * 7 % 250) * 0.31:.2f} ms",
        f"[dim]req-{i:04d}[/dim] handled [bold]{(i * 13) % 900 + 10}[/bold] items (retry={i % 3})",
    )

buf = io.StringIO()
Console(file=buf, width=120, force_terminal=True, color_system="truecolor").print(table)
rendered = buf.getvalue()
print(f"chars={len(rendered)} sha256={hashlib.sha256(rendered.encode('utf-8')).hexdigest()[:16]}")
EOF
git add bench_oa.py && git commit -m "add bench"
oa init --bench ".venv/bin/python bench_oa.py" --test ".venv/bin/python -m pytest -q --ignore=tests/test_card.py" --lock bench_oa.py --pairs 20
git apply /path/to/oa/docs/showcases/rich.patch
oa evaluate --against baseline --pairs 20
```
