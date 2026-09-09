# Aureus manufacturing work-order correctness fixture

This is one genuine **one-page work order**, not a long operational report or
independent adoption. It freezes the original application's capture008 after a
synthetic five-unit manufacturing workflow. The original action, rendered HTML,
expected facts, source hashes and licenses are in `fixtures/manufacturing`.
No application bootstrap, database, vendor directory or credentials are needed
to inspect this fixture or run its PDF oracle.

## Provenance and input

Source: [Aureus ERP at d7e0ad85](https://github.com/aureuserp/aureuserp/tree/d7e0ad85ae8fdea91d1cb1895a81000c332e830c).
`PrintMOAction` calls the original `print-mo.blade.php` through
Laravel-dompdf and downloads an A4 portrait PDF. The source action and template
are included under `source/` and covered by `licenses/Aureus-MIT.txt`.
The HTML Code128 bars came from milon/barcode v13.1; its LGPL license is retained.
The original dompdf UA stylesheet and LGPL license are retained because the
unchanged template inherits its 1.2 cm page margin.

The shared input adds **only two `@font-face` declarations** referencing the
original DejaVu Sans regular/bold files. Removing that exact CSS restores the
original captured HTML byte-for-byte. Both providers must receive these same
font bytes, whose licenses are included. All original declarations, quantities,
barcode elements (including zero-width bars) and capture008 component order are
unchanged. The upstream component relation has no `ORDER BY`; this frozen-input
identity is not a promise that fresh application runs regenerate identical HTML.

The legacy renderer is dompdf 3.1.6 through Laravel-dompdf 3.1.2, DPI 96, with
remote resources disabled and original font-subsetting settings. The manifest
records exact source references and the shared repaired Composer-lock identity.
The adapter must preserve original renderer options; the oracle does not prove
those options were used. Pliego's request mapping is `47622x67351au`, margins
`2721,2721,2721,2721au`: millimeters converted to 60 Au/CSS px and rounded once.
This does not add an `@page` rule or round the request through whole CSS pixels.

## Run outside measurement

Use a dedicated Python environment; do not change global dependencies. The
requirements file pins binary wheels for CPython 3.12/3.13 on Linux x86_64 and
Windows amd64. Sources: [ZXing 2.3.0 metadata](https://pypi.org/pypi/zxing-cpp/2.3.0/json)
and [Pillow 11.3.0 metadata](https://pypi.org/pypi/pillow/11.3.0/json).

```sh
python -m pip install --require-hashes -r manufacturing_requirements.txt
python manufacturing_test.py
python manufacturing_font_test.py
python manufacturing_oracle.py --pdf /absolute/output.pdf \
  --provider dompdf --poppler-dir /absolute/poppler/bin --output /absolute/fresh-oracle-output
python manufacturing_oracle.py --provider pliego --pdf /absolute/bundle/document.pdf \
  --scene /absolute/bundle/scene.json --bundle /absolute/bundle/bundle.json \
  --poppler-dir /absolute/poppler/bin --output /absolute/fresh-candidate-oracle-output
```

The Poppler directory must explicitly contain `pdftotext`, `pdftoppm` and
`pdffonts` (or their `.exe` counterparts). Their hashes, version-command output,
arguments and 30-second deadlines are retained. Pin their package and hashes in
the campaign environment; accepting an explicit tool directory is not a Linux
qualification claim. `--corpus` relocates the complete fixture;
`--assets` optionally verifies a separately staged directory containing the two
exact TTF files. Paths with spaces work as ordinary quoted arguments.
The default provider remains `dompdf`; `pliego` requires both `--scene` and
`--bundle`, while legacy invocations reject those arguments. Font verification
uses the shared `ledger_fonts.py` helper and the campaign's pinned pypdf 6.16.2
and fonttools 4.60.0 in addition to the barcode requirements; no dependency is
installed by the oracle. Each fixture independently pins its two original faces.

Each PDF must have one A4 page, complete in-bounds text, the correct five-unit
production row, component quantities 10/5/2.50, two ordered operation rows with
12.5/18.0 minute durations, unchanged `1 / 1` header/footer, two embedded expected
font faces, and all three independently decoded, checksum-valid Code128 values.
The five original observation-corruption checks run for every PDF. Additional
unit corruptions cover font substitution, clipping, duplication, component order,
checksum/format and source-resource changes. `reference-observation.json` is an
untimed unit-test reference from the reviewed legacy PDF; live PDF validation
always extracts and decodes the supplied PDF instead.

The dompdf path now requires the exact original unsubsetted font bytes embedded
in the PDF. The Pliego path verifies every referenced sanitized scene font against
the original outlines, metrics, Unicode mapping and style; the actual PDF's
subset programs and CID/Unicode mappings must agree with that hash-bound scene.
Changing font programs or rebinding a consistently rehashed wrong scene fails.
Synthetic font units test this boundary, not actual native document support.

`report.json` retains `pdfSha256`, `fontProof`, and `layoutFingerprint`. The latter
hashes extracted page/text coordinates, normalized face names and decoded barcode
positions, excluding paths, timings, PDF metadata/object IDs and subset prefixes.
It is an exact same-environment recurrence gate, not a cross-provider equality
requirement. A timed sample must match an explicitly visually reviewed preflight
variant. Actual raster review, storage/readback, performance and release gates
remain separate; `benchmarkQualified` stays false in this oracle.
