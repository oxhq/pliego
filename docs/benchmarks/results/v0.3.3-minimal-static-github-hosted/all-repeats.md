# Pliego v0.3.3 hosted benchmark series

> Evidence class: `github-hosted-exploratory`; publication status: `hosted-series`.
> All three repeats are retained. No best or canonical repeat is selected.

- Run: [33243607869 attempt 1](https://github.com/oxhq/pliego/actions/runs/33243607869/attempts/1)
- Revision: `48e8192992d1e9b66eed8ded164ba67cc25a3e7c`
- Runner image: `ubuntu24` `20260823.283.1`
- Fixture: `minimal-static`
- Repeats: 3 fresh GitHub-hosted jobs; 100 timed samples per renderer per repeat
- Per repeat: 1 correctness preflight, 10 discarded warmups, seed 1
- Correctness: all 100 timed samples per renderer passed the shared PDF oracle in every repeat

## Retained source artifacts

| Repeat | Comparison SHA-256 | Interleaved artifact SHA-256 | Schedule SHA-256 |
| ---: | --- | --- | --- |
| 1 | `00aef39189f127b0db2696f4b6eb3c4bde2db140789181b242132c031428ed84` | `9f311eb9dc313cb20097a7be9a80f29d7059ef2c493c8eb28391f39ac5763846` | `168ff06590e9f3a6cd9346c223a4b5b9151c081f7a0925492d368cb46b45dee3` |
| 2 | `27c73bacd62c080a4f3134a9b1ed50d03c7f8cd0f50ada5d23d6ea78ef59878f` | `15e86ad2aa2d35460ca373abb8f0907be20f4929a9f5fa17a7789ce891534acb` | `168ff06590e9f3a6cd9346c223a4b5b9151c081f7a0925492d368cb46b45dee3` |
| 3 | `126131143f1c0ab0f99da86cb11a7ab7820e9e6f9f9afc4c9a14abb39b74caf0` | `543ac0a0edd774e432874645ccfb0107a2d70888781a9c7d5224591d93ad5e30` | `168ff06590e9f3a6cd9346c223a4b5b9151c081f7a0925492d368cb46b45dee3` |

## Per-repeat hosted environments

| Repeat | Runner name | CPU model | Cores | RAM bytes | Kernel | Virtualization |
| ---: | --- | --- | ---: | ---: | --- | --- |
| 1 | `GitHub Actions 1000007835` | `AMD EPYC 7763 64-Core Processor` | 4 | 16770748416 | `6.17.0-1022-azure` | `microsoft` |
| 2 | `GitHub Actions 1000007836` | `AMD EPYC 7763 64-Core Processor` | 4 | 16766414848 | `6.17.0-1022-azure` | `microsoft` |
| 3 | `GitHub Actions 1000007837` | `AMD EPYC 9V74 80-Core Processor` | 4 | 16766410752 | `6.17.0-1022-azure` | `microsoft` |

## Per-run p50s and between-run spread

For `read_bytes` and `write_bytes`, values come from cgroup `io.stat`. Memory-backed stdout/stderr capture is excluded for every target. Browsershot's private tmpfs Node/Chrome `TMPDIR` is also excluded from block I/O but remains charged to cgroup memory; its PHP `TMPDIR`, HOME/XDG roots, explicit Chromium profile, artifacts, and PDF stay on the measured ext4 storage.
Cgroup `memory.peak` covers every timed sample and sampled RSS is retained as an aggregated lower bound. Cadence-dependent PSS observations remain in each raw repeat artifact only and are excluded from comparative aggregates because short-lived processes may exit before the first PSS sample.

| Renderer | Metric | Unit | Repeat 1 p50 | Repeat 2 p50 | Repeat 3 p50 | Min | Max | Mean | Relative spread |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `artifact_bytes` | bytes | 0 | 0 | 0 | 0 | 0 | 0 | 0% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `cpu_system_ms` | ms | 539.48 | 565.974 | 450.495 | 450.495 | 565.974 | 518.649667 | 22.265318% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `cpu_total_ms` | ms | 1316.349 | 1380.981 | 1066.083 | 1066.083 | 1380.981 | 1254.471 | 25.102055% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `cpu_user_ms` | ms | 778.783 | 812.988 | 612.575 | 612.575 | 812.988 | 734.782 | 27.275165% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `memory_current_bytes` | bytes | 548864 | 532480 | 528384 | 528384 | 548864 | 536576 | 3.816794% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `memory_peak_bytes` | bytes | 223703040 | 201170944 | 224124928 | 201170944 | 224124928 | 216332970.666667 | 10.610488% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `one_shot_wall_ms` | ms | 1338.267 | 1178.897 | 1079.396 | 1079.396 | 1338.267 | 1198.853333 | 21.593217% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `pdf_bytes` | bytes | 4364 | 4364 | 4364 | 4364 | 4364 | 4364 | 0% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `read_bytes` | bytes | 24133632 | 0 | 24051712 | 0 | 24133632 | 16061781.333333 | 150.255015% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `read_operations` | operations | 100 | 0 | 171 | 0 | 171 | 90.333333 | 189.298893% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `sampled_peak_rss_kib_lower_bound` | KiB | 1063952 | 1046716 | 1074512 | 1046716 | 1074512 | 1061726.666667 | 2.618% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `wall_ms` | ms | 1117.57 | 947.887 | 879.863 | 879.863 | 1117.57 | 981.773333 | 24.212004% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `write_bytes` | bytes | 5193728 | 5193728 | 5193728 | 5193728 | 5193728 | 5193728 | 0% |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | `write_operations` | operations | 1224 | 1224 | 1804 | 1224 | 1804 | 1417.333333 | 40.921919% |
| dompdf 3.1.6 (cold Composer adapter) | `artifact_bytes` | bytes | 49101 | 49101 | 49101 | 49101 | 49101 | 49101 | 0% |
| dompdf 3.1.6 (cold Composer adapter) | `cpu_system_ms` | ms | 37.416 | 40.034 | 37.868 | 37.416 | 40.034 | 38.439333 | 6.810732% |
| dompdf 3.1.6 (cold Composer adapter) | `cpu_total_ms` | ms | 137.709 | 144.94 | 104.182 | 104.182 | 144.94 | 128.943667 | 31.609152% |
| dompdf 3.1.6 (cold Composer adapter) | `cpu_user_ms` | ms | 100.122 | 105.28 | 66.75 | 66.75 | 105.28 | 90.717333 | 42.472589% |
| dompdf 3.1.6 (cold Composer adapter) | `memory_current_bytes` | bytes | 454656 | 495616 | 471040 | 454656 | 495616 | 473770.666667 | 8.645533% |
| dompdf 3.1.6 (cold Composer adapter) | `memory_peak_bytes` | bytes | 22142976 | 22159360 | 22183936 | 22142976 | 22183936 | 22162090.666667 | 0.18482% |
| dompdf 3.1.6 (cold Composer adapter) | `one_shot_wall_ms` | ms | 725.169 | 526.284 | 487.344 | 487.344 | 725.169 | 579.599 | 41.032679% |
| dompdf 3.1.6 (cold Composer adapter) | `pdf_bytes` | bytes | 3628 | 3628 | 3628 | 3628 | 3628 | 3628 | 0% |
| dompdf 3.1.6 (cold Composer adapter) | `read_bytes` | bytes | 0 | 0 | 0 | 0 | 0 | 0 | 0% |
| dompdf 3.1.6 (cold Composer adapter) | `read_operations` | operations | 0 | 0 | 0 | 0 | 0 | 0 | 0% |
| dompdf 3.1.6 (cold Composer adapter) | `sampled_peak_rss_kib_lower_bound` | KiB | 59036 | 59028 | 59148 | 59028 | 59148 | 59070.666667 | 0.203147% |
| dompdf 3.1.6 (cold Composer adapter) | `wall_ms` | ms | 531.282 | 330.605 | 313.688 | 313.688 | 531.282 | 391.858333 | 55.528741% |
| dompdf 3.1.6 (cold Composer adapter) | `write_bytes` | bytes | 3104768 | 3104768 | 3104768 | 3104768 | 3104768 | 3104768 | 0% |
| dompdf 3.1.6 (cold Composer adapter) | `write_operations` | operations | 745 | 745 | 799 | 745 | 799 | 763 | 7.077326% |
| Pliego 0.3.3 (published API 2 bundle) | `artifact_bytes` | bytes | 41569 | 41569 | 41569 | 41569 | 41569 | 41569 | 0% |
| Pliego 0.3.3 (published API 2 bundle) | `cpu_system_ms` | ms | 109.661 | 115.797 | 98.225 | 98.225 | 115.797 | 107.894333 | 16.286305% |
| Pliego 0.3.3 (published API 2 bundle) | `cpu_total_ms` | ms | 624 | 636.37 | 492.491 | 492.491 | 636.37 | 584.287 | 24.624714% |
| Pliego 0.3.3 (published API 2 bundle) | `cpu_user_ms` | ms | 513.347 | 519.304 | 393.62 | 393.62 | 519.304 | 475.423667 | 26.43621% |
| Pliego 0.3.3 (published API 2 bundle) | `memory_current_bytes` | bytes | 696320 | 684032 | 655360 | 655360 | 696320 | 678570.666667 | 6.036217% |
| Pliego 0.3.3 (published API 2 bundle) | `memory_peak_bytes` | bytes | 178094080 | 178135040 | 178429952 | 178094080 | 178429952 | 178219690.666667 | 0.18846% |
| Pliego 0.3.3 (published API 2 bundle) | `one_shot_wall_ms` | ms | 891.861 | 897.63 | 743.124 | 743.124 | 897.63 | 844.205 | 18.301953% |
| Pliego 0.3.3 (published API 2 bundle) | `pdf_bytes` | bytes | 3925 | 3925 | 3925 | 3925 | 3925 | 3925 | 0% |
| Pliego 0.3.3 (published API 2 bundle) | `read_bytes` | bytes | 0 | 0 | 0 | 0 | 0 | 0 | 0% |
| Pliego 0.3.3 (published API 2 bundle) | `read_operations` | operations | 0 | 0 | 0 | 0 | 0 | 0 | 0% |
| Pliego 0.3.3 (published API 2 bundle) | `sampled_peak_rss_kib_lower_bound` | KiB | 300808 | 302584 | 301616 | 300808 | 302584 | 301669.333333 | 0.588724% |
| Pliego 0.3.3 (published API 2 bundle) | `wall_ms` | ms | 558.748 | 558.546 | 447.58 | 447.58 | 558.748 | 521.624667 | 21.311876% |
| Pliego 0.3.3 (published API 2 bundle) | `write_bytes` | bytes | 786432 | 786432 | 786432 | 786432 | 786432 | 786432 | 0% |
| Pliego 0.3.3 (published API 2 bundle) | `write_operations` | operations | 183 | 183 | 284 | 183 | 284 | 216.666667 | 46.615385% |

## Serial throughput by repeat

| Renderer | Repeat 1 renders/min | Repeat 2 renders/min | Repeat 3 renders/min | Min | Max | Mean | Relative spread |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Browsershot 5.4.0 + Puppeteer 25.8.0 (cold Chromium adapter) | 45.01637 | 45.202762 | 49.111798 | 45.01637 | 49.111798 | 46.443643 | 8.81806% |
| dompdf 3.1.6 (cold Composer adapter) | 82.582476 | 113.854589 | 101.497472 | 82.582476 | 113.854589 | 99.311512 | 31.48891% |
| Pliego 0.3.3 (published API 2 bundle) | 67.185775 | 66.809523 | 77.775913 | 66.809523 | 77.775913 | 70.590404 | 15.535241% |

## Claim boundary

Three correctness-gated repeats on GitHub-hosted VMs; repeat-to-repeat spread is retained, no best or canonical repeat is selected, and the series is not dedicated-host evidence or a general production-performance ranking.

Relative spread is `(max - min) / mean * 100` across the three per-run p50s, or across the
three serial-throughput values. The three source comparisons and raw interleaved artifacts are the retained
verification sources for this summary.
