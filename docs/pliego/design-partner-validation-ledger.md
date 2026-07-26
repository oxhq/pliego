# Pliego design-partner validation ledger

Status: **empty redacted template**. The zero values below are initialization, not
outreach, customer, template, payment, or market evidence.

Use a private working ledger for authorized contact details and source evidence.
Commit or share only a redacted snapshot in this format. Never add names, email
addresses, private repository URLs, template source, customer data, credentials, or
unredacted payment records.

This ledger records the OXH-273 decision gate. It does not authorize outreach, change
Linear status, or claim validation by itself.

## Counting rules

- **Targeted outreach** counts one human-authorized, direct outreach to one unique
  Laravel team selected for a documented production-document reason. Bulk spam,
  duplicate follow-ups, likes, stars, and passive sign-ups do not count.
- **Qualified conversation** counts one two-way conversation confirming a real
  production document problem, one plausible deployment, access to an authorized
  template, an acceptance owner, and ability to consider the fixed deposit. A reply
  expressing general interest does not count. The threshold counts at most one
  qualified conversation per redacted organization ID (`ORG-*`); follow-up calls or
  additional contacts at the same team do not increase the total.
- **Template inspected** counts one real, authorized, problematic production template
  inspected deeply enough to record its document class, current renderer, failure,
  resources, and requested capability. Each `TPL-*` ID maps to exactly one stable,
  unique authorized template identity. Reinspection or another variant of that same
  template keeps the same `TPL-*` ID and does not increase the total. A synthetic demo
  does not count.
- **Paid deposit** counts cleared funds of at least **$3,750** from a qualified customer
  against the signed one-family, one-deployment scope. A verbal yes, letter of intent,
  purchase-order discussion, unpaid invoice, or unpaid pilot does not count as
  cleared payment evidence. The threshold counts at most one depositing customer per
  `ORG-*`, regardless of how many `DEP-*` payment rows or installments that organization
  has.
- One organization may have several contacts, but it is counted once for targeted
  outreach, once for a qualified conversation, and once as a depositing customer.

### Reconciliation rule

At the evidence freeze, derive the gate-summary actuals from eligible log rows; never
type an estimated total:

- outreach actual = distinct `ORG-*` values on eligible `OUT-*` rows;
- conversation actual = distinct `ORG-*` values on eligible `CONV-*` rows;
- template actual = distinct eligible `TPL-*` values after reconciling each ID to one
  unique authorized template in the private evidence;
- deposit actual = distinct `ORG-*` values whose eligible cleared `DEP-*` rows against
  one signed scope total at least $3,750.

If duplicate or conflicting rows exist, mark them ineligible until the private evidence
custodian reconciles them. The redacted summary records the eligible unique-ID counts,
not the number of log rows.

## Gate summary

- Evidence freeze: `YYYY-MM-DD HH:MM TZ`
- Private evidence custodian: `[authorized human/role]`
- Redaction reviewed by: `[authorized human/role]`

| Measure | Required | Actual | Evidence query or private reference | Met? |
| --- | ---: | ---: | --- | --- |
| Unique targeted outreaches | 50 | 0 | `OUT-*` | No |
| Qualified conversations | 15 | 0 | `CONV-*` | No |
| Real problematic templates inspected | 8 | 0 | `TPL-*` | No |
| Qualified customers with cleared deposits | 2 at $3,750 or more | 0 | `DEP-*` | No |

Do not replace actual counts with percentages or pipeline estimates.

## Outreach and qualification log

Add one row per unique targeted organization. Store contact identity and message
contents privately.

| Outreach ID | Date | Redacted organization ID | Segment and document class | Why targeted | Human authorization ref | Channel | Outcome | Qualified conversation ID | Eligible? | Evidence ref |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `OUT-001` | `YYYY-MM-DD` | `ORG-001` | `[Laravel segment / invoice]` | `[specific production pain signal]` | `[private approval ref]` | `[email/event/referral]` | `[no reply/declined/replied/qualified]` | `[CONV-001 or —]` | `[yes/no + reason]` | `[private/redacted ref]` |

## Qualified conversation log

Add a row only when the counting rule is satisfied.

| Conversation ID | Date | Organization ID | Production pain | Current renderer | One deployment identified? | Template authorized? | Acceptance owner? | Deposit feasible? | Objections | Requested capabilities | Eligible? | Evidence ref |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `CONV-001` | `YYYY-MM-DD` | `ORG-001` | `[specific painful job]` | `[DOMPDF/Browsershot/etc.]` | `[yes/no]` | `[yes/no]` | `[yes/no]` | `[yes/no/unknown]` | `[coded summary]` | `[coded summary]` | `[yes/no + reason]` | `[private/redacted ref]` |

## Template inspection log

Do not paste source or customer data. Record only the minimum redacted classification
needed to aggregate product learning.

| Template ID | Date | Organization ID | Document family | Current renderer | Reproducible problem | Pages/data shape | Key resources | Requested capability | Reusable generic class | Eligible? | Private evidence ref |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `TPL-001` | `YYYY-MM-DD` | `ORG-001` | `[invoice/statement/etc.]` | `[renderer]` | `[redacted failure]` | `[bounded summary]` | `[fonts/images/SVG/JS/network]` | `[capability]` | `[class or none]` | `[yes/no + reason]` | `[private ref]` |

## Deposit log

Amounts may be reported as the threshold or a range in the redacted copy. Store the
signed scope and payment record privately.

| Deposit ID | Cleared date | Organization ID | Amount evidence | Signed scope ref | One family | One deployment | Evidence custodian | Counts toward gate? |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `DEP-001` | `YYYY-MM-DD` | `ORG-001` | `>= $3,750` | `[private ref]` | `[redacted family ID]` | `[redacted deployment ID]` | `[role]` | `[yes/no + reason]` |

## Objection and capability roll-up

Aggregate only evidence linked to qualified conversations or inspected templates.
Do not turn a single comment into a market claim.

| Code | Category | Count | Linked IDs | Reusable learning or next test |
| --- | --- | ---: | --- | --- |
| `OBJ-001` | `[price/trust/compatibility/deployment/timing/etc.]` | 0 | `[CONV-*]` | `[evidence-backed summary]` |
| `CAP-001` | `[table/font/JS/resource/diagnostics/etc.]` | 0 | `[CONV-* / TPL-*]` | `[generic document class or capability]` |

## Decision record

- Decision: `pending`
- Allowed final values: `advance` or `stay`
- Decision date: `YYYY-MM-DD`
- Authorized decision owner: `[human/role]`

### Threshold evaluation

- [ ] 50 unique targeted outreaches are evidenced.
- [ ] 15 qualified conversations are evidenced.
- [ ] 8 real problematic templates are evidenced.
- [ ] 2 qualified customers have each paid a cleared deposit of at least $3,750.

`advance` is valid only when all four boxes are checked. If any box remains unchecked,
record `stay`; do not expand into M5+ compatibility work on unpaid interest.

### Advance rationale

Complete only for `advance`:

- Paid painful job:
- Repeated document classes:
- Capabilities required by both paid scopes:
- Generic open-source work implied:
- Explicit evidence references:

### Stay rationale

Complete only for `stay`:

- Missing threshold(s):
- Strongest evidenced objection:
- What was learned from inspected templates:
- Cheapest falsifiable follow-up, if any:
- Work explicitly not authorized:

The decision record reports evidence; it does not itself move a Linear issue or
authorize roadmap expansion.
