# API 2 correctness-qualification budgets

The links, tables, fixed-content, nonpainting-content, and table-background
correctness suites use the existing API 2 request default: **60,000 ms engine
host-wall budget**, with a **65-second outer process bound** for optimized
packages. The direct-debug workflow retains its explicit 180-second outer
allowance. The engine budget is not reset after graphics initialization.

This is an explicit qualification-policy decision based on the paired macOS
Intel diagnostics below, not a native runtime fix, an automatic retry, or a
release waiver. A fresh complete platform matrix must pass before promotion.
The scene/PDF geometry, text, resource-closure, typed-failure, and artifact
oracles are unchanged. Dedicated short caller-deadline tests are unchanged.

## Evidence behind the decision

The earlier package-order census (run `33964456723`, artifact `9969060697`)
retained six timeouts among 108 completed attempts. Each occurred immediately
after an unsupported gradient fixture, before any controlled command. Its ZIP
SHA256 is `7a1d83ca38de4802ce14edab35d0427dec3eff45f428228e457dcaa6d1401771`.

Two follow-up jobs used the same native binary, checker, 18 fixed inputs, and
three predetermined rotated pair orders. Each pair consisted of a predecessor
followed by the same no-href probe. The requests differed only in host_wall_ms.

| Predecessor | Probe with 10,000 ms engine / 30 s outer | Probe with 60,000 ms engine / 65 s outer |
| --- | --- | --- |
| HTTPS control | 3/3 success | 3/3 success |
| Original link-gradient fixture | 3/3 timeout | 3/3 success |
| Original table row-image-layer gradient | 3/3 timeout | 3/3 success |

In each job, all three HTTPS predecessors succeeded and all six gradient
predecessors retained `artifact/SCENE_ENCODING_FAILED`. Thus the primary job
had 6 successes, 6 expected artifact failures, and 6 settlement timeouts; the
counterfactual had 12 successes and 6 expected artifact failures. All 36
terminal results, input/artifact closures, and diagnostic records revalidated.
Neither job had an outer timeout or incomplete census. Workflow success means
valid diagnostic records, not that every render was accepted.

- Native development source: `c9ff7594271c6b2173193e6e59d1e7713bd254d5`;
  binary SHA256 `560379c2d2fb44039a04a08bd7b27dcb91a83b816f98f6436086269b4b8602e0`.
- Checker proof source: `527aa639829cb9d45b3171639a48d3f41d73e550`.
- Primary: [run 33965643297](https://github.com/oxhq/pliego/actions/runs/33965643297),
  artifact `9969403913`, ZIP SHA256
  `a082a9e212272dd106945036595bf27613b0bfa95796bef44247d4d9a34ea463`.
- Counterfactual: [run 33965644671](https://github.com/oxhq/pliego/actions/runs/33965644671),
  artifact `9969414090`, ZIP SHA256
  `a044912dec64f80e3beadd79f78bdddf89aee7a01272b241ed9dec13d4e93b13`.

## What remains unresolved

All six primary probe timeouts recorded zero commands and responses, no last
command or observation, and `settlement-before-command`. The first startup
milestone, `render_context_ready`, already accounted for 14.282–23.656 seconds;
it precedes Servo/WebView construction. It spans software rendering-context
creation, making the context current, and page-reservation commit, so it does
not identify the exact slow graphics/CGL call or cleanup mechanism.

The successful post-gradient probes in the default-budget job still took
18.173–25.817 seconds of total process wall time. Successful renders emit no
private startup diagnostic, so those durations cannot be attributed to a
specific call. The jobs used the same macOS image but not a proven identical
physical host/load history. Shared renderer/driver cleanup remains a hypothesis;
reaping the direct child does not establish descendant or driver cleanup.

Pliego therefore makes **no ten-second completion promise** from these tests.
These observations are not benchmark samples, throughput evidence, or proof
that the slow startup disappeared. Further diagnosis remains possible without
conflating correctness qualification with a tighter performance target.

The diagnostic checker preserves its original matched-control (9), package-order
(108), and gradient-pair (18) sequences at 10,000 ms / 30 seconds, plus the
separately selected 60,000 ms / 65-second counterfactual. Regression tests bind
their exact request-sequence hashes from before this decision. No archived
failure, native policy, or caller-deadline evidence is rewritten.
