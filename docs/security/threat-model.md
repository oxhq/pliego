# Security threat model

This document describes the intended security boundary of the Pliego v0.2
controlled runtime and its engine API 1 integration. It complements the exact
capability limits in the [support profile](../pliego/support-profile.md). It is not
a claim that the engine is safe for hostile HTML.

## Scope and trust boundary

Pliego is designed to render templates, HTML, JavaScript, fonts, images, and other
assets controlled by the application operator. A document runs in its own native
Pliego process. The caller is responsible for choosing that process's operating-
system identity, filesystem access, network access, time limit, memory limit, CPU
limit, and artifact-retention policy.

| Component or input | Trust assumption |
| --- | --- |
| Application template and bundled assets | Trusted and reviewed by the operator |
| Data inserted into a template | Escaped and validated by the application |
| Explicitly allowlisted remote URL roots | Trusted for the requested document purpose |
| Pliego binary and SDK | Obtained from a verified release or a reviewed build |
| Arbitrary user or tenant HTML/JavaScript | Untrusted and unsupported |
| Host filesystem and credentials | Sensitive; access must be minimized by deployment |

Network denial reduces remote-resource risk, but it does not make untrusted markup
safe. HTML, CSS, JavaScript, image, font, and layout parsers remain a native attack
surface.

## Assets and security goals

The security goals within the supported boundary are:

- protect host files, credentials, and network services from unintended document
  access;
- prevent an incomplete or unsupported render from being presented as a successful
  requested PDF;
- make the exact binary, authorized resources, and retained evidence auditable;
- bound the damage of hangs and excessive resource use through process and deployment
  controls; and
- provide a private path for coordinated vulnerability reporting.

Availability against deliberately malicious documents, hostile multi-tenant
isolation, browser-wide web compatibility, and a proof of byte-deterministic output
for arbitrary documents are outside the current guarantee.

## Threats, controls, and residual risk

| Threat | Current control | Residual risk and operator action |
| --- | --- | --- |
| Server-side request forgery or data exfiltration through remote resources | Network access and cache use are denied by default, redirects are rejected, and remote URL roots require explicit authorization. | An allowlisted service can still expose sensitive data, and a URL root can be broader than intended. Keep egress denied at the OS or container layer unless needed, and grant the narrowest roots. |
| Unintended local-file or host-font access | Callers provide application-owned inputs and explicit assets; host-font fallback is disabled by default. | The native process still has the permissions of its OS identity. Run it with a minimal working directory, read-only inputs, isolated output, and no ambient credentials. |
| CPU, memory, disk, or wall-time exhaustion | One document uses one process; the PHP bridge can enforce a wall timeout and kill that process; resource-body and cache limits exist. | There is no engine-wide hard limit for arbitrary HTML, page count, memory, or CPU. Apply OS or container limits and cap input size, retained artifacts, and concurrent jobs. |
| Native parser, layout, script, font, image, or graphics exploit | The supported model limits inputs to trusted application-owned content; releases track reviewed Servo and dependency changes. | Native vulnerabilities remain possible. Do not accept hostile markup. Use an additional security boundary if the product requires it and keep the engine and OS patched. |
| Remote resource drift or nondeterministic output | Offline rendering is the default; authorized resource evidence can be retained with the job. | Allowlisted content, host state, timestamps, and unsupported dynamic behavior can change output. Bundle exact assets and render offline when reproducibility matters. |
| Partial, stale, or unsupported output treated as success | Unsupported paint fails before the requested PDF is published by default, and SDKs expose typed failures. Validated engine failure evidence is promoted separately from success output; deterministic publication preflight creates no public artifact tree. | A host or power failure can leave private temporary state, and callers can misuse partial-scene diagnostic options. Publish only the returned success artifact and segregate, access-control, and prune diagnostic storage. |
| Abnormal document-process termination leaves private residue | Malformed or abnormal child output is never promoted to the public artifact root, and the staging container is owner-only and excluded from recovery. | A nonempty `.pliego-runtime-*` container can remain for forensic inspection and consume disk. Apply same-user storage quotas and identity-aware retention; do not recursively delete an unverified path. |
| Unsafe hyperlinks in a valid PDF | Link annotations preserve document links. | A PDF viewer may open a malicious destination. Validate or remove user-derived links before rendering and apply viewer-side policy. |
| Compromised or substituted distribution | Releases publish SHA-256 checksums beside native archives; archives include source pointers, licenses, dependency reports, and native notices; the installer verifies package-pinned size and SHA-256. | Checksums served from the same compromised channel are not independent signatures. Pin expected hashes through the application release process. macOS bundles are not Developer ID signed or notarized. |
| Sensitive data retained in evidence | Artifact path fields and retention behavior are explicit to the caller and SDK; a preflight path field is not an existence guarantee. | Inputs, resources, diagnostics, and PDFs can contain personal or confidential data. Check that evidence exists before handling it, restrict access, encrypt storage where required, set short retention, and avoid uploading evidence to public issues. |

## Deployment checklist

For the supported trusted-input use case:

1. Verify the Pliego archive and use the SDK-pinned engine version.
2. Render only application-owned templates; escape and validate inserted data.
3. Keep network, redirects, host fonts, and cache disabled unless a document requires
   a narrowly reviewed exception.
4. Run the process as an unprivileged identity with an isolated writable directory,
   no ambient secrets, and only the required read-only assets.
   On Unix, do not install a custom or auto-reaping `SIGCHLD` disposition around Pliego;
   the supervisor rejects that launch state before it creates a worker process group.
5. Apply OS or container wall-time, memory, CPU, process-count, file-size, and disk
   quotas. Set application concurrency according to measured capacity.
6. Treat a timeout, crash, unsupported feature, readiness failure, or partial-scene
   result as a failed job. Never promote a diagnostic PDF to the requested output.
7. Protect and expire input, resource, scene, PDF, log, and diagnostic artifacts
   according to the data classification of the document. Treat a reported artifact
   path as a locator and check that it exists before reading diagnostics.
8. Keep Servo, Pliego, SDK, and operating-system updates in the normal security patch
   process.

## Vulnerability reporting

Do not open a public issue for a suspected vulnerability. Follow the private GitHub
Security Advisory process described in [SECURITY.md](../../SECURITY.md). Generic
Servo vulnerabilities may require coordinated upstream disclosure; the project will
coordinate that handoff rather than asking reporters to disclose twice.

Future work toward stronger isolation must define and test a new trust boundary. It
must not silently broaden the v0.2 supported-input claim.
