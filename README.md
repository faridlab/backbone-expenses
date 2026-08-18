# backbone-expenses

Employee expense claims — the People pillar's H-4 surface (Wave 1 P3). Odoo 19 `hr_expense`
semantics: **no sheet model** — grouping is a read projection and posting goes direct to the
General Ledger, one envelope per expense.

## What it owns

- `expense_categories` — company expense classifications with their GL expense account
  (per-category accounts; the locked W1 decision — no catalog coupling).
- `expenses` — the claim: employee, category, date, amount (ex-tax), currency (single-currency
  v1), dual payment mode (`own_account` = employee reimbursable, `company_account` = company
  paid), reference, receipt link (bucket file id), and the **two-field lifecycle**:
  `approval_state` (hand-set: draft → submitted → approved | refused) + `state` (computed+stored
  financial: draft → submitted → approved → posted → done | refused). Legal pairs are enforced by
  a DB CHECK.
- `expense_tax_lines` — the **removable tax overlay** (mirrors billing's `invoice_tax_lines`):
  pre-computed input-VAT / withholding lines. No backbone-tax dependency; an empty overlay is
  legal.

## Seams (ADR-0004 — zero Cargo edges between shipped modules)

| Seam | Port | Default | Wired by |
|---|---|---|---|
| GL posting | `GlPostSink` (re-exported from `backbone-gl-posting`, `source_type = "expense"`) | unwired → `post` rejects with `gl_seam_unwired` | the composing app over accounting's `PostingService` (finance wave) |
| Approvals | `ApprovalFiling` (file + status, fail-closed TR2) | `UnwiredApprovals` | H-9 engine (P6); engine-side `expense` resource variant noted there |
| Reimbursement | `ReimbursementSink` | `UnwiredReimbursement` (settle fails closed) | the finance wave over payment's `create_payment` |

## Composition duties

The module enforces its own invariants (row-truth state guards, payload-bound submit link,
in-company category reads, balanced envelopes, TR2 fail-closed approvals). A composing service
remains responsible for:

- **employee_id validation** — a claim's `employee_id` is trusted to belong to the claim's
  company; the host maps its authenticated actor to an in-company employee before calling.
- **category-master authorization** — the guarded claim verbs are locked behind the module's
  auth, but who may create/edit `expense_categories` (an operator surface) is the host's RBAC
  decision.
- **GL account validation** — `post_accounts` carries caller-supplied GL account ids; the host
  adapter over accounting must resolve them against the company's own chart of accounts.

## Fence

`company_fence: strict` from birth (ADR-0014) — all three tables carry company-isolation RLS.

## Golden path

```bash
metaphor schema validate
metaphor schema generate
metaphor migration generate <name>
metaphor test
metaphor lint check
```

See `CLAUDE.md` for the module conventions and `docs/` for the handbook.
