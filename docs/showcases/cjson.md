# Showcase: DaveGamble/cJSON (1.89x faster parse and print in `-O3` C)

- Target: [DaveGamble/cJSON](https://github.com/DaveGamble/cJSON) (`6d9f2443ab`, 11.5k+ stars)
- Profiler backend: `linux:perf` (rootless `:u` hardware counters + native symbol sampling)
- Compiler flags: `gcc 16.2 -O3 -g -fno-omit-frame-pointer -std=c89 -pedantic -Wall -Wextra -Werror`
- Host: `linux x86_64`, AMD Ryzen 7 9800X3D
- Verification: 19/19 upstream CTest suites pass (`cmake -DENABLE_CJSON_TEST=On`), 180 locked files intact, byte-identical output SHA-256 (`90d2a2cb3adaf081`), 1 file changed (`cJSON.c`)
- Apply-ready patch: [`cjson.patch`](cjson.patch)

## Verified results (`oa verify --pairs 20`)

Workload: 80 iterations of `cJSON_ParseWithLength` + `cJSON_PrintUnformatted` plus 20 iterations of formatted `cJSON_Print` on a deterministic 1,200-record structured JSON document containing nested objects, arrays, integers, decimals, booleans, and escaped strings (`\n`, `\"`, `\\`).

| Metric | Baseline (`cJSON.c`) | After `oa` | Delta |
| --- | ---: | ---: | ---: |
| Interleaved median wall time (`n = 20` pairs) | 104.25 ms | 55.64 ms | **1.89x** (`95% CI [1.85x, 1.94x]`, `p = 0.0000`) |
| p95 / MAD | 132.36 ms / 2.99 ms | 63.25 ms / 1.79 ms | -52.2% p95 |
| Peak RSS | 14,244 KiB | 14,040 KiB | -1.4% |
| User instructions (`perf stat`) | 2,216,295,394 | 1,336,845,902 | **-39.7%** (-879M instructions) |
| User cycles (`perf stat`) | 489,043,212 | 267,442,161 | **-45.3%** (1.83x fewer cycles) |
| Instructions per cycle (IPC) | 4.53 | 5.00 | +10.4% |
| Branch misses (`perf stat`) | 829,931 | 367,523 | **-55.7%** |
| Upstream CTest suites | 19/19 pass | 19/19 pass | 0 regressions |

## What `linux:perf` found

The initial `oa profile` run on `-O3` compiled C surfaced four clear bottlenecks:

1. `parse_number` called `malloc`, `memcpy`, `strtod`, and `free` on every single number in the input, even for plain integers and short decimals.
2. `print_number` called locale-aware `sprintf("%d")` on every integer and `sprintf("%1.15g")` followed by `sscanf("%lg")` on every float.
3. `update_offset` ran `strlen` over the just-written buffer after every string, value, array, and object (accounting for 19.0% of total CPU time inside `libc.so.6` alone).
4. `parse_value` ran three `strncmp` calls (`"null"`, `"false"`, `"true"`) before checking the first byte, `parse_string` copied unescaped strings byte-by-byte, and `print_string_ptr` scanned every key and value twice.

## Experiment ledger

| # | Status | Hypothesis | File | Paired gain | 95% CI | p-value |
| ---: | --- | --- | --- | ---: | --- | ---: |
| 1 | KEPT | Zero-malloc integer/decimal fast paths in `parse_number` (64-byte stack fallback for `strtod`) and fast integer/`strtod` formatting in `print_number` | `cJSON.c` | 1.22x | [1.17x, 1.27x] | 0.0000 |
| 2 | KEPT | Track `output_buffer->offset` directly to remove all `update_offset`/`strlen` calls, 256-byte escape LUT in `print_string_ptr`, `memcpy` fast path in `parse_string`, `switch` dispatch in `parse_value`, inline `ensure_fast` macro | `cJSON.c` | 1.26x | [1.22x, 1.28x] | 0.0000 |
| 3 | KEPT | Exact IEEE-754 round-trip fixed-point decimal fast path in `print_number` (`[0.0001, 1e6)`, <=6 fractional digits) and single-pass speculative copy in `print_string_ptr` | `cJSON.c` | 1.25x | [1.15x, 1.31x] | 0.0000 |

Direct 20-pair `oa verify` of final HEAD against `refs/oa/baseline`: `VERIFIED: 1.89x wall (95% CI [1.85x, 1.94x], p=0.0000), -1.4% peak RSS, SHA-256 exact, 180 locked file(s) intact`.

The decimal fast path in Experiment #3 checks exact IEEE-754 equality (`recovered == d`) before bypassing `sprintf("%1.15g")`, so values like `0.1 + 0.2` fall back cleanly to `sprintf` and preserve byte-for-byte output across all CTest cases.

## Reproduce

```bash
git clone https://github.com/DaveGamble/cJSON.git
cd cJSON
git checkout 6d9f2443ab071f86e5d9b43025a40929ec41c46c
git apply /path/to/oa/docs/showcases/cjson.patch
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DENABLE_CJSON_TEST=On
cmake --build build -j4
ctest --test-dir build --output-on-failure
```
