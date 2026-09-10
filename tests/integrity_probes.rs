//! Integrity probes — route-level (Wave 1 P3, H-4). The guarded composition locks generic
//! mutation, the two-field lifecycle holds its legal pairs, the DB CHECK is the arbiter, and
//! the three seams fail closed.
//!
//! Tenancy: the module ships NONE (ADR-0029) — no company column, no fence declaration, no
//! tenant predicate in any statement. Two things replace the old in-module fence:
//!
//! 1. **Caller identity** rides the request as the [`OrgContext`] extension the composing
//!    service's org auth middleware inserts (the lead probe-suite harness pattern). The
//!    handlers require its PRESENCE (401 without one) and derive nothing tenant-shaped from
//!    it — route gating + actor stamping only.
//! 2. **Row isolation** is the composing service's tenancy decorator. This suite connects as
//!    the DB owner (a superuser, whom RLS can never bind), so raw assertion SQL runs plain —
//!    and the POSTURE itself is pinned from below by EXP-11/EXP-17 under `SET ROLE` to a
//!    plain non-superuser: the tenant axis is gone, the RLS enable+force flags stay armed for
//!    the decorator, and until the decorator installs policies the probe role is default-denied
//!    (zero rows, writes refused) no matter what legacy variable is set.
//!
//! Verbs that feed an unstripped consumer seam (submit → approvals, post → GL, settle →
//! reimbursement) still need the seams' legacy company twin. The write service sources it
//! from the AMBIENT org scope and fails closed when none is bound — so every probe that
//! drives submit/post/settle runs inside [`scoped`], the single-company scope emulation
//! (`OrgScope::for_company_unit`) exactly mirroring what a composing service binds per
//! request. EXP-25 pins the fail-closed twin itself.
//!
//! The module's own write service is default-UNWIRED (the family posture), so route-level
//! probes assert the fail-closed contract; the wired-success paths (post → posted with a
//! balanced envelope, settle → done, submit → linked) run at the SERVICE level on a
//! separately-constructed `ExpensesWriteService::new(pool).with_…(fake)` — exactly the shape
//! a composing service uses to wire the adapters.
//!
//! DB: DATABASE_URL wins, else the module's local test DB (`backbone_expenses_test` on the
//! metaphora dev postgres, migrated). Fresh random ids per test so parallel runs never
//! collide.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode};
use axum::middleware::{self, Next};
use rust_decimal::Decimal;
use sqlx::{Acquire, PgPool};
use std::future::Future;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;

use backbone_auth::org::OrgContext;
use backbone_expenses::{
    create_guarded_expenses_routes, org_scope, AccountingPostEnvelope, ApprovalFiling,
    ApprovalSeamError, ApprovalVerdict, ExpenseApprovalFilingRequest, ExpensePaymentMode,
    ExpenseState, ExpensesModule, ExpensesWriteService, GlPostAck, GlPostSink, GlPostRejected,
    NewExpense, PostAccounts, ReimbursementAck, ReimbursementRequest, ReimbursementSeamError,
    ReimbursementSink, TaxLineWrite,
};

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgresql://serpa:serpa_dev_password@127.0.0.1:5432/backbone_expenses_test".into()
    });
    PgPool::connect(&url).await.unwrap()
}

async fn module(pool: &PgPool) -> ExpensesModule {
    ExpensesModule::builder()
        .with_database(pool.clone())
        .build()
        .unwrap()
}

/// The caller identity a request carries in production (inserted by the composing service's
/// org auth layer). The module's handlers only require its PRESENCE — the `OrgContext`
/// extractor rejects a request without one 401 — and derive nothing tenant-shaped from it.
fn caller() -> OrgContext {
    OrgContext {
        acting_unit_id: Uuid::new_v4(),
        entitled_units: vec![],
        legacy_company_id: None,
        user_id: Uuid::new_v4().to_string(),
    }
}

/// Wrap the router with the extension the host auth stack provides in production.
fn with_caller(router: axum::Router, org: OrgContext) -> axum::Router {
    router.layer(middleware::from_fn(
        move |mut req: Request, next: Next| {
            let org = org.clone();
            async move {
                req.extensions_mut().insert(org);
                next.run(req).await
            }
        },
    ))
}

/// The guarded composition + caller extension — the mounting a composing service uses.
fn guarded(m: &ExpensesModule) -> axum::Router {
    with_caller(create_guarded_expenses_routes(m), caller())
}

/// Run `f` with an ambient org scope bound — the single-company emulation of what a
/// composing service resolves and binds per request. The write service picks the seams'
/// legacy company twin off this scope (and relays it onto its transactions).
async fn scoped<F, R>(pool: &PgPool, company: Uuid, f: F) -> R
where
    F: Future<Output = R>,
{
    org_scope::with_org_request_scope(
        pool,
        org_scope::OrgScope::for_company_unit(company),
        f,
    )
    .await
    .unwrap()
}

async fn req(app: axum::Router, method: &str, uri: &str, body: String) -> StatusCode {
    req_full(app, method, uri, body).await.0
}

/// Status + body — for probes that must pin the stable error code, not just the status.
async fn req_full(app: axum::Router, method: &str, uri: &str, body: String) -> (StatusCode, String) {
    let r = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// Seed a live category and return its id (fresh ids per test, so no clashes). No tenant
/// axis to seed — the module's tables carry none (the decorator backfills org_unit_id).
async fn seed_category(pool: &PgPool, code: &str) -> Uuid {
    let id = Uuid::new_v4();
    let account = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO expenses.expense_categories (id, code, name, expense_account_id, metadata)
           VALUES ($1, $2, $3, $4, '{}'::jsonb)"#,
    )
    .bind(id)
    .bind(code)
    .bind(format!("category {code}"))
    .bind(account)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn create_claim(
    app: axum::Router,
    category: Uuid,
    employee: Uuid,
    amount: &str,
) -> (StatusCode, String) {
    let body = format!(
        r#"{{"employeeId":"{employee}","categoryId":"{category}","expenseDate":"2026-08-10","description":"taxi to client","amountTotal":{amount}}}"#
    );
    req_full(app, "POST", "/expenses", body).await
}

fn claim_id(body: &str) -> Uuid {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// Plain scalar read for assertions — the suite connects as the DB owner, whom RLS can never
/// bind, so the strip-posture tables answer without any scope binding.
async fn one<T>(pool: &PgPool, sql: String) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres>
        + sqlx::Type<sqlx::Postgres>
        + Send
        + Sync
        + Unpin,
{
    sqlx::query_scalar::<_, T>(&sql)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn state_pair(pool: &PgPool, id: Uuid) -> (String, String) {
    one(
        pool,
        format!(
            "SELECT (approval_state::text, state::text) FROM expenses.expenses WHERE id = '{id}'"
        ),
    )
    .await
}

/// A `NewExpense` with the boring parts filled in.
fn new_claim(
    category: Uuid,
    employee: Uuid,
    amount: Decimal,
    mode: ExpensePaymentMode,
) -> NewExpense {
    NewExpense {
        employee_id: employee,
        category_id: category,
        expense_date: chrono::NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
        description: "probe claim".into(),
        amount_total: amount,
        currency: "IDR".into(),
        payment_mode: mode,
        reference: None,
        receipt_file_id: None,
    }
}

fn accounts() -> PostAccounts {
    PostAccounts {
        employee_payable_account_id: Uuid::new_v4(),
        bank_account_id: Uuid::new_v4(),
    }
}

// ─── in-test seam fakes (the composition layer's stand-ins) ───────────────────

/// Records the last envelope it was handed and acks it — proves `post` sends a BALANCED
/// envelope and lets the test read it back.
#[derive(Default)]
struct RecordingGlSink {
    last: Mutex<Option<AccountingPostEnvelope>>,
}

#[async_trait::async_trait]
impl GlPostSink for RecordingGlSink {
    async fn post(
        &self,
        envelope: &AccountingPostEnvelope,
    ) -> Result<GlPostAck, GlPostRejected> {
        *self.last.lock().unwrap() = Some(envelope.clone());
        Ok(GlPostAck {
            post_id: Uuid::new_v4(),
            journal_id: Uuid::new_v4(),
            idempotent_reuse: false,
        })
    }
}

/// Acks every reimbursement with a fixed payment id.
struct FixedReimbursement(Uuid);

#[async_trait::async_trait]
impl ReimbursementSink for FixedReimbursement {
    async fn reimburse(
        &self,
        _req: &ReimbursementRequest,
    ) -> Result<ReimbursementAck, ReimbursementSeamError> {
        Ok(ReimbursementAck { payment_id: self.0 })
    }
}

/// Files with a fresh id; the verdict is whatever the test staged.
struct FakeApprovals {
    verdict: ApprovalVerdict,
}

#[async_trait::async_trait]
impl ApprovalFiling for FakeApprovals {
    async fn file(
        &self,
        _req: &ExpenseApprovalFilingRequest,
    ) -> Result<Uuid, ApprovalSeamError> {
        Ok(Uuid::new_v4())
    }
    async fn status(&self, _id: Uuid) -> Result<ApprovalVerdict, ApprovalSeamError> {
        Ok(self.verdict)
    }
}

// ─── EXP-1: create happy path — draft/draft, IDR default, exactly one row ─────

#[tokio::test]
async fn guarded_create_lands_draft_draft() {
    let pool = pool().await;
    let m = module(&pool).await;
    let category = seed_category(&pool, "TRVL").await;
    let employee = Uuid::new_v4();

    // No ambient scope: the module's create carries no tenant axis at all.
    let (status, body) = create_claim(guarded(&m), category, employee, "250000").await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    assert!(
        body.contains(r#""approvalState":"draft""#),
        "approval_state draft: {body}"
    );
    assert!(body.contains(r#""state":"draft""#), "state draft: {body}");
    assert!(body.contains(r#""currency":"IDR""#), "IDR default: {body}");

    let n: i64 = one(
        &pool,
        format!("SELECT count(*) FROM expenses.expenses WHERE employee_id = '{employee}'"),
    )
    .await;
    assert_eq!(n, 1, "exactly one claim row");
}

// ─── EXP-2: no tenant predicate ships — another scope reads the same row ──────

#[tokio::test]
async fn no_tenant_predicate_ships_in_the_box() {
    let pool = pool().await;
    let company_a = Uuid::new_v4();
    let company_b = Uuid::new_v4();
    let category = seed_category(&pool, "MEAL").await;
    let employee = Uuid::new_v4();

    let svc = ExpensesWriteService::new(pool.clone());
    let claim = scoped(
        &pool,
        company_a,
        svc.create_expense(
            new_claim(category, employee, Decimal::new(50_000, 2), ExpensePaymentMode::OwnAccount),
            None,
        ),
    )
    .await
    .unwrap();

    // The undecorated module carries no scoping column and no fence, so a DIFFERENT scope
    // reads the row straight away. Isolation is the composing service's decorator (ADR-0029)
    // — asserted there, deliberately not here.
    let seen = scoped(&pool, company_b, svc.get_expense(claim.id))
        .await
        .expect("undecorated module: another scope's read sees the row");
    assert_eq!(seen.id, claim.id);
}

// ─── EXP-3: input validation — negative amount 422, stable code ───────────────

#[tokio::test]
async fn negative_amount_is_refused() {
    let pool = pool().await;
    let m = module(&pool).await;
    let category = seed_category(&pool, "PRNT").await;

    let (status, body) = create_claim(guarded(&m), category, Uuid::new_v4(), "-1").await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "negative amount: {body}"
    );
    assert!(body.contains("negative_amount"), "stable code: {body}");
}

// ─── EXP-4: row-truth guard — a submitted claim rejects edits and re-submit ──

#[tokio::test]
async fn row_truth_guards_the_lifecycle() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "FUEL").await;
    let employee = Uuid::new_v4();

    let pair = scoped(&pool, company, async {
        let app = guarded(&m);
        let (_, body) = create_claim(app.clone(), category, employee, "100000").await;
        let id = claim_id(&body);

        // Submit (unwired seam ⇒ no link) → submitted/submitted.
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id}/submit"),
                String::new()
            )
            .await,
            StatusCode::OK
        );

        // PATCH on a submitted claim → 409: the ROW's state decides, not the payload.
        let s = req(
            app.clone(),
            "PATCH",
            &format!("/expenses/{id}"),
            r#"{"description":"edited after submit"}"#.to_string(),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "row-truth edit guard");

        // Double submit → 409.
        let s = req(
            app,
            "POST",
            &format!("/expenses/{id}/submit"),
            String::new(),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "double submit");

        let pair = state_pair(&pool, id).await;
        pair
    })
    .await;

    assert_eq!(
        pair,
        ("submitted".into(), "submitted".into()),
        "legal pair after submit"
    );
}

// ─── EXP-5: unlinked direct-approval path + sticky refuse ─────────────────────

#[tokio::test]
async fn unlinked_claim_approves_directly_then_refuses_sticky() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "TOLS").await;
    let employee = Uuid::new_v4();

    scoped(&pool, company, async {
        let app = guarded(&m);

        // Claim B: the refuse path (refused is sticky — keep it off the approved path).
        let (_, body_b) = create_claim(app.clone(), category, Uuid::new_v4(), "75000").await;
        let id_b = claim_id(&body_b);
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id_b}/submit"),
                String::new()
            )
            .await,
            StatusCode::OK
        );
        let s = req(
            app.clone(),
            "POST",
            &format!("/expenses/{id_b}/refuse"),
            r#"{"reason":"out of policy"}"#.to_string(),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "refuse submitted claim");
        assert_eq!(
            state_pair(&pool, id_b).await,
            ("refused".into(), "refused".into()),
            "refused pair"
        );

        // Refuse is sticky: a second refuse matches zero guard rows → 409.
        let s = req(
            app.clone(),
            "POST",
            &format!("/expenses/{id_b}/refuse"),
            String::new(),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "sticky refuse");

        // The unlinked claim (unwired seam ⇒ no approval_request_id) approves directly — the
        // manager-verb semantics every unwired deployment of the family ships with.
        let (_, body) = create_claim(app.clone(), category, employee, "60000").await;
        let id = claim_id(&body);
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id}/submit"),
                String::new()
            )
            .await,
            StatusCode::OK
        );
        let s = req(
            app,
            "POST",
            &format!("/expenses/{id}/approve"),
            String::new(),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "unlinked approve");
        assert_eq!(
            state_pair(&pool, id).await,
            ("approved".into(), "approved".into()),
            "approved pair"
        );
    })
    .await;
}

// ─── EXP-6: wired submit files + links; TR2 honors the verdict ────────────────

#[tokio::test]
async fn wired_submit_files_and_links_and_tr2_honors_verdicts() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "INVT").await;
    let employee = Uuid::new_v4();

    scoped(&pool, company, async {
        // Approved verdict: submit links, approve passes.
        let svc =
            ExpensesWriteService::new(pool.clone()).with_approvals(Arc::new(FakeApprovals {
                verdict: ApprovalVerdict::Approved,
            }));
        let claim = svc
            .create_expense(
                new_claim(category, employee, Decimal::new(123_000, 2), ExpensePaymentMode::OwnAccount),
                None,
            )
            .await
            .unwrap();

        let submitted = svc
            .submit_expense(claim.id, Some("please".into()), None)
            .await
            .unwrap();
        assert!(
            submitted.approval_request_id.is_some(),
            "wired port ⇒ linked at submit"
        );

        let approved = svc.approve_expense(claim.id, None).await.unwrap();
        assert!(
            matches!(approved.state, ExpenseState::Approved),
            "Approved verdict admits the verb"
        );

        // Pending verdict: submit links, approve is REFUSED — fail-closed, never a bypass.
        let svc_pending = ExpensesWriteService::new(pool.clone()).with_approvals(Arc::new(
            FakeApprovals {
                verdict: ApprovalVerdict::Pending,
            },
        ));
        let claim2 = svc_pending
            .create_expense(
                new_claim(category, Uuid::new_v4(), Decimal::new(45_000, 2), ExpensePaymentMode::OwnAccount),
                None,
            )
            .await
            .unwrap();
        svc_pending
            .submit_expense(claim2.id, None, None)
            .await
            .unwrap();
        let err = svc_pending
            .approve_expense(claim2.id, None)
            .await
            .unwrap_err();
        assert_eq!(err.http_status(), 409, "Pending verdict fails closed: {err}");
        assert_eq!(err.code(), "approval_not_granted");
        assert_eq!(
            state_pair(&pool, claim2.id).await,
            ("submitted".into(), "submitted".into()),
            "row stays submitted after the refused grant"
        );
    })
    .await;
}

// ─── EXP-7: approve fails CLOSED on a linked claim when the port is unwired ───

#[tokio::test]
async fn linked_claim_never_bypasses_the_engine() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "LKDN").await;

    scoped(&pool, company, async {
        let app = guarded(&m);
        let (_, body) = create_claim(app.clone(), category, Uuid::new_v4(), "99000").await;
        let id = claim_id(&body);
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id}/submit"),
                String::new()
            )
            .await,
            StatusCode::OK
        );

        // Out-of-band linkage (the PATCH/backfill scenario): the row now carries a link, but the
        // deployment's port is unwired. Approve MUST fail closed — 409, never a bypass.
        let link = Uuid::new_v4();
        sqlx::query("UPDATE expenses.expenses SET approval_request_id = $1 WHERE id = $2")
            .bind(link)
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();

        let (status, body) = req_full(
            app,
            "POST",
            &format!("/expenses/{id}/approve"),
            String::new(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "fail-closed approve: {body}");
        assert!(body.contains("approval_not_granted"), "stable code: {body}");
    })
    .await;
}

// ─── EXP-8: the DB is the arbiter — illegal pairs rejected by the CHECK ───────

#[tokio::test]
async fn db_check_rejects_illegal_state_pairs() {
    let pool = pool().await;
    let category = seed_category(&pool, "CHCK").await;

    let result = sqlx::query(
        r#"INSERT INTO expenses.expenses
               (id, employee_id, category_id, expense_date, description,
                amount_total, currency, payment_mode, approval_state, state, metadata)
           VALUES ($1, $2, $3, '2026-08-10', 'illegal pair', 1, 'IDR',
                   'own_account', 'approved', 'draft', '{}'::jsonb)"#,
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(category)
    .execute(&pool)
    .await;
    let err = match result { Err(e) => e, Ok(_) => panic!("illegal (approved, draft) pair must be rejected") };
    let constraint = err
        .as_database_error()
        .and_then(|d| d.constraint())
        .unwrap_or_default();
    assert!(
        constraint.contains("expenses_state_pair_legal"),
        "the pair CHECK fired: {constraint} ({err})"
    );

    // The non-neg CHECK holds the same way.
    let result = sqlx::query(
        r#"INSERT INTO expenses.expenses
               (id, employee_id, category_id, expense_date, description,
                amount_total, currency, payment_mode, approval_state, state, metadata)
           VALUES ($1, $2, $3, '2026-08-10', 'negative', -1, 'IDR',
                   'own_account', 'draft', 'draft', '{}'::jsonb)"#,
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(category)
    .execute(&pool)
    .await;
    let err = match result { Err(e) => e, Ok(_) => panic!("negative amount must be rejected at the DB") };
    let constraint = err
        .as_database_error()
        .and_then(|d| d.constraint())
        .unwrap_or_default();
    assert!(
        constraint.contains("amount_total_nonneg"),
        "the amount CHECK fired: {constraint}"
    );
}

// ─── EXP-9: post through a wired sink — balanced envelope + GL stamps ─────────

#[tokio::test]
async fn post_builds_a_balanced_envelope_with_tax_overlay() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "POST").await;
    let employee = Uuid::new_v4();

    let sink = Arc::new(RecordingGlSink::default());
    let svc = ExpensesWriteService::new(pool.clone()).with_gl_sink(sink.clone());

    scoped(&pool, company, async {
        // 10,000.00 gross.
        let claim = svc
            .create_expense(
                new_claim(category, employee, Decimal::new(1_000_000, 2), ExpensePaymentMode::OwnAccount),
                None,
            )
            .await
            .unwrap();

        // Tax overlay (pre-computed — billing's removable-overlay pattern): input PPN 110.00,
        // withholding PPh 50.00.
        svc.set_tax_lines(
            claim.id,
            vec![
                TaxLineWrite {
                    basis: "input".into(),
                    account_id: Uuid::new_v4(),
                    description: Some("PPN".into()),
                    rate: Decimal::new(11, 0),
                    tax_amount: Decimal::new(11_000, 2),
                },
                TaxLineWrite {
                    basis: "withholding".into(),
                    account_id: Uuid::new_v4(),
                    description: Some("PPh 21".into()),
                    rate: Decimal::new(5, 0),
                    tax_amount: Decimal::new(5_000, 2),
                },
            ],
            None,
        )
        .await
        .unwrap();

        svc.submit_expense(claim.id, None, None).await.unwrap();
        svc.approve_expense(claim.id, None).await.unwrap();

        let posted = svc
            .post_expense(claim.id, accounts(), None)
            .await
            .unwrap();
        assert!(matches!(posted.state, ExpenseState::Posted), "state → posted");
        assert!(
            posted.journal_id.is_some() && posted.accounting_post_id.is_some(),
            "GL ack stamped on the row"
        );

        // Dr(expense 10,000 + input 110) == Cr(payable 10,060 + withholding 50).
        let envelope = sink.last.lock().unwrap().clone().expect("sink saw the envelope");
        assert_eq!(envelope.source_type, "expense");
        assert_eq!(envelope.source_id, claim.id);
        assert_eq!(
            envelope.idempotency_key,
            format!("expense:{company}:{}", claim.id)
        );
        assert!(
            envelope.is_balanced(),
            "balanced with tax overlay: {:?}",
            envelope.totals()
        );
        // 4 domain lines: expense Dr, input Dr, withholding Cr, payable Cr.
        assert_eq!(
            envelope.lines.len(),
            4,
            "tax overlay rides the envelope: {:?}",
            envelope.lines
        );
    })
    .await;
}

// ─── EXP-10: post unwired fails closed with the stable code; double post 409 ──

#[tokio::test]
async fn post_unwired_fails_closed_and_double_post_conflicts() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "UNWD").await;

    scoped(&pool, company, async {
        let app = guarded(&m);
        let (_, body) = create_claim(app.clone(), category, Uuid::new_v4(), "42000").await;
        let id = claim_id(&body);
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id}/submit"),
                String::new()
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id}/approve"),
                String::new()
            )
            .await,
            StatusCode::OK
        );

        // Accounting lands later — the unwired sink MUST refuse, stable code, row stays approved.
        let (status, body) = req_full(
            app,
            "POST",
            &format!("/expenses/{id}/post"),
            format!(
                r#"{{"postAccounts":{{"employeePayableAccountId":"{}","bankAccountId":"{}"}}}}"#,
                Uuid::new_v4(),
                Uuid::new_v4()
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unwired post: {body}");
        assert!(body.contains("gl_seam_unwired"), "stable seam code: {body}");
        let state: String = one(
            &pool,
            format!("SELECT state::text FROM expenses.expenses WHERE id = '{id}'"),
        )
        .await;
        assert_eq!(state, "approved", "row stays retryable");

        // Wire a sink at the SERVICE level (the composition point), post, then double-post → 409.
        let svc = ExpensesWriteService::new(pool.clone())
            .with_gl_sink(Arc::new(RecordingGlSink::default()));
        svc.post_expense(id, accounts(), None).await.unwrap();
        let err = svc
            .post_expense(id, accounts(), None)
            .await
            .unwrap_err();
        assert_eq!(err.http_status(), 409, "double post conflicts: {err}");
        assert_eq!(err.code(), "already_posted");
    })
    .await;
}

// ─── EXP-11: the strip posture — tenant axis gone, fence flags stay armed ─────
//
// The module ships no tenant axis (ADR-0029): no company_id column, no legacy company-isolation
// policies, no company-leading indexes. Row-level security stays ENABLED + FORCED — the
// composing service's tenancy decorator owns the policies that make it bite. Probed from
// below (SET ROLE to a plain non-superuser, whom RLS does bind): with no policy admitting
// it, the role sees ZERO rows no matter what legacy variable is set — default deny.

#[tokio::test]
async fn tenancy_posture_holds_for_non_superuser() {
    let pool = pool().await;
    let category = seed_category(&pool, "FENC").await;

    // Seed one claim through the owner pool so the default-deny assertion below is
    // meaningful (a row exists; the probe role just cannot see it).
    sqlx::query(
        r#"INSERT INTO expenses.expenses
               (id, employee_id, category_id, expense_date, description, amount_total,
                currency, payment_mode, approval_state, state, metadata)
           VALUES ($1, $2, $3, '2026-08-10', 'posture probe', 1, 'IDR',
                   'own_account', 'draft', 'draft', '{}'::jsonb)"#,
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(category)
    .execute(&pool)
    .await
    .unwrap();

    // ── schema posture: the tenant axis is gone, the fence flags stay armed ──
    for table in ["expenses", "expense_categories", "expense_tax_lines"] {
        let company_col: bool = sqlx::query_scalar(
            r#"SELECT EXISTS (
                   SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'expenses' AND table_name = $1
                     AND column_name = 'company_id'
               )"#,
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!company_col, "expenses.{table} must not carry a company_id column");

        let legacy_policies: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM pg_policies
               WHERE schemaname = 'expenses' AND tablename = $1
                 AND policyname LIKE '%company_isolation'"#,
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            legacy_policies, 0,
            "expenses.{table} must not carry a legacy company-isolation policy"
        );

        let company_indexes: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM pg_indexes
               WHERE schemaname = 'expenses' AND tablename = $1
                 AND indexname LIKE '%company_id%'"#,
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(company_indexes, 0, "expenses.{table} must not carry company-leading indexes");

        let (rls_enabled, rls_forced): (bool, bool) = sqlx::query_as(
            r#"SELECT relrowsecurity, relforcerowsecurity
               FROM pg_class
               WHERE oid = to_regclass($1)"#,
        )
        .bind(format!("expenses.{table}"))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(rls_enabled, "expenses.{table} must keep row-level security ENABLED (the decorator owns the policies)");
        assert!(rls_forced, "expenses.{table} must keep row-level security FORCED (the decorator owns the policies)");
    }

    // ── default-deny, probed from below ─────────────────────────────────────

    // The probe role: non-superuser, minimal grants, idempotent (NOLOGIN — privileges from a
    // prior run make DROP ROLE refuse, so the family pattern creates-if-absent instead).
    // The two posture probes share the role: the advisory lock serializes their
    // create/grant/set-role windows so a fresh database cannot race two CREATE ROLEs.
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(814401)")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        r#"DO $$ BEGIN
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'expenses_probe_rls') THEN
                   CREATE ROLE expenses_probe_rls NOLOGIN;
               END IF;
           END $$"#,
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("GRANT USAGE ON SCHEMA expenses TO expenses_probe_rls")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("GRANT SELECT ON ALL TABLES IN SCHEMA expenses TO expenses_probe_rls")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("SET ROLE expenses_probe_rls")
        .execute(&mut *conn)
        .await
        .unwrap();

    // With no policy admitting it, the role sees nothing — even though the owner-seeded row
    // exists. Setting the legacy company variable resurrects nothing: no policy reads it
    // anymore (the decorator's org-scoped policies will, once composed). One explicit
    // transaction; a SET/RESET pairing is session-level, but keeping the read transactional
    // matches the family probe pattern.
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.company_id', $1, true)")
        .bind(Uuid::new_v4().to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM expenses.expenses")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0, "a role no policy admits sees zero rows, legacy variable or not");
    tx.rollback().await.unwrap();

    sqlx::query("RESET ROLE").execute(&mut *conn).await.unwrap();
    sqlx::query("SELECT pg_advisory_unlock(814401)")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    // The owner pool still sees its row — the denial above is the missing policy, not an
    // empty database.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM expenses.expenses")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(n >= 1, "the owner pool must still see the seeded claim");
}

// ─── EXP-12: settle — own_account reimburses; company_account + unwired refuse ─

#[tokio::test]
async fn settle_reimburses_own_account_only() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "STTL").await;
    let employee = Uuid::new_v4();
    let gross = Decimal::new(800_000, 2); // 8,000.00

    let payment_id = Uuid::new_v4();
    let svc = ExpensesWriteService::new(pool.clone())
        .with_gl_sink(Arc::new(RecordingGlSink::default()))
        .with_reimbursement(Arc::new(FixedReimbursement(payment_id)));

    scoped(&pool, company, async {
        // own_account: full lifecycle to done, payment ack stamped.
        let claim = svc
            .create_expense(
                new_claim(category, employee, gross, ExpensePaymentMode::OwnAccount),
                None,
            )
            .await
            .unwrap();
        svc.submit_expense(claim.id, None, None).await.unwrap();
        svc.approve_expense(claim.id, None).await.unwrap();
        svc.post_expense(claim.id, accounts(), None).await.unwrap();
        let settled = svc.settle_expense(claim.id, None).await.unwrap();
        assert!(matches!(settled.state, ExpenseState::Done), "state → done");
        assert_eq!(
            settled.reimbursement_id,
            Some(payment_id),
            "payment ack stamped"
        );

        // company_account: posted claims settle at the bank — settle refuses (409).
        let company_claim = svc
            .create_expense(
                new_claim(category, Uuid::new_v4(), gross, ExpensePaymentMode::CompanyAccount),
                None,
            )
            .await
            .unwrap();
        svc.submit_expense(company_claim.id, None, None).await.unwrap();
        svc.approve_expense(company_claim.id, None).await.unwrap();
        svc.post_expense(company_claim.id, accounts(), None).await.unwrap();
        let err = svc
            .settle_expense(company_claim.id, None)
            .await
            .unwrap_err();
        assert_eq!(err.http_status(), 409, "company_account settle: {err}");
        assert_eq!(err.code(), "not_reimbursable");

        // Unwired reimbursement seam fails closed: posted stays posted, never 'done'.
        let unwired = ExpensesWriteService::new(pool.clone())
            .with_gl_sink(Arc::new(RecordingGlSink::default()));
        let claim3 = unwired
            .create_expense(
                new_claim(category, Uuid::new_v4(), gross, ExpensePaymentMode::OwnAccount),
                None,
            )
            .await
            .unwrap();
        unwired
            .submit_expense(claim3.id, None, None)
            .await
            .unwrap();
        unwired
            .approve_expense(claim3.id, None)
            .await
            .unwrap();
        unwired
            .post_expense(claim3.id, accounts(), None)
            .await
            .unwrap();
        let err = unwired
            .settle_expense(claim3.id, None)
            .await
            .unwrap_err();
        assert_eq!(err.http_status(), 422, "unwired settle fails closed: {err}");
        assert_eq!(err.code(), "reimbursement_seam_unwired");
        let state: String = one(
            &pool,
            format!("SELECT state::text FROM expenses.expenses WHERE id = '{}'", claim3.id),
        )
        .await;
        assert_eq!(state, "posted", "row stays retryable");
    })
    .await;
}

// ─── EXP-13: receipts — attach on open claims, frozen once decided ────────────

#[tokio::test]
async fn receipt_attach_on_open_claims_only() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "RCPT").await;

    scoped(&pool, company, async {
        let app = guarded(&m);
        let (_, body) = create_claim(app.clone(), category, Uuid::new_v4(), "8000").await;
        let id = claim_id(&body);

        let file = Uuid::new_v4();
        let s = req(
            app.clone(),
            "POST",
            &format!("/expenses/{id}/receipt"),
            format!(r#"{{"receiptFileId":"{file}"}}"#),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "attach on draft");
        let rid: Uuid = one(
            &pool,
            format!("SELECT receipt_file_id FROM expenses.expenses WHERE id = '{id}'"),
        )
        .await;
        assert_eq!(rid, file, "receipt stamped");

        // Once refused, the evidence set is frozen: the open-claims guard matches zero rows.
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id}/submit"),
                String::new()
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id}/refuse"),
                String::new()
            )
            .await,
            StatusCode::OK
        );
        let s = req(
            app,
            "POST",
            &format!("/expenses/{id}/receipt"),
            format!(r#"{{"receiptFileId":"{file}"}}"#),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "decided claim: guard matches zero rows");
    })
    .await;
}

// ─── EXP-14: the report projection — grouped totals, no sheet entity ──────────

#[tokio::test]
async fn report_projection_groups_by_category_and_state() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let cat_a = seed_category(&pool, "RP-A").await;
    let cat_b = seed_category(&pool, "RP-B").await;
    let employee = Uuid::new_v4();

    scoped(&pool, company, async {
        let app = guarded(&m);
        let (_, a1) = create_claim(app.clone(), cat_a, employee, "10000").await;
        let (_, _a2) = create_claim(app.clone(), cat_a, employee, "25000").await;
        let (_, _b1) = create_claim(app.clone(), cat_b, employee, "5000").await;
        let id_a1 = claim_id(&a1);

        // Advance one RP-A claim to submitted so the projection shows BOTH states.
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id_a1}/submit"),
                String::new()
            )
            .await,
            StatusCode::OK
        );

        let (status, body) = req_full(
            app.clone(),
            "GET",
            &format!("/expenses/report?from=2026-01-01&to=2026-12-31&employeeId={employee}"),
            String::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "report: {body}");

        let rows: Vec<serde_json::Value> =
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone();
        assert_eq!(
            rows.len(),
            3,
            "three groups (RP-A draft, RP-A submitted, RP-B draft): {rows:?}"
        );

        let rp_a_draft = rows
            .iter()
            .find(|r| r["categoryCode"] == "RP-A" && r["state"] == "draft")
            .expect("RP-A draft group");
        assert_eq!(rp_a_draft["lineCount"], serde_json::json!(1), "grouping counts rows");
        let total: Decimal =
            serde_json::from_value::<Decimal>(rp_a_draft["amountTotal"].clone()).unwrap();
        assert_eq!(total, Decimal::new(25_000, 0), "grouping sums totals");

        // `from > to` never runs — a 422, not a 500.
        let (status, body) = req_full(
            app,
            "GET",
            "/expenses/report?from=2026-12-31&to=2026-01-01",
            String::new(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "bad range: {body}");
    })
    .await;
}

// ─── EXP-15: no caller identity — no surface ──────────────────────────────────

#[tokio::test]
async fn unauthenticated_is_rejected() {
    let pool = pool().await;
    let m = module(&pool).await;
    // The raw router WITHOUT the caller extension a composing service's org auth middleware
    // inserts: the `OrgContext` extractor rejects the request before any handler runs.
    let app = create_guarded_expenses_routes(&m);
    let (status, _) = req_full(
        app,
        "GET",
        "/expenses/report?from=2026-01-01&to=2026-12-31",
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ─── EXP-16: the tax overlay is FROZEN at submit (council F1) ─────────────────
//
// The overlay feeds the gross: `amount_total + input` is both the GL credit and the settle
// amount — a line landing after the approval verdict would inflate what the manager signed.
// The draft-only guard lives IN THE SQL (parent's (draft, draft) pair on every overlay
// statement), so even a set_tax_lines racing a commit lands zero rows → 409, and the serial
// case is pinned here at the route level.

#[tokio::test]
async fn tax_overlay_is_frozen_at_submit() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "FRZ").await;
    let employee = Uuid::new_v4();

    scoped(&pool, company, async {
        let app = guarded(&m);
        let (_, body) = create_claim(app.clone(), category, employee, "50000").await;
        let id = claim_id(&body);

        // Draft: the overlay is writable.
        let s = req(
            app.clone(),
            "PUT",
            &format!("/expenses/{id}/tax-lines"),
            format!(
                r#"{{"lines":[{{"basis":"input","accountId":"{}","taxAmount":5500,"rate":11}}]}}"#,
                Uuid::new_v4()
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "set overlay on draft");

        // Submit freezes the evidence set the approver will rule on.
        assert_eq!(
            req(
                app.clone(),
                "POST",
                &format!("/expenses/{id}/submit"),
                String::new()
            )
            .await,
            StatusCode::OK
        );

        // Post-submit replace → 409; the row-truth guard matched zero rows.
        let (status, body) = req_full(
            app,
            "PUT",
            &format!("/expenses/{id}/tax-lines"),
            format!(
                r#"{{"lines":[{{"basis":"input","accountId":"{}","taxAmount":999999,"rate":11}}]}}"#,
                Uuid::new_v4()
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "overlay frozen at submit: {body}");
        assert!(body.contains("not_draft"), "stable code: {body}");

        // And the refused replace left the frozen lines exactly as they were.
        let n: i64 = one(
            &pool,
            format!(
                "SELECT count(*) FROM expenses.expense_tax_lines WHERE expense_id = '{id}' \
                 AND (metadata->>'deleted_at') IS NULL"
            ),
        )
        .await;
        assert_eq!(n, 1, "the original overlay row survives untouched");
        let amt: Decimal = one(
            &pool,
            format!(
                "SELECT tax_amount FROM expenses.expense_tax_lines WHERE expense_id = '{id}' \
                 AND (metadata->>'deleted_at') IS NULL"
            ),
        )
        .await;
        assert_eq!(amt, Decimal::new(5_500, 0), "no inflated line landed");
    })
    .await;
}

// ─── EXP-17: the write posture — a policyless role cannot write, the owner can ─
//
// The read side is pinned by EXP-11; this is the write leg. With the tenant axis stripped and
// no decorator installed yet, the tables carry RLS ENABLE+FORCE and ZERO policies — so a
// plain non-superuser's INSERT hits default deny (nothing admits it), while the module's
// owner-role writes go straight through. The composing decorator's policies are what grant a
// scoped app role its writes at composition time.

#[tokio::test]
async fn write_posture_holds_for_non_superuser() {
    let pool = pool().await;

    // Same probe role and same advisory window as the read-posture probe above —
    // the lock keeps the two SET ROLE windows from racing a fresh database's
    // create-if-absent.
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(814401)")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        r#"DO $$ BEGIN
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'expenses_probe_rls') THEN
                   CREATE ROLE expenses_probe_rls NOLOGIN;
               END IF;
           END $$"#,
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("GRANT USAGE ON SCHEMA expenses TO expenses_probe_rls")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "GRANT SELECT, INSERT, UPDATE ON ALL TABLES IN SCHEMA expenses TO expenses_probe_rls",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("SET ROLE expenses_probe_rls")
        .execute(&mut *conn)
        .await
        .unwrap();

    // The role holds the GRANTs but no policy admits its write — default deny refuses the
    // INSERT even though the table carries no tenant column at all.
    let refused = sqlx::query(
        r#"INSERT INTO expenses.expense_categories
               (id, code, name, expense_account_id, metadata)
           VALUES ($1, 'FENCE-W', 'fence write probe', $2, '{}'::jsonb)"#,
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(&mut *conn)
    .await;
    let err = match refused {
        Err(e) => e,
        Ok(_) => panic!("a role no policy admits must not write (default deny)"),
    };
    assert!(
        err.as_database_error().is_some(),
        "policy denial, not a transport error: {err}"
    );

    sqlx::query("RESET ROLE").execute(&mut *conn).await.unwrap();
    sqlx::query("SELECT pg_advisory_unlock(814401)")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    // The owner role writes fine — no tenant axis on the module's own write path.
    let result = sqlx::query(
        r#"INSERT INTO expenses.expense_categories
               (id, code, name, expense_account_id, metadata)
           VALUES ($1, 'FENCE-O', 'owner write probe', $2, '{}'::jsonb)"#,
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    assert!(result.is_ok(), "the owner role writes without any tenant axis: {result:?}");
}

// ─── EXP-22: a raced draft edit inside the file window never links a stale filing ──

/// A port that mutates the draft out-of-band inside `file` — exactly the concurrent-edit
/// window between the service's read and the compare-and-set link write. The id it returns
/// is fixed so the orphan left behind is traceable (the convergence probe reuses the trick).
struct RacingApprovals {
    pool: PgPool,
    expense_id: Uuid,
    request_id: Uuid,
    race_once: Mutex<bool>,
}

#[async_trait::async_trait]
impl ApprovalFiling for RacingApprovals {
    async fn file(
        &self,
        _req: &ExpenseApprovalFilingRequest,
    ) -> Result<Uuid, ApprovalSeamError> {
        let should_race = *self.race_once.lock().unwrap();
        if should_race {
            *self.race_once.lock().unwrap() = false;
            sqlx::query("UPDATE expenses.expenses SET amount_total = amount_total + 1 WHERE id = $1")
                .bind(self.expense_id)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        Ok(self.request_id)
    }
    async fn status(&self, _id: Uuid) -> Result<ApprovalVerdict, ApprovalSeamError> {
        Ok(ApprovalVerdict::Approved)
    }
}

#[tokio::test]
async fn filing_payload_race_409s_and_does_not_link() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "RACE").await;
    let employee = Uuid::new_v4();

    scoped(&pool, company, async {
        let svc = ExpensesWriteService::new(pool.clone());
        let claim = svc
            .create_expense(
                new_claim(category, employee, Decimal::new(50_000, 2), ExpensePaymentMode::OwnAccount),
                None,
            )
            .await
            .unwrap();

        // The port edits the draft inside the file window: the filing describes the pre-edit
        // amount, the row now carries post-edit values.
        let svc = svc.with_approvals(Arc::new(RacingApprovals {
            pool: pool.clone(),
            expense_id: claim.id,
            request_id: Uuid::new_v4(),
            race_once: Mutex::new(true),
        }));
        let err = svc
            .submit_expense(claim.id, None, None)
            .await
            .unwrap_err();
        assert_eq!(err.http_status(), 409, "raced payload ⇒ conflict: {err}");
        assert_eq!(err.code(), "not_draft");

        // The row was NEVER linked to the stale filing — still draft, no request id.
        assert_eq!(
            state_pair(&pool, claim.id).await,
            ("draft".into(), "draft".into()),
            "the row keeps its draft pair after the refused link"
        );
        let linked: Option<String> = one(
            &pool,
            format!("SELECT approval_request_id::text FROM expenses.expenses WHERE id = '{}'", claim.id),
        )
        .await;
        assert_eq!(linked, None, "no approval link on a raced-out submit");
    })
    .await;
}

// ─── EXP-23: the retry converges — same live request, now on the fresh row ─────

#[tokio::test]
async fn submit_retry_converges_same_request() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "CNVG").await;
    let employee = Uuid::new_v4();

    let request_id = Uuid::new_v4(); // the engine's one live request for this resource
    let svc = ExpensesWriteService::new(pool.clone());

    scoped(&pool, company, async {
        let claim = svc
            .create_expense(
                new_claim(category, employee, Decimal::new(75_000, 2), ExpensePaymentMode::OwnAccount),
                None,
            )
            .await
            .unwrap();

        let racing = Arc::new(RacingApprovals {
            pool: pool.clone(),
            expense_id: claim.id,
            request_id,
            race_once: Mutex::new(true),
        });
        let svc = svc.with_approvals(racing.clone());

        // Attempt 1: the race 409s, leaving an orphaned-but-live filing in the engine.
        assert_eq!(
            svc.submit_expense(claim.id, None, None)
                .await
                .unwrap_err()
                .http_status(),
            409
        );

        // Attempt 2 (idempotent file ⇒ the same live request id): the CAS now matches the
        // fresh row and the link lands on the SAME request the orphan was filed under.
        let submitted = svc
            .submit_expense(claim.id, None, None)
            .await
            .unwrap();
        assert_eq!(
            submitted.approval_request_id,
            Some(request_id),
            "retry links the one live request — no duplicate filing"
        );
        assert_eq!(
            state_pair(&pool, claim.id).await,
            ("submitted".into(), "submitted".into())
        );
        assert!(!*racing.race_once.lock().unwrap());
    })
    .await;
}

// ─── EXP-24: the module-built service can be armed post-build (the compose path) ──

#[tokio::test]
async fn module_built_service_arms_approvals_after_build() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, "ARMD").await;

    // The module was built unwired (family default); the composing app arms it afterwards —
    // the exact sequence a service that composes this module performs at startup.
    m.set_expenses_approvals(Arc::new(FakeApprovals {
        verdict: ApprovalVerdict::Approved,
    }));

    scoped(&pool, company, async {
        let app = guarded(&m);
        let (status, body) = create_claim(app.clone(), category, Uuid::new_v4(), "12000").await;
        assert_eq!(status, StatusCode::CREATED, "create: {body}");
        let id = claim_id(&body);

        assert_eq!(
            req(app, "POST", &format!("/expenses/{id}/submit"), String::new()).await,
            StatusCode::OK
        );
        let linked: Option<String> = one(
            &pool,
            format!("SELECT approval_request_id::text FROM expenses.expenses WHERE id = '{id}'"),
        )
        .await;
        assert!(linked.is_some(), "armed-after-build port ⇒ submit links: {linked:?}");
    })
    .await;
}

// ─── EXP-25: the seams' legacy twin fails closed with no ambient scope ────────
//
// The write service never guesses a company. submit/post/settle feed consumer seams that
// still key on one (approvals / GL / reimbursement); with NO ambient org scope bound — a
// posture production never runs, but a bare module deployment could — the verb fails closed
// with the stable `no_org_scope` code instead of inventing a tenant key.

#[tokio::test]
async fn legacy_twin_fails_closed_without_a_scope() {
    let pool = pool().await;
    let category = seed_category(&pool, "NOOR").await;

    let svc = ExpensesWriteService::new(pool.clone()).with_approvals(Arc::new(FakeApprovals {
        verdict: ApprovalVerdict::Approved,
    }));

    // Create carries no tenant axis — it works scope-free.
    let claim = svc
        .create_expense(
            new_claim(category, Uuid::new_v4(), Decimal::new(10_000, 2), ExpensePaymentMode::OwnAccount),
            None,
        )
        .await
        .unwrap();

    // A WIRED submit needs the filing's legacy company twin; none is bound ⇒ fail closed.
    let err = svc.submit_expense(claim.id, None, None).await.unwrap_err();
    assert_eq!(err.http_status(), 500, "no scope to source the twin from: {err}");
    assert_eq!(err.code(), "no_org_scope");
}
