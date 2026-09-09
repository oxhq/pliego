# Application-derived comparison adapters

These are candidate benchmark tools, not published comparison results. They load
the actual reviewed application dependency trees without booting an application
kernel, `.env`, database, queue, or HTTP server. Frozen Blade generation and real
SDK/storage workflows are separate evidence; this measures cold renderer-process
work on the same frozen HTML and assets.

| Adapter | Incumbent and input | Preserved settings |
| --- | --- | --- |
| `invobook-browsershot/adapter.php` | Invobook's Browsershot5.0.5; repaired simple invoice | Original A4 portrait, zero margins, background printing, two supplied DejaVu faces. Harness Puppeteer25.8.0 is separately identified, not an upstream app pin. |
| `aureus-dompdf/adapter.php` | Aureus's dompdf3.1.6 and Laravel-dompdf3.1.2; 300-row ledger and capture008 manufacturing work order | Exact original vendor options and A4 orientation. Only temp/font-cache/chroot locations change to owned sample paths. Authored page/footer rules and original manufacturing UA1.2cm margin remain unchanged. |

Both reject unknown input HTML hashes, wrong request geometry, changed application
locks/config, and changed supplied font bytes. PDF correctness must still be
checked independently; an embedded font name or a list of browser failures is not
complete font/resource proof. Browsershot5.0.5's failed-request list reports HTTP
response errors, not all failed local loads. The frozen byte closure, PDF oracle,
visual review, and no-network measurement namespace are separate gates.

Create an **untracked**, read-only `runtime.json` beside each adapter. The exact
configuration bytes/path, Composer lock and complete installed dependency tree
are retained by `identity`; prepare and seal that runtime closure outside timing.
No dependency installation occurs in an adapter invocation.

```json
{
  "schema": "pliego.real-document-runtime.v1",
  "app_root": "/absolute/pinned-application",
  "node_path": "/usr/lib/pliego-benchmark/node",
  "chrome_path": "/usr/lib/pliego-benchmark/chrome/chrome",
  "node_modules": "/absolute/pinned-harness/node_modules"
}
```

Only the invoice adapter requires the Node/Chrome/module fields. The checked
application locks are Invobook `24f563534a57775144db27b746011117b8db209aa39dee35f5c0ba01ed96ca74`
and Aureus `82133507ad710cc2748d95cba0ea3dfe5d375728c0d8d3587303e91d027c5fae`.
The latter includes the disclosed shared three-dependency security repair; its
renderer versions were not changed. The original Dompdf vendor configuration is
`4d22df6fb0728d4a4020c8fe435263cb2e2bb81c7a675ea4f30e2a78537df10b`.

The adapters accept the existing `identity` / `render INPUT --output PDF
--artifacts DIR --page-size TUPLEau --page-margins TUPLEau` transport. Render is
Linux-only under the existing cgroup/private-runtime/durability recipe. Local
Windows identity/helper tests do not qualify Linux execution. On Windows, Chrome's
CLI version is deliberately unavailable: `chrome.exe --version` can open the
user's current browser rather than return metadata.

The old generic adapters expose only an opt-in library return so these tools reuse
their tested publication and private-runtime cleanup helpers. Their default CLI,
dependency versions and configuration are unchanged. Each new identity binds the
exact helper hashes; no copied generic renderer is called under an app label.
The old minimal-static publication verifier and retained evidence are unchanged.

For Browsershot5.0.5, shared-memory `TMPDIR` is scoped across `getenv()`, `$_ENV`
and `$_SERVER` during the child invocation, then restored even on failure.
Otherwise Symfony's startup environment can override `putenv()`. The original
private HOME/XDG/profile and flushed teardown contract is retained. Node/Chrome
descendant cleanup on failure belongs to the root-owned sampler, not a promise
made merely because `savePdf()` threw.

Renderer evidence is created exclusively and checked for a complete write before
PDF publication. Artifact/cache flushing and the declared PDF publication work
remain inside the measured adapter. Post-measurement evidence copying and PDF
oracles are outside it. These durations are not Laravel request or storage latency.

Contributor guards:

```sh
php benchmarks/tools/test_real_document_adapters.php invobook-browsershot
php benchmarks/tools/test_real_document_adapters.php invobook-browsershot --actual-symfony
php benchmarks/tools/test_real_document_adapters.php aureus-dompdf
php benchmarks/adapters/browsershot/adapter.php self-test
php benchmarks/adapters/dompdf/adapter.php self-test
python benchmarks/tools/test_benchmark_runtime.py
```

The actual-Symfony option requires the configured reviewed Invobook vendor and
checks the environment received by a real short-lived child process. It does not
launch a browser. Before timing, run both app adapters in the hosted recipe,
qualify their PDFs against the portable family oracles, verify the exact candidate
Pliego output, and complete the full raw-evidence campaign. There are no speed or
reliability ratios in this document.
