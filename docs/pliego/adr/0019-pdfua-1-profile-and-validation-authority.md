# ADR 0019: PDF/UA-1 profile and validation authority

- Status: Accepted
- Date: 2026-08-10

## Context

Pliego 1.0 needs one accessibility target before the semantic scene, PDF writer, and API 2
contract freeze. A tagged PDF is not automatically accessible, and a machine validator cannot decide
every semantic question. Advertising a profile before those distinctions and proof layers are stable
would turn partial implementation into a public conformance claim.

The current public `DocumentScene` has `semantic_layer: null`, the API 2 runtime fixture advertises no
profiles, and the Krilla adapter is proved only for positioned text, paths, images, and links. It does
not currently produce or claim tagged PDF, PDF/UA-1, PDF/UA-2, or assistive-technology
interoperability.

The two candidate standards have different foundations:

| Candidate | Foundation | Available validation material | Pliego 1.0 impact |
| --- | --- | --- | --- |
| PDF/UA-1 | ISO 14289-1:2014 over ISO 32000-1:2008 (PDF 1.7) | Matterhorn Protocol 1.1, PDF/UA-1 Reference Suite 1.1, Tagged PDF Best Practice Guide: Syntax, and veraPDF `ua1` machine checks | Requires a semantic scene and tagged writer, but does not also require adopting PDF 2.0 |
| PDF/UA-2 | ISO 14289-2:2024 over ISO 32000-2:2020 (PDF 2.0) | Newer PDF/UA-2 guidance and validator support | Adds a PDF 2.0 writer and a newer structure model before the existing PDF 1.x path has semantic proof |

Neither candidate is implemented today. PDF/UA-1 is the smaller compatible 1.0 target and has a
stable public test protocol and reference suite. PDF/UA-2 remains a future, separately versioned
profile rather than an automatic upgrade of PDF/UA-1.

PDF/UA-1 can coexist with suitable PDF/A profiles, but Pliego 1.0 does not infer or claim dual
conformance. A future PDF/A combination needs its own explicit profile, writer policy, validator
lock, and evidence.

## Decision

Select this exact API 2 profile reference for Pliego 1.0:

```json
{"schema":"pliego.profile.pdfua-1","version":1}
```

Profile selection is explicit. A runtime may accept the reference only when that exact tuple appears
in its `pliego.runtime-contract` API 2 `profiles` array. An unknown schema, unknown version, or a
known but unadvertised profile fails before rendering. Readers do not assume forward compatibility
between profile versions.

The checked-in descriptor defines the contract but does not advertise support. Runtime profile arrays
remain empty until the final R3.5 gate has all required proof.

### Three distinct output states

- **Untagged PDF** has no semantic structure claim and is not PDF/UA conforming.
- **Tagged PDF** contains a structure tree, but tagging alone is not a PDF/UA claim.
- **PDF/UA-1 conforming** means the exact profile was requested and advertised, the PDF metadata and
  render identity bind that profile, and deterministic evidence records every required Pliego gate as
  passed. A tagged or machine-clean PDF without the other gates remains non-conforming for Pliego's
  public contract.

### Authority hierarchy

Use the following order when sources disagree:

1. ISO 14289-1:2014, edition 2, is the normative PDF/UA-1 authority.
2. ISO 32000-1:2008 is the normative base PDF authority where ISO 14289-1 incorporates it.
3. Matterhorn Protocol 1.1 is the public conformance testing model. It maps failure conditions and
   practical machine/human classifications, but it does not replace either ISO standard.
4. Tagged PDF Best Practice Guide: Syntax is implementation guidance. It may make the output better
   but cannot weaken a normative requirement.
5. PDF/UA-1 Reference Suite 1.1 supplies positive examples. Passing or resembling the suite is not
   proof of complete conformance.
6. The exact locked veraPDF distribution and its built-in `ua1` profile supply per-render machine
   results. A validator result does not settle human-semantic or assistive-technology questions.

The Matterhorn 1.1 inventory has 31 checkpoints and 136 failure conditions: 87 are classified as
machine-determinable, 47 generally require human judgment, and 2 have no specific test (`23-001` and
`27-001`). These classifications are an evidence-routing model, not a claim that either class is
sufficient by itself or permanently tied to one implementation technique.

Normative clause text is not copied into this repository. Full clause access and a human review of
the licensed ISO publications remain mandatory lock inputs.

### Content-addressed author assurance

The profile reserves `pliego/author-assurance.json` in the normalized input manifest with media type
`application/vnd.pliego.author-assurance+json`. The input manifest supplies the assurance document's
real byte length and SHA-256 address. The document binds the exact profile and content-addressed
source or template revision.

An unevaluated assurance is valid input but cannot satisfy conformance. A passed or failed assurance
must carry a stable reviewer identifier, a content-addressed review record, and checkpoint results.
Detailed authoring semantics remain owned by the semantic authoring contract; this envelope prevents
that later contract from depending on a host path, timestamp, or mutable template name.

### Four independent evidence gates

`pliego.conformance-evidence.pdfua-1` distinguishes:

1. per-render machine validation of the exact PDF under the exact validation lock;
2. release-corpus proof for the exact runtime release;
3. human or template assurance supplied through the content-addressed author-assurance input; and
4. assistive-technology evidence from a pinned release matrix.

Every report is content-addressed. The deterministic evidence document contains no host path, wall
clock, mutable URL result, or free-form claim. `satisfied` requires all four gates to be `passed`, an
exact ready validation lock, and no blockers. One failed gate yields `failed`. Missing proof yields
`not-evaluated`, never partial conformance.

This OXH-339 slice defines that future envelope but deliberately rejects every `satisfied` document
during semantic validation. OXH-345 and OXH-346 must add resolved byte closure for the ready lock,
author assurance, validator and corpus reports, and assistive-technology matrix before removing that
guard. Artifact-shaped hashes or four unverified status strings can never activate the profile.

### Fail-closed validator and reference lock

The validation lock pins identities and revisions now, while unresolved bytes remain explicit:

- ISO 14289-1:2014 edition 2 and ISO 32000-1:2008 edition 1;
- Matterhorn Protocol 1.1;
- Tagged PDF Best Practice Guide: Syntax, 2019 publication corrected 2023-07-26;
- PDF/UA-1 Reference Suite 1.1;
- veraPDF 1.30.2, annotated source tag `v1.30.2`, tag object
  `91d810bac357ef114f1a178d247d61d0233f9472`, peeled source commit
  `7d9b5c3f709846ab83f86ca1a538b24eac2d3f72`, and flavour `ua1`.

The initial lock state is `blocked`. Licensed ISO clause review, authoritative document/archive byte
digests, the veraPDF distribution URL and digest, a verified signature digest/key, and a container
image reference/digest are deliberately null and named as blockers. A release may not replace them
with guessed values, a mutable `latest` tag, or hashes of an unofficial mirror.

## Compatibility and failure behavior

- Request, runtime advertisement, semantic layer, PDF metadata, result, and evidence bind the same
  exact profile reference and standard revision.
- Unsupported profile negotiation is an invocation error before document execution, not a best-effort
  untagged render.
- Conformance failure publishes no successful delivery under the requested profile.
- A validator upgrade, protocol revision, reference-suite byte change, or authority interpretation
  change requires a new ready lock and release-corpus proof. A semantic contract change requires a
  new profile version.
- PDF/UA-2 support requires a different profile schema/version and cannot be inferred from a
  PDF/UA-1 request.

## Consequences

- Pliego 1.0 has one precise accessibility target without prematurely freezing PDF 2.0 behavior.
- Machine, corpus, human/template, and assistive-technology proof cannot be collapsed into one green
  boolean.
- Mutable upstream downloads cannot silently change the validator or reference corpus.
- Current builds continue making no PDF/UA claim; the contract is reviewable before writer work.
- R3.5 remains blocked on semantic scene, authoring, tagged writer, real verifier/reference pins,
  corpus execution, human review, and assistive-technology evidence.

## Rejected alternatives

- **Select PDF/UA-2 for 1.0.** It couples the first semantic release to PDF 2.0 writer adoption and a
  newer interoperability surface without current runtime proof.
- **Call any tagged PDF conforming.** Tags are necessary structure, not complete conformance evidence.
- **Treat veraPDF success as sufficient.** veraPDF explicitly performs machine-verifiable PDF/UA
  checks; semantic judgment remains separate.
- **Use the reference suite as a validator.** Positive examples do not enumerate all invalid output.
- **Pin only a version string or `latest` image.** The executed bytes could change without a contract
  change.
- **Inline reviewer prose in render results.** It is not canonical, content-addressed, or reusable
  across a reviewed template revision.

## Proof boundary

This ADR and the adjacent schemas prove only a local, versioned contract and fail-closed unresolved
lock state. They do not prove tagged-PDF generation, ISO conformance, veraPDF execution, corpus
coverage, PDF/A coexistence, assistive-technology behavior, reader interoperability, CI, packaging,
or release availability.

## References

- [ISO 14289-1:2014](https://www.iso.org/standard/64599.html)
- [ISO 32000-1:2008](https://www.iso.org/standard/51502.html)
- [PDF Association: ISO 14289-1](https://pdfa.org/resource/iso-14289-pdfua/)
- [Matterhorn Protocol 1.1](https://pdfa.org/resource/the-matterhorn-protocol/)
- [Tagged PDF Best Practice Guide: Syntax](https://pdfa.org/resource/tagged-pdf-best-practice-guide-syntax/)
- [PDF/UA-1 Reference Suite 1.1](https://pdfa.org/resource/pdfua-reference-suite/)
- [veraPDF validation documentation](https://docs.verapdf.org/validation/)
- [veraPDF CLI profile selection](https://site.verapdf.org/cli/validation/)
- [veraPDF apps v1.30.2](https://github.com/veraPDF/veraPDF-apps/releases/tag/v1.30.2)
- [ADR 0016: Krilla as the initial DocumentScene PDF backend](0016-document-scene-pdf-backend.md)
- [ADR 0018: API 2 contract and public artifacts](0018-api-2-contract-and-public-artifacts.md)
- [OXH-339](https://linear.app/oxhq/issue/OXH-339)
- [OXH-346](https://linear.app/oxhq/issue/OXH-346)
