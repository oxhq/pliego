# Controlled execution ledger

This U4 slice adds an opt-in, session-scoped execution ledger to Servo's controlled document-clock
domain. Realtime WebViews and controlled clocks configured without limits retain the existing path:
they do not construct, update, or expose a DocumentExecutionLedger.

The ledger enforces only work with a truthful engine-owned boundary:

- an ordinary controlled task is counted before its queued event is removed or processed. Servo's
  TimerFired, TaskQueue Inactive/WakeUp, and ignored host-animation pump markers are not charged;
  the retained or actual timer task is counted when it reaches the queue;
- each microtask is counted inside MicrotaskQueue before that individual job runs, so a job that
  continually requeues itself reaches the microtask limit within the same DriveOneTurn. The main
  SpiderMonkey queue and every nested debugger interrupt queue share the exact policy slot;
- an update-the-rendering invocation is counted before it samples the frame time or runs callbacks,
  style, layout, or paint; and
- each invocation of Servo's central DOM mutation-record hook is counted without becoming an
  admission decision. Crossing this limit is terminal, but the engine neither suppresses nor claims
  to roll back the DOM algorithm.

The first limit breach is sticky. Counters freeze at that boundary, later controlled work is not
admitted, and the next authoritative control response carries the limits, counters, and typed
terminal reason. A DriveOneTurn that observes the boundary remains a completed observation; it does
not turn committed work into a control-plane rejection. A guarded AdvanceTo presented after a
terminal is definitively rejected while the ledger is locked and before clock mutation. Missing
DriveOneTurn or AdvanceTo responses retain their existing indeterminate transport semantics.

The optional virtual-span limit is measured from the controlled clock's initial monotonic offset.
ScriptThread first rejects a stale timer snapshot without changing policy state, then holds the
ledger guard across a second exact scheduler validation and clock mutation. A before-initial or
over-span request latches one typed terminal without moving the clock. Raw clock and scheduler APIs
remain mechanisms; this ScriptThread conditional-advance path is the current enforcement authority.

Controlled monotonic offsets use unsigned 128-bit nanoseconds and wall time uses signed 128-bit
nanoseconds, covering the API 2 epoch and span arithmetic without a u64 narrowing. Performance
conversion casts the exact integer-millisecond part before fractional precision. JavaScript Date's
f64-microsecond SpiderMonkey hook is fail-closed: an in-TimeClip millisecond that no adjacent f64
candidate can preserve latches a typed clock terminal and returns NaN; exact values outside
TimeClip return the specification's NaN without being misrounded back to a finite boundary. Neither
path consults host wall time.

When the ledger is enabled, producer empty-checkpoint qualification is also bound to the exact
execution observation. Any task, microtask, rendering, mutation-record, resource-evidence, or
terminal counter change discards the previous empty candidate even if no producer ticket changed;
two new unchanged checkpoints are required. A clock without a ledger retains the U1-U3 producer
observer behavior.

The ScriptThread-owned producer fence also records a clearly named owned_resource_events total. It
is retained evidence, not an advertised API 2 budget: the profile-null contract deliberately omits
a post-readiness resource limit until the runtime has a truthful readiness-phase transition from
which to start that counter.

## Remaining seams

This slice does not claim:

- CPU or host-wall hard interruption of JavaScript already running inside one task or microtask;
- a post-readiness resource budget, until readiness is a typed phase transition;
- a universal post-DOM-write generation (CharacterData deliberately queues its record before the
  write), or complete style, graphics beyond the retained Canvas 2D subset, image, font, or worker
  mutation generations;
- classification of infinite timers, RAF loops, declarative animations, workers, or resources; or
- universal visual settlement for the unsupported source classes below, or stable API 2 terminal
  artifact integration.

Those are separate fail-closed gates. An opt-in internal Pliego session now maps the normalized API
2 epoch, virtual span, task, microtask, rendering, and mutation limits into a controlled clock before
navigation. The default production `render` route and its explicit `render-controlled` alias drive
that coordinator, obtain an opaque candidate, reserve the exact Paint presentation without
readback, consume the candidate after two ScriptThread revalidations around layout serialization,
and revalidate Paint again before pixel readback. A ledger or capture terminal is routed through
the existing fail-closed publication transaction, so it cannot expose a PDF, scene bundle, or
requested output. Neither route has a realtime fallback. The controlled transaction accepts only
Canvas 2D transcripts whose retained image keys and registry generation survive Script consume,
Paint finalization, layout serialization, and atomic freeze.

This still does not enforce CPU or host-wall interruption inside one already-running JavaScript
turn, or the post-readiness resource budget described above. The widened serde/IPC shapes are
verified for one same-build runtime; they are not a backward-wire-compatibility claim for older
binaries, and this route alone is not the stable API 2 artifact contract.
