# ADR 0020: Canonical semantic DocumentScene v1

- Status: Proposed
- Date: 2026-08-10

## Context

API 2 reserves one strict, versioned, content-addressed `DocumentScene.semantic_layer` resource, but
ADR 0018 deliberately does not define that resource. Paint order is not reading order, geometry is
not structure, and a PDF tag name is backend policy rather than a profile-neutral document model.
Adding roles to paint operations would make visual consumers interpret semantics accidentally and
would make one operation serve two incompatible orderings.

Pliego needs an immutable logical document that can be captured once, reviewed independently of the
PDF writer, and associated back to exact page operations without exposing Servo runtime handles.
Unknown roles, ambiguous ownership, unclassified decoration, and unstable identifiers must fail
before any backend can claim tagged or accessible output.

This ADR defines a contract slice only. The schema, representative golden, rejection corpus, and
dependency-free self-test live in [`contracts/document-semantics`](../../../contracts/document-semantics).
They do not change Servo capture, the Pliego runtime, API negotiation, bundle publication, the PDF
backend, or release behavior.

## Decision

### Independent semantic resource

The semantic document uses this exact envelope identity:

```json
{"schema":"pliego.document-semantics.canonical","version":1}
```

It carries the exact requested profile reference and the containing scene contract tuple, but it does
not contain the scene digest. The containing `DocumentScene` content-addresses the semantic resource,
so putting the containing scene digest inside that resource would create an impossible hash cycle.
The association is intentionally one-way:

```text
DocumentScene -> semantic resource -> stable page/operation locators
```

The schema is profile-neutral. The accepted golden binds the selected
`{"schema":"pliego.profile.pdfua-1","version":1}` reference, while roles and locators contain no PDF
object numbers, structure tags, MCIDs, ParentTree keys, writer settings, or validator policy.

### Canonical identity and bounds

The semantic digest is the SHA-256 content address of exact typed canonical JSON bytes: schema field
order, retained array order, compact separators, UTF-8 without a BOM, and one trailing line feed.
Duplicate object names, floats, non-finite values, lexical `-0`, unknown members, unknown variants,
and unsupported schema versions reject before typed decoding.

Every collection and string is bounded. Accessible text is at most 4,096 Unicode scalar values and
16,384 UTF-8 bytes, uses NFC, has no leading or trailing whitespace, and contains no control
characters. The contract has no generic metadata map. Adding a semantic field or role requires a new
schema version rather than an unbounded extension object.

### Explicit invariant policy and document metadata

The resource carries a closed semantic policy with three independent decisions:

- logical content is `required` or `allow-empty`;
- navigation is `required` or `explicit-none`; and
- paint coverage is always `complete` in version 1.

The selected PDF/UA-1 golden uses `required`, `required`, and `complete`. A required logical tree must
contain real non-root structure and at least one logically owned paint fragment. A required outline
must be nonempty. This blocks both an empty tag tree and an `Outline` value that exists but has no
items; backend object presence is not semantic proof.

`allow-empty` and `explicit-none` are typed future-policy hooks, not implicit defaults. `allow-empty`
is valid only for a root-only tree, and `explicit-none` is invalid when headings exist. A profile must
bind its exact policy before runtime advertisement. The PDF/UA-1 fixture does not permit either
weaker value.

Document metadata contains a required canonical title and an optional language override; null
language inherits the required document language. The title is author/capture input, not text the PDF
writer may guess from a first heading or filename. Matterhorn failure conditions 06-003 and 06-004
make a missing or non-identifying `dc:title` a PDF/UA-1 failure; this contract supplies the stable
semantic input but does not claim that a writer emitted valid XMP.

### Stable source locators

The source root is the canonical API 2 input root `pliego-input:///`, plus the portable entrypoint.
Source nodes use either:

- a deterministic index in composed-DOM preorder; or
- a generated `before`, `after`, or `marker` slot owned by one composed-DOM preorder index.

Host paths, process-local node IDs, addresses, timestamps, and debug join IDs are excluded. OXH-341
must prove that the typed Servo capture produces these locators deterministically; this contract does
not infer them from serialized HTML after layout.

### Logical tree and reading order

Logical node IDs are unsigned integers equal to their position in the node array. A depth-first walk
from root `0` must visit exactly `0..N-1`. Every non-root node has exactly one parent. Cycles,
duplicates, gaps, dangling IDs, a second `document` node, or an unreachable node reject.

Each node's `children` array is the canonical reading order. It can interleave semantic-node and
page-fragment references, so inline structure and continued page content do not need a second order
map. Consumers must not sort children by node ID, page, geometry, or paint operation. Changing this
array changes semantic identity even when paint bytes do not change.

The closed role vocabulary is:

- document, section, heading, paragraph, span, quote, code, and note;
- list, list-item, list-label, and list-body;
- table, table-head, table-body, table-foot, table-row, table-header-cell, and table-cell;
- figure, formula, caption, and link.

Heading level is a bounded integer from 1 through 65,535, and every heading carries an explicit
canonical title instead of asking the outline writer to extract text. The first numbered heading is
level 1, and later headings cannot skip a level on descent. Lists declare ordered state, one explicit
`decimal`, upper/lower Roman, or upper/lower alphabetic numbering system (or `none` for an unordered
list), and a canonical start;
list-item ordinals follow child order, and every item contains label then body. Tables declare a
bounded row/column grid; row groups, rows, cells, spans, scope, and ascending header references must
form an exact non-overlapping grid. Links carry a canonical resolved API 2 URL and a non-null
accessible name.

The document language is a required canonical, case-normalized restricted BCP 47 form. A node
language is either null to inherit or another canonical form. This version intentionally omits
extensions and private-use subtags and rejects repeated subtags rather than accepting spellings whose
normalization is undefined.

A meaningful figure or formula requires alternate text. Decorative images belong in the artifact
subtree, not as figures with empty alternate text. A logical link carries both an explicit title/name
and alternate text for its tagged annotation, in addition to its resolved target. `replacement_text`
is the profile-neutral ActualText-equivalent value and is allowed on `span`, `code`, `figure`, or
`formula`, including a graphic meant to be consumed primarily as text. These fields model author
intent; they do not by themselves prove that the intent is correct.

### Deterministic navigation

Navigation is always explicit. The `outline` variant has nonempty root and item arrays. Item IDs equal
their array positions, and a depth-first traversal across roots and children must visit exactly
`0..N-1`. Cycles, duplicate references, unreachable items, dangling targets, and reordered IDs reject.
The typed `none` variant exists only for a profile policy that explicitly permits it; absence is never
equivalent to none.

Each outline item carries a canonical title, an inherited or explicit canonical language, one stable
semantic-node target, and one explicit page/app-unit destination. Target nodes are unique. A heading
item title must equal the target heading's explicit title, and the destination page must occur in the
target node's fragment subtree. Page coordinates must remain inside the declared page box. The writer
therefore does not infer bookmarks from headings, geometry, or paint order.

Krilla 0.8.2 exposes `Document::set_outline`, and its outline documentation starts from an empty
outline root before children are added. This contract treats an empty outline as insufficient under
the selected required-navigation policy. Matterhorn checkpoint 27 supplies no separate mechanical
navigation test, while failure condition 11-003 requires language to be determinable for outline
entries that exist. The explicit policy keeps those profile decisions outside PDF object construction.

### Paint fragments and page association

Fragments form a separate locator table in ascending page, operation, and subrange order. Fragment
IDs equal their table positions. Reading order references this table; it is never recovered from the
table's locator order.

The closed fragment kinds are:

- `text`, with nonempty half-open glyph and UTF-8 byte ranges;
- `image`;
- `path`; and
- `annotation`, associated with an API 2 link operation.

Every locator names one one-based page number and zero-based operation index. The fragment kind must
match the retained paint variant. Text ranges must be on UTF-8 boundaries and cover a text operation
without gaps or overlap. Every non-text operation has exactly one fragment. Across the logical and
artifact subtrees, every fragment has exactly one owner. This gives the semantic layer exact paint
closure without making paint traversal the reading-order authority.

An annotation must be under a logical link whose canonical target equals the paint target. A logical
image or path must be under a meaningful figure or formula with alternate text. Text fragments bind
both a glyph range and the exact UTF-8 span covered by those glyphs. The contract retains page association
across pagination while leaving backend tag and annotation-object construction to later work.

### Artifacts and decoration

Artifacts have a separate preorder-indexed forest with the closed classifications `pagination`,
`layout`, `decoration`, and `background`. Pagination artifacts additionally require the explicit
subtype `header`, `footer`, or `page-number`; all other classifications require null subtype. Artifact children may contain only artifact nodes or
fragments. Logical children cannot name artifacts, and a fragment cannot appear in both structures.

This separation makes decoration explicit and prevents a PDF backend from silently guessing that an
unmapped path, image, or text run is decorative. It also prevents pagination furniture from entering
logical reading order merely because it painted first.

### Version and failure policy

Readers accept only the exact schema/version tuple they implement. Unknown role, fragment,
classification, semantics payload, language spelling, or member fails closed. A future role,
language policy, source-locator form, or relationship rule requires a new semantic schema version and
profile compatibility decision. PDF/UA-2 remains a separate profile; this model does not infer it from
PDF/UA-1 semantics.

## API 2 boundary

The self-test verifies that the semantic resource fits API 2's reserved generic semantic-layer
reference and binds the same exact profile tuple. It also verifies that current accepted API 2
fixtures still have `semantic_layer: null`, runtime `profiles` arrays remain empty, and a PDF/UA-1
request remains rejected as unadvertised.

Defining this resource therefore makes no runtime support, tagged-PDF, or PDF/UA conformance claim.
Runtime advertisement can change only at the final R3.5 gate after capture, authoring diagnostics,
tagged writing, validator/reference locks, corpus evidence, human assurance, and assistive-technology
evidence all close.

## Contract evidence

The local self-test currently proves:

- the schema and every nested object are closed;
- one representative logical tree covers document title, required policy, deterministic outline,
  heading title, replacement text, lists, tables, figure/formula alternate text, tagged-link title and
  alternate text, language inheritance/override, and an interleaved figure fragment/caption;
- all four operations in the checked-in API 2 scene are associated exactly once, with the decorative
  path outside logical structure;
- 53 adversarial cases reject, including unknown semantics, ID drift, cycles, dangling references,
  logical/artifact mixing, invalid language, metadata injection, list/table drift, noncanonical links,
  missing title/outline, empty tree/outline, dangling or reordered navigation, missing heading,
  skipped heading levels, invalid list numbering, missing pagination subtype, figure/formula/annotation
  text, glyph/text drift, page-edge destinations, paint mismatch, control characters, and lexical negative zero; and
- 100 fresh Python processes reproduce the exact representative semantic digest.

The 100-process check proves deterministic parsing, validation, canonical serialization, and hashing
of the same contract artifact. It does **not** satisfy OXH-340's direct-capture acceptance by itself:
there is no Servo capture in this slice and no proof that 100 independently captured equivalent pages
produce the same semantic document.

## Consequences

- Reading order, paint order, and fragment-locator order are explicit independent sequences.
- Semantic and artifact ownership is total and unambiguous before a backend sees the document.
- Stable source and page locators can survive process boundaries without exposing engine handles.
- The selected PDF/UA-1 profile can consume the same model without embedding PDF writer policy in it.
- Visual consumers remain unchanged and cannot silently ignore semantics under a requested profile;
  current unprofiled fixtures continue using `semantic_layer: null`.

## Rejected alternatives

- **Put roles on paint operations.** One order cannot represent both paint traversal and logical
  reading order, and visual consumers would accidentally become semantic consumers.
- **Sort structure by geometry or DOM IDs.** Geometry is ambiguous across columns and pagination;
  process-local DOM IDs are not stable artifact identity.
- **Inline fragments inside semantic nodes.** Continued content and reuse checks become harder, and
  exact paint closure cannot be audited as one locator table.
- **Leave unmapped paint as implicit decoration.** Missing semantics would look like a valid artifact
  classification instead of failing closed.
- **Use arbitrary roles or metadata maps.** Readers could not distinguish an ignorable extension from
  a semantic change.
- **Reference the containing scene digest.** The scene already hashes the semantic resource, producing
  an unsatisfiable content-address cycle.
- **Embed PDF structure tags and object policy.** That couples a profile-neutral capture contract to
  one writer and prevents independent backend review.

## Proof boundary and remaining blockers

This contract does not implement or prove typed Servo semantic capture, composed-tree locator
stability, direct-capture equality, runtime decoding, bundle/resource publication, SDK behavior,
authoring diagnostics, tagged-PDF generation, structure-tree/MCID/ParentTree correctness, font
Unicode mapping, annotation tagging, machine validation, human review, assistive-technology behavior,
CI, packaging, or release availability.

OXH-303 and OXH-341 must supply the real typed capture and its repeated-process proof. OXH-342 must
define author-facing fail-closed diagnostics. OXH-343 through OXH-346 must bind this model to tagged
PDF, validation, evidence closure, and the final API/runtime advertisement. Until those gates land,
Pliego produces no public semantic or PDF/UA claim from this contract.

## References

- [Krilla 0.8.2 outline API](https://docs.rs/krilla/0.8.2/krilla/outline/index.html)
- [Matterhorn Protocol 1.1](https://pdfa.org/wp-content/uploads/2021/04/Matterhorn-Protocol-1-1.pdf)
- [ADR 0014: Document scene v1 and canonical ordering](0014-document-scene-v1-and-canonical-ordering.md)
- [ADR 0018: API 2 contract and public artifacts](0018-api-2-contract-and-public-artifacts.md)
- [OXH-303](https://linear.app/oxhq/issue/OXH-303)
- [OXH-339](https://linear.app/oxhq/issue/OXH-339)
- [OXH-340](https://linear.app/oxhq/issue/OXH-340)
- [OXH-341](https://linear.app/oxhq/issue/OXH-341)
- [OXH-342](https://linear.app/oxhq/issue/OXH-342)
- [OXH-346](https://linear.app/oxhq/issue/OXH-346)
