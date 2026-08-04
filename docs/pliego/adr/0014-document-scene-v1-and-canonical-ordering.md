# ADR 0014: Document scene v1 and canonical ordering

- Status: Accepted
- Date: 2026-07-19

## Context

Pliego needs a deterministic scene artifact between retained Servo layout and document backends.
The current implementation defines `DocumentScene` as typed JSON, retains paint events from one
display-list traversal, and stores captured binary resources by SHA-256. The format needs an explicit
hash boundary and ordering rule before more operations and resources are added.

This decision describes the implemented internal format. It does not declare a stable public schema.

## Decision

### Envelope and version

A document scene is a JSON object with this typed envelope:

```json
{
  "schema": "pliego.document-scene",
  "version": 1,
  "pages": [
    {
      "size": { "width": 612.0, "height": 792.0 },
      "operations": []
    }
  ]
}
```

Readers validate both `schema` and `version` and reject values they do not implement. Version 1
requires at least one page and preserves the ordered page sequence produced by pagination.

Named pages and stable page identifiers are not scene fields. Version 1 is an internal working
schema, not a compatibility promise for external clients. A stable public schema and compatibility
policy require a separate decision.

### Canonical operation order

`Page.operations` preserves the order observed by the single retained display-list paint traversal.
Scene production does not trigger a second layout traversal and does not sort operations by type,
geometry, DOM order, or a process-local identifier. Repeated operations remain repeated and retain
their relative order.

The retained debug snapshot may contain fragment, tag, spatial-node, clip, or similar capture-local
identifiers for joining diagnostic data. Those identifiers are not scene fields and do not
participate in canonical ordering or scene identity.

### Resources

Binary resources referenced by scene operations are content-addressed as `sha256:<digest>`. Resource
bytes are stored under their digest with collision verification. Request IDs, source URLs, redirects,
WebRender keys, font identifiers, and other runtime handles remain provenance or debug metadata; they
are not resource identities and are excluded from the normalized scene hash unless represented by an
explicit stable scene field.

### Typed JSON and unknown operations

Operations use the tagged `type` field and the implemented `text`, `path`, `image`, and `link`
variants. Deserialization rejects an unknown operation variant; there is no catch-all operation.
This strictness applies to variants. Version 1 does not reject every unknown object member because
the current Serde types do not use `deny_unknown_fields`.

The canonical serializer operates on the typed scene, not on arbitrary JSON objects. Struct field
order and sequence order are therefore deterministic in the current representation; no recursive
JSON-key sorting step exists. A future schema field with map semantics must define a deterministic
ordering before it can enter the hash boundary.

### Normalized hash boundary

The scene hash is SHA-256 over the exact compact UTF-8 bytes returned by
`DocumentScene::normalized_json()`. That function first validates the schema, version, non-empty
ordered page sequence, geometry, and required strings, then serializes the typed value with
`serde_json::to_vec`.
Pretty-printed artifacts, input whitespace, debug snapshots, process-local join IDs, and binary
resource bytes outside the scene envelope are not hashed directly. Content-addressed resource
references inside operations are part of the serialized scene and therefore bind the scene to those
bytes.

## Consequences

- Equal validated typed scenes with equal sequence order produce equal normalized bytes and hashes.
- Paint order remains authoritative; a geometry-based or DOM-based reordering would be a schema
  behavior change.
- Unknown operation types and unknown schema versions fail instead of degrading silently.
- Binary resources deduplicate by content while URLs and runtime identifiers cannot perturb resource
  identity.
- A stable public compatibility contract remains intentionally unresolved.

## References

- [`ports/pliego/src/scene.rs`](../../../ports/pliego/src/scene.rs)
- [`ports/pliego/src/session.rs`](../../../ports/pliego/src/session.rs)
- [`components/shared/layout/lib.rs`](../../../components/shared/layout/lib.rs)
- [`components/layout/display_list/mod.rs`](../../../components/layout/display_list/mod.rs)
