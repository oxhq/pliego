# chartjs-showcase

Managed reference — this fixture is **not** duplicated. The input is
`ports/pliego/tests/fixtures/chartjs-report/index.html` (the same fixture the
engine's `check_chartjs_report.py` uses): Chart.js 4.5.1, `ReportSans`
(DejaVuSans) font, a deterministic non-animated bar chart, a synchronous
full-canvas `getImageData` readback, and `window.pliego.ready()`.

## Prepare once (from the repository root)

```sh
cd ports/pliego/tests/fixtures/chartjs-report
npm ci                                   # installs chart.js 4.5.1 (pinned)
cp ../../../../../components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf ReportSans.ttf
```

`run_benchmark.py` refuses to run this fixture until
`node_modules/chart.js/dist/chart.umd.js` and `ReportSans.ttf` exist next to
the input. The Chart.js UMD artifact is content-pinned by SHA-256
(`ecc3cd1e…`) in the engine's own check; treat a hash change as a fixture
change and re-verify before comparing numbers across runs.

Expected (see `manifest.toml`): 1 page, contains "Northstar Operations" and
"Account contribution".
