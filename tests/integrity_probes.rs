//! Integrity probes — route-level (Wave 1 P3, H-4). The guarded composition locks generic
//! mutation, the two-field lifecycle holds its legal pairs, the DB CHECK is the arbiter, the
//! three seams fail closed, and the company fence holds cross-tenant.
//!
//! Every request runs behind the REAL `company_auth` middleware with a minted HS256 token —
//! the same mounting a composing service uses in production (ADR-0008; the party/attendance
//! probe-suite harness pattern). The DB runs the strict fence (RLS ENABLE+FORCE on every
//! expenses table) — but this suite connects as the DB owner (a superuser, whom RLS can
//! never bind; the fence migration says the app connects as a non-superuser). So verbs carry
//! their company predicate in SQL (belt-and-braces — cross-tenant is a 404 even here), raw
//! assertion SQL runs inside `company_scope::with_company_scope` (re-exported at the crate
//! root), and the FENCE itself is pinned by EXP-11 under `SET ROLE` to a plain non-superuser
//! (the serpa_app posture): unbound sees zero rows, bound sees exactly its company's rows.
//!
//! The module's own write service is default-UNWIRED (the family posture), so route-level
//! probes assert the fail-closed contract; the wired-success paths (post → posted with a
//! balanced envelope, settle → done, submit → linked) run at the SERVICE level on a
//! separately-constructed `ExpensesWriteService::new(pool).with_…(fake)` — exactly the shape
//! a composing service (serpa, W2/P6) uses to wire the adapters.
//!
//! DB: DATABASE_URL wins, else the module's local test DB (`backbone_expenses_test` on the
//! metaphora dev postgres, migrated). Fresh random company ids per test so parallel runs
//! never collide.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use rust_decimal::Decimal;
use sqlx::{Acquire, PgPool};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;

use backbone_auth::company::{company_auth, CompanyVerifier};
use backbone_expenses::{
    company_scope, create_guarded_expenses_routes, AccountingPostEnvelope, ApprovalFiling,
    ApprovalSeamError, ApprovalVerdict, ExpenseApprovalFilingRequest, ExpensePaymentMode,
    ExpenseState, ExpensesModule, ExpensesWriteService, GlPostAck, GlPostSink, GlPostRejected,
    NewExpense, PostAccounts, ReimbursementAck, ReimbursementRequest, ReimbursementSeamError,
    ReimbursementSink, TaxLineWrite,
};

const SECRET: &[u8] = b"expenses-integrity-probe-secret";

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

fn token_for(company: Uuid) -> String {
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize
        + 3600;
    let claims = serde_json::json!({"sub": "integrity-probe", "company_id": company, "exp": exp});
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(SECRET),
    )
    .unwrap()
}

async fn req(app: axum::Router, method: &str, uri: &str, token: &str, body: String) -> StatusCode {
    req_full(app, method, uri, Some(token), body).await.0
}

/// Status + body — for probes that must pin the stable error code, not just the status.
async fn req_full(
    app: axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: String,
) -> (StatusCode, String) {
    let app = app.route_layer(from_fn_with_state(
        CompanyVerifier::hs256(SECRET),
        company_auth,
    ));
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let r = b.body(Body::from(body)).unwrap();
    let resp = app.oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// Seed a live category and return its id (fresh ids per test, so no clashes).
async fn seed_category(pool: &PgPool, company: Uuid, code: &str) -> Uuid {
    let id = Uuid::new_v4();
    let account = Uuid::new_v4();
    company_scope::with_company_scope(Some(company), async {
        sqlx::query(
            r#"INSERT INTO expenses.expense_categories (id, company_id, code, name, expense_account_id, metadata)
               VALUES ($1, $2, $3, $4, $5, '{}'::jsonb)"#,
        )
        .bind(id)
        .bind(company)
        .bind(code)
        .bind(format!("category {code}"))
        .bind(account)
        .execute(pool)
        .await
        .unwrap();
    })
    .await;
    id
}

async fn create_claim(
    app: axum::Router,
    t: &str,
    _company: Uuid,
    category: Uuid,
    employee: Uuid,
    amount: &str,
) -> (StatusCode, String) {
    let body = format!(
        r#"{{"employeeId":"{employee}","categoryId":"{category}","expenseDate":"2026-08-10","description":"taxi to client","amountTotal":{amount}}}"#
    );
    req_full(app, "POST", "/expenses", Some(t), body).await
}

fn claim_id(body: &str) -> Uuid {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// Scoped scalar read for assertions — binds `app.company_id` the way the request scope does
/// so the FORCE-fenced tables answer under RLS (an unbound connection sees 0 rows by design).
async fn scoped_one<T>(pool: &PgPool, company: Uuid, sql: String) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres>
        + sqlx::Type<sqlx::Postgres>
        + Send
        + Sync
        + Unpin,
{
    company_scope::with_company_scope(Some(company), async move {
        sqlx::query_scalar::<_, T>(&sql)
            .fetch_one(pool)
            .await
            .unwrap()
    })
    .await
}

async fn state_pair(pool: &PgPool, company: Uuid, id: Uuid) -> (String, String) {
    scoped_one(
        pool,
        company,
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
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "TRVL").await;
    let employee = Uuid::new_v4();
    let t = token_for(company);

    let (status, body) = create_claim(
        create_guarded_expenses_routes(&m),
        &t,
        company,
        category,
        employee,
        "250000",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    assert!(
        body.contains(r#""approvalState":"draft""#),
        "approval_state draft: {body}"
    );
    assert!(body.contains(r#""state":"draft""#), "state draft: {body}");
    assert!(body.contains(r#""currency":"IDR""#), "IDR default: {body}");

    let n: i64 = scoped_one(
        &pool,
        company,
        format!("SELECT count(*) FROM expenses.expenses WHERE employee_id = '{employee}'"),
    )
    .await;
    assert_eq!(n, 1, "exactly one claim row");
}

// ─── EXP-2: cross-tenant — another company's claim is a 404, never data ───────

#[tokio::test]
async fn cross_tenant_claim_is_hidden() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company_a = Uuid::new_v4();
    let company_b = Uuid::new_v4();
    let category = seed_category(&pool, company_a, "MEAL").await;
    let employee = Uuid::new_v4();
    let t_a = token_for(company_a);
    let t_b = token_for(company_b);

    let (_, body) = create_claim(
        create_guarded_expenses_routes(&m),
        &t_a,
        company_a,
        category,
        employee,
        "50000",
    )
    .await;
    let id = claim_id(&body);

    // B cannot mutate it — every verb's SQL carries the company predicate, so the other
    // company's claim matches zero rows: 404, no oracle that it exists. (The generic GET
    // read path rides the DB fence instead of SQL — production asserts it as the app role;
    // the fence itself is pinned by EXP-11 below. Belt-and-braces, the fenced service read
    // also 404s here.)
    let app = create_guarded_expenses_routes(&m);
    assert_eq!(
        req(
            app,
            "POST",
            &format!("/expenses/{id}/submit"),
            &t_b,
            String::new()
        )
        .await,
        StatusCode::NOT_FOUND
    );
    let svc = ExpensesWriteService::new(pool.clone());
    assert!(
        svc.get_expense(company_b, id).await.is_err(),
        "fenced service read: other company's claim is invisible"
    );

    // And B's own category is invisible to A's claims (cross-tenant category → 404).
    let category_b = seed_category(&pool, company_b, "ONLY-B").await;
    let (status, body) = create_claim(
        create_guarded_expenses_routes(&m),
        &t_a,
        company_a,
        category_b,
        employee,
        "1000",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cross-tenant category: {body}");
}

// ─── EXP-3: input validation — negative amount 422, stable code ───────────────

#[tokio::test]
async fn negative_amount_is_refused() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "PRNT").await;
    let t = token_for(company);

    let (status, body) = create_claim(
        create_guarded_expenses_routes(&m),
        &t,
        company,
        category,
        Uuid::new_v4(),
        "-1",
    )
    .await;
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
    let category = seed_category(&pool, company, "FUEL").await;
    let employee = Uuid::new_v4();
    let t = token_for(company);
    let app = create_guarded_expenses_routes(&m);

    let (_, body) = create_claim(app.clone(), &t, company, category, employee, "100000").await;
    let id = claim_id(&body);

    // Submit (unwired seam ⇒ no link) → submitted/submitted.
    assert_eq!(
        req(
            app.clone(),
            "POST",
            &format!("/expenses/{id}/submit"),
            &t,
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
        &t,
        r#"{"description":"edited after submit"}"#.to_string(),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "row-truth edit guard");

    // Double submit → 409.
    let s = req(
        app,
        "POST",
        &format!("/expenses/{id}/submit"),
        &t,
        String::new(),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "double submit");

    assert_eq!(
        state_pair(&pool, company, id).await,
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
    let category = seed_category(&pool, company, "TOLS").await;
    let employee = Uuid::new_v4();
    let t = token_for(company);
    let app = create_guarded_expenses_routes(&m);

    // Claim B: the refuse path (refused is sticky — keep it off the approved path).
    let (_, body_b) = create_claim(app.clone(), &t, company, category, Uuid::new_v4(), "75000").await;
    let id_b = claim_id(&body_b);
    assert_eq!(
        req(
            app.clone(),
            "POST",
            &format!("/expenses/{id_b}/submit"),
            &t,
            String::new()
        )
        .await,
        StatusCode::OK
    );
    let s = req(
        app.clone(),
        "POST",
        &format!("/expenses/{id_b}/refuse"),
        &t,
        r#"{"reason":"out of policy"}"#.to_string(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "refuse submitted claim");
    assert_eq!(
        state_pair(&pool, company, id_b).await,
        ("refused".into(), "refused".into()),
        "refused pair"
    );

    // Refuse is sticky: a second refuse matches zero guard rows → 409.
    let s = req(
        app.clone(),
        "POST",
        &format!("/expenses/{id_b}/refuse"),
        &t,
        String::new(),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "sticky refuse");

    // The unlinked claim (unwired seam ⇒ no approval_request_id) approves directly — the
    // manager-verb semantics every unwired deployment of the family ships with.
    let (_, body) = create_claim(app.clone(), &t, company, category, employee, "60000").await;
    let id = claim_id(&body);
    assert_eq!(
        req(
            app.clone(),
            "POST",
            &format!("/expenses/{id}/submit"),
            &t,
            String::new()
        )
        .await,
        StatusCode::OK
    );
    let s = req(
        app,
        "POST",
        &format!("/expenses/{id}/approve"),
        &t,
        String::new(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "unlinked approve");
    assert_eq!(
        state_pair(&pool, company, id).await,
        ("approved".into(), "approved".into()),
        "approved pair"
    );
}

// ─── EXP-6: wired submit files + links; TR2 honors the verdict ────────────────

#[tokio::test]
async fn wired_submit_files_and_links_and_tr2_honors_verdicts() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "INVT").await;
    let employee = Uuid::new_v4();

    // Approved verdict: submit links, approve passes.
    let svc = ExpensesWriteService::new(pool.clone()).with_approvals(Arc::new(FakeApprovals {
        verdict: ApprovalVerdict::Approved,
    }));
    let claim = svc
        .create_expense(
            company,
            new_claim(category, employee, Decimal::new(123_000, 2), ExpensePaymentMode::OwnAccount),
            None,
        )
        .await
        .unwrap();

    let submitted = svc
        .submit_expense(company, claim.id, Some("please".into()), None)
        .await
        .unwrap();
    assert!(
        submitted.approval_request_id.is_some(),
        "wired port ⇒ linked at submit"
    );

    let approved = svc.approve_expense(company, claim.id, None).await.unwrap();
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
            company,
            new_claim(category, Uuid::new_v4(), Decimal::new(45_000, 2), ExpensePaymentMode::OwnAccount),
            None,
        )
        .await
        .unwrap();
    svc_pending
        .submit_expense(company, claim2.id, None, None)
        .await
        .unwrap();
    let err = svc_pending
        .approve_expense(company, claim2.id, None)
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 409, "Pending verdict fails closed: {err}");
    assert_eq!(err.code(), "approval_not_granted");
    assert_eq!(
        state_pair(&pool, company, claim2.id).await,
        ("submitted".into(), "submitted".into()),
        "row stays submitted after the refused grant"
    );
}

// ─── EXP-7: approve fails CLOSED on a linked claim when the port is unwired ───

#[tokio::test]
async fn linked_claim_never_bypasses_the_engine() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "LKDN").await;
    let t = token_for(company);
    let app = create_guarded_expenses_routes(&m);

    let (_, body) = create_claim(app.clone(), &t, company, category, Uuid::new_v4(), "99000").await;
    let id = claim_id(&body);
    assert_eq!(
        req(
            app.clone(),
            "POST",
            &format!("/expenses/{id}/submit"),
            &t,
            String::new()
        )
        .await,
        StatusCode::OK
    );

    // Out-of-band linkage (the PATCH/backfill scenario): the row now carries a link, but the
    // deployment's port is unwired. Approve MUST fail closed — 409, never a bypass.
    let link = Uuid::new_v4();
    company_scope::with_company_scope(Some(company), async {
        sqlx::query("UPDATE expenses.expenses SET approval_request_id = $1 WHERE id = $2")
            .bind(link)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    })
    .await;

    let (status, body) = req_full(
        app,
        "POST",
        &format!("/expenses/{id}/approve"),
        Some(&t),
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "fail-closed approve: {body}");
    assert!(body.contains("approval_not_granted"), "stable code: {body}");
}

// ─── EXP-8: the DB is the arbiter — illegal pairs rejected by the CHECK ───────

#[tokio::test]
async fn db_check_rejects_illegal_state_pairs() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "CHCK").await;

    let result = company_scope::with_company_scope(Some(company), async {
        sqlx::query(
            r#"INSERT INTO expenses.expenses
                   (id, company_id, employee_id, category_id, expense_date, description,
                    amount_total, currency, payment_mode, approval_state, state, metadata)
               VALUES ($1, $2, $3, $4, '2026-08-10', 'illegal pair', 1, 'IDR',
                       'own_account', 'approved', 'draft', '{}'::jsonb)"#,
        )
        .bind(Uuid::new_v4())
        .bind(company)
        .bind(Uuid::new_v4())
        .bind(category)
        .execute(&pool)
        .await
    })
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
    let result = company_scope::with_company_scope(Some(company), async {
        sqlx::query(
            r#"INSERT INTO expenses.expenses
                   (id, company_id, employee_id, category_id, expense_date, description,
                    amount_total, currency, payment_mode, approval_state, state, metadata)
               VALUES ($1, $2, $3, $4, '2026-08-10', 'negative', -1, 'IDR',
                       'own_account', 'draft', 'draft', '{}'::jsonb)"#,
        )
        .bind(Uuid::new_v4())
        .bind(company)
        .bind(Uuid::new_v4())
        .bind(category)
        .execute(&pool)
        .await
    })
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
    let category = seed_category(&pool, company, "POST").await;
    let employee = Uuid::new_v4();

    let sink = Arc::new(RecordingGlSink::default());
    let svc = ExpensesWriteService::new(pool.clone()).with_gl_sink(sink.clone());
    // 10,000.00 gross.
    let claim = svc
        .create_expense(
            company,
            new_claim(category, employee, Decimal::new(1_000_000, 2), ExpensePaymentMode::OwnAccount),
            None,
        )
        .await
        .unwrap();

    // Tax overlay (pre-computed — billing's removable-overlay pattern): input PPN 110.00,
    // withholding PPh 50.00.
    svc.set_tax_lines(
        company,
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

    svc.submit_expense(company, claim.id, None, None)
        .await
        .unwrap();
    svc.approve_expense(company, claim.id, None)
        .await
        .unwrap();

    let posted = svc
        .post_expense(company, claim.id, accounts(), None)
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
}

// ─── EXP-10: post unwired fails closed with the stable code; double post 409 ──

#[tokio::test]
async fn post_unwired_fails_closed_and_double_post_conflicts() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "UNWD").await;
    let t = token_for(company);
    let app = create_guarded_expenses_routes(&m);

    let (_, body) = create_claim(app.clone(), &t, company, category, Uuid::new_v4(), "42000").await;
    let id = claim_id(&body);
    assert_eq!(
        req(
            app.clone(),
            "POST",
            &format!("/expenses/{id}/submit"),
            &t,
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
            &t,
            String::new()
        )
        .await,
        StatusCode::OK
    );

    // Accounting lands in W2 — the unwired sink MUST refuse, stable code, row stays approved.
    let (status, body) = req_full(
        app,
        "POST",
        &format!("/expenses/{id}/post"),
        Some(&t),
        format!(
            r#"{{"postAccounts":{{"employeePayableAccountId":"{}","bankAccountId":"{}"}}}}"#,
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unwired post: {body}");
    assert!(body.contains("gl_seam_unwired"), "stable seam code: {body}");
    let state: String = scoped_one(
        &pool,
        company,
        format!("SELECT state::text FROM expenses.expenses WHERE id = '{id}'"),
    )
    .await;
    assert_eq!(state, "approved", "row stays retryable");

    // Wire a sink at the SERVICE level (the W2 composition point), post, then double-post → 409.
    let svc = ExpensesWriteService::new(pool.clone())
        .with_gl_sink(Arc::new(RecordingGlSink::default()));
    svc.post_expense(company, id, accounts(), None)
        .await
        .unwrap();
    let err = svc
        .post_expense(company, id, accounts(), None)
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 409, "double post conflicts: {err}");
    assert_eq!(err.code(), "already_posted");
}

// ─── EXP-11: the fence — an unbound non-superuser sees ZERO rows ──────────────

#[tokio::test]
async fn unscoped_connection_sees_nothing() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "FENC").await;
    let t = token_for(company);

    let (status, _) = create_claim(
        create_guarded_expenses_routes(&m),
        &t,
        company,
        category,
        Uuid::new_v4(),
        "1000",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The fence is the arbiter — but this suite connects as the DB owner, whom RLS can
    // never bind (superusers bypass RLS even under FORCE; the fence migration itself says
    // the app must connect as a non-superuser). Run the probe the way production does:
    // SET ROLE to a plain role (the serpa_app posture) on one dedicated connection.
    let mut conn = pool.acquire().await.unwrap();
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

    // Unbound (no tenant): zero rows by design — the fence default.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM expenses.expenses")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(n, 0, "unbound non-superuser sees zero rows");

    // Bound to the company (request-scoped set_config, transaction-local like the app):
    // exactly its company's rows — this one claim.
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.company_id', $1, true)")
        .bind(company.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM expenses.expenses")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 1, "bound connection sees exactly its company's rows");
    tx.rollback().await.unwrap();

    sqlx::query("RESET ROLE").execute(&mut *conn).await.unwrap();
}

// ─── EXP-12: settle — own_account reimburses; company_account + unwired refuse ─

#[tokio::test]
async fn settle_reimburses_own_account_only() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "STTL").await;
    let employee = Uuid::new_v4();
    let gross = Decimal::new(800_000, 2); // 8,000.00

    let payment_id = Uuid::new_v4();
    let svc = ExpensesWriteService::new(pool.clone())
        .with_gl_sink(Arc::new(RecordingGlSink::default()))
        .with_reimbursement(Arc::new(FixedReimbursement(payment_id)));

    // own_account: full lifecycle to done, payment ack stamped.
    let claim = svc
        .create_expense(
            company,
            new_claim(category, employee, gross, ExpensePaymentMode::OwnAccount),
            None,
        )
        .await
        .unwrap();
    svc.submit_expense(company, claim.id, None, None)
        .await
        .unwrap();
    svc.approve_expense(company, claim.id, None)
        .await
        .unwrap();
    svc.post_expense(company, claim.id, accounts(), None)
        .await
        .unwrap();
    let settled = svc.settle_expense(company, claim.id, None).await.unwrap();
    assert!(matches!(settled.state, ExpenseState::Done), "state → done");
    assert_eq!(
        settled.reimbursement_id,
        Some(payment_id),
        "payment ack stamped"
    );

    // company_account: posted claims settle at the bank — settle refuses (409).
    let company_claim = svc
        .create_expense(
            company,
            new_claim(category, Uuid::new_v4(), gross, ExpensePaymentMode::CompanyAccount),
            None,
        )
        .await
        .unwrap();
    svc.submit_expense(company, company_claim.id, None, None)
        .await
        .unwrap();
    svc.approve_expense(company, company_claim.id, None)
        .await
        .unwrap();
    svc.post_expense(company, company_claim.id, accounts(), None)
        .await
        .unwrap();
    let err = svc
        .settle_expense(company, company_claim.id, None)
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 409, "company_account settle: {err}");
    assert_eq!(err.code(), "not_reimbursable");

    // Unwired reimbursement seam fails closed: posted stays posted, never 'done'.
    let unwired = ExpensesWriteService::new(pool.clone())
        .with_gl_sink(Arc::new(RecordingGlSink::default()));
    let claim3 = unwired
        .create_expense(
            company,
            new_claim(category, Uuid::new_v4(), gross, ExpensePaymentMode::OwnAccount),
            None,
        )
        .await
        .unwrap();
    unwired
        .submit_expense(company, claim3.id, None, None)
        .await
        .unwrap();
    unwired
        .approve_expense(company, claim3.id, None)
        .await
        .unwrap();
    unwired
        .post_expense(company, claim3.id, accounts(), None)
        .await
        .unwrap();
    let err = unwired
        .settle_expense(company, claim3.id, None)
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 422, "unwired settle fails closed: {err}");
    assert_eq!(err.code(), "reimbursement_seam_unwired");
    let state: String = scoped_one(
        &pool,
        company,
        format!("SELECT state::text FROM expenses.expenses WHERE id = '{}'", claim3.id),
    )
    .await;
    assert_eq!(state, "posted", "row stays retryable");
}

// ─── EXP-13: receipts — attach on open claims, frozen once decided ────────────

#[tokio::test]
async fn receipt_attach_on_open_claims_only() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "RCPT").await;
    let t = token_for(company);
    let app = create_guarded_expenses_routes(&m);

    let (_, body) = create_claim(app.clone(), &t, company, category, Uuid::new_v4(), "8000").await;
    let id = claim_id(&body);

    let file = Uuid::new_v4();
    let s = req(
        app.clone(),
        "POST",
        &format!("/expenses/{id}/receipt"),
        &t,
        format!(r#"{{"receiptFileId":"{file}"}}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "attach on draft");
    let rid: Uuid = scoped_one(
        &pool,
        company,
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
            &t,
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
            &t,
            String::new()
        )
        .await,
        StatusCode::OK
    );
    let s = req(
        app,
        "POST",
        &format!("/expenses/{id}/receipt"),
        &t,
        format!(r#"{{"receiptFileId":"{file}"}}"#),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "decided claim: guard matches zero rows");
}

// ─── EXP-14: the report projection — grouped totals, no sheet entity ──────────

#[tokio::test]
async fn report_projection_groups_by_category_and_state() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let cat_a = seed_category(&pool, company, "RP-A").await;
    let cat_b = seed_category(&pool, company, "RP-B").await;
    let employee = Uuid::new_v4();
    let t = token_for(company);
    let app = create_guarded_expenses_routes(&m);

    let (_, a1) = create_claim(app.clone(), &t, company, cat_a, employee, "10000").await;
    let (_, _a2) = create_claim(app.clone(), &t, company, cat_a, employee, "25000").await;
    let (_, _b1) = create_claim(app.clone(), &t, company, cat_b, employee, "5000").await;
    let id_a1 = claim_id(&a1);

    // Advance one RP-A claim to submitted so the projection shows BOTH states.
    assert_eq!(
        req(
            app.clone(),
            "POST",
            &format!("/expenses/{id_a1}/submit"),
            &t,
            String::new()
        )
        .await,
        StatusCode::OK
    );

    let (status, body) = req_full(
        app,
        "GET",
        &format!("/expenses/report?from=2026-01-01&to=2026-12-31&employeeId={employee}"),
        Some(&t),
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
        create_guarded_expenses_routes(&m),
        "GET",
        "/expenses/report?from=2026-12-31&to=2026-01-01",
        Some(&t),
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "bad range: {body}");
}

// ─── EXP-15: unauthenticated — no token, no surface ───────────────────────────

#[tokio::test]
async fn unauthenticated_is_rejected() {
    let pool = pool().await;
    let m = module(&pool).await;
    let app = create_guarded_expenses_routes(&m);
    let (status, _) = req_full(
        app,
        "GET",
        "/expenses/report?from=2026-01-01&to=2026-12-31",
        None,
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
    let category = seed_category(&pool, company, "FRZ").await;
    let employee = Uuid::new_v4();
    let t = token_for(company);
    let app = create_guarded_expenses_routes(&m);

    let (_, body) = create_claim(app.clone(), &t, company, category, employee, "50000").await;
    let id = claim_id(&body);

    // Draft: the overlay is writable.
    let s = req(
        app.clone(),
        "PUT",
        &format!("/expenses/{id}/tax-lines"),
        &t,
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
            &t,
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
        Some(&t),
        format!(
            r#"{{"lines":[{{"basis":"input","accountId":"{}","taxAmount":999999,"rate":11}}]}}"#,
            Uuid::new_v4()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "overlay frozen at submit: {body}");
    assert!(body.contains("not_draft"), "stable code: {body}");

    // And the refused replace left the frozen lines exactly as they were.
    let n: i64 = scoped_one(
        &pool,
        company,
        format!(
            "SELECT count(*) FROM expenses.expense_tax_lines WHERE expense_id = '{id}' \
             AND (metadata->>'deleted_at') IS NULL"
        ),
    )
    .await;
    assert_eq!(n, 1, "the original overlay row survives untouched");
    let amt: Decimal = scoped_one(
        &pool,
        company,
        format!(
            "SELECT tax_amount FROM expenses.expense_tax_lines WHERE expense_id = '{id}' \
             AND (metadata->>'deleted_at') IS NULL"
        ),
    )
    .await;
    assert_eq!(amt, Decimal::new(5_500, 0), "no inflated line landed");
}

// ─── EXP-17: the fence WRITE leg — a non-superuser writes only its company ────
//
// Council F4: reads-as-app-role were pinned in EXP-11, but the WITH CHECK leg (the write
// side of every policy) was never exercised — the exact gap behind the party v0.3.3
// 401/silent-500 incident. Drive one scoped write and one cross-scope write under SET ROLE.

#[tokio::test]
async fn fence_write_leg_holds_for_non_superuser() {
    let pool = pool().await;
    let company = Uuid::new_v4();

    let mut conn = pool.acquire().await.unwrap();
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

    // Scoped write: bound to the company, writing the company's row — passes WITH CHECK.
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.company_id', $1, true)")
        .bind(company.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO expenses.expense_categories
               (id, company_id, code, name, expense_account_id, metadata)
           VALUES ($1, $2, 'FENCE-W', 'fence write probe', $3, '{}'::jsonb)"#,
    )
    .bind(Uuid::new_v4())
    .bind(company)
    .bind(Uuid::new_v4())
    .execute(&mut *tx)
    .await
    .expect("scoped insert passes the WITH CHECK leg");

    // Cross-scope write: a different company_id — the policy must refuse it.
    let cross = sqlx::query(
        r#"INSERT INTO expenses.expense_categories
               (id, company_id, code, name, expense_account_id, metadata)
           VALUES ($1, $2, 'FENCE-X', 'cross-scope', $3, '{}'::jsonb)"#,
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(&mut *tx)
    .await;
    let err = match cross {
        Err(e) => e,
        Ok(_) => panic!("cross-scope insert must violate the fence's WITH CHECK"),
    };
    assert!(
        err.as_database_error().is_some(),
        "policy violation, not a transport error: {err}"
    );
    tx.rollback().await.unwrap();

    sqlx::query("RESET ROLE").execute(&mut *conn).await.unwrap();
}

// ─── EXP-22: a raced draft edit inside the file window never links a stale filing ──

/// A port that mutates the draft out-of-band inside `file` — exactly the concurrent-edit
/// window between the service's read and the compare-and-set link write. The id it returns
/// is fixed so the orphan left behind is traceable (the convergence probe reuses the trick).
struct RacingApprovals {
    pool: PgPool,
    company: Uuid,
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
            company_scope::with_company_scope(Some(self.company), async {
                sqlx::query("UPDATE expenses.expenses SET amount_total = amount_total + 1 WHERE id = $1")
                    .bind(self.expense_id)
                    .execute(&self.pool)
                    .await
                    .unwrap();
            })
            .await;
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
    let category = seed_category(&pool, company, "RACE").await;
    let employee = Uuid::new_v4();

    let svc = ExpensesWriteService::new(pool.clone());
    let claim = svc
        .create_expense(
            company,
            new_claim(category, employee, Decimal::new(50_000, 2), ExpensePaymentMode::OwnAccount),
            None,
        )
        .await
        .unwrap();

    // The port edits the draft inside the file window: the filing describes the pre-edit
    // amount, the row now carries post-edit values.
    let svc = svc.with_approvals(Arc::new(RacingApprovals {
        pool: pool.clone(),
        company,
        expense_id: claim.id,
        request_id: Uuid::new_v4(),
        race_once: Mutex::new(true),
    }));
    let err = svc
        .submit_expense(company, claim.id, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 409, "raced payload ⇒ conflict: {err}");
    assert_eq!(err.code(), "not_draft");

    // The row was NEVER linked to the stale filing — still draft, no request id.
    assert_eq!(
        state_pair(&pool, company, claim.id).await,
        ("draft".into(), "draft".into()),
        "the row keeps its draft pair after the refused link"
    );
    let linked: Option<String> = scoped_one(
        &pool,
        company,
        format!("SELECT approval_request_id::text FROM expenses.expenses WHERE id = '{}'", claim.id),
    )
    .await;
    assert_eq!(linked, None, "no approval link on a raced-out submit");
}

// ─── EXP-23: the retry converges — same live request, now on the fresh row ─────

#[tokio::test]
async fn submit_retry_converges_same_request() {
    let pool = pool().await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "CNVG").await;
    let employee = Uuid::new_v4();

    let request_id = Uuid::new_v4(); // the engine's one live request for this resource
    let svc = ExpensesWriteService::new(pool.clone());
    let claim = svc
        .create_expense(
            company,
            new_claim(category, employee, Decimal::new(75_000, 2), ExpensePaymentMode::OwnAccount),
            None,
        )
        .await
        .unwrap();

    let racing = Arc::new(RacingApprovals {
        pool: pool.clone(),
        company,
        expense_id: claim.id,
        request_id,
        race_once: Mutex::new(true),
    });
    let svc = svc.with_approvals(racing.clone());

    // Attempt 1: the race 409s, leaving an orphaned-but-live filing in the engine.
    assert_eq!(
        svc.submit_expense(company, claim.id, None, None)
            .await
            .unwrap_err()
            .http_status(),
        409
    );

    // Attempt 2 (idempotent file ⇒ the same live request id): the CAS now matches the
    // fresh row and the link lands on the SAME request the orphan was filed under.
    let submitted = svc
        .submit_expense(company, claim.id, None, None)
        .await
        .unwrap();
    assert_eq!(
        submitted.approval_request_id,
        Some(request_id),
        "retry links the one live request — no duplicate filing"
    );
    assert_eq!(
        state_pair(&pool, company, claim.id).await,
        ("submitted".into(), "submitted".into())
    );
    assert!(!*racing.race_once.lock().unwrap());
}

// ─── EXP-24: the module-built service can be armed post-build (the compose path) ──

#[tokio::test]
async fn module_built_service_arms_approvals_after_build() {
    let pool = pool().await;
    let m = module(&pool).await;
    let company = Uuid::new_v4();
    let category = seed_category(&pool, company, "ARMD").await;
    let t = token_for(company);
    let request_id = Uuid::new_v4();

    // The module was built unwired (family default); the composing app arms it afterwards —
    // the exact sequence a service that composes this module performs at startup.
    m.set_expenses_approvals(Arc::new(FakeApprovals {
        verdict: ApprovalVerdict::Approved,
    }));
    let _ = request_id;

    let app = create_guarded_expenses_routes(&m);
    let (status, body) = create_claim(app.clone(), &t, company, category, Uuid::new_v4(), "12000").await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    let id = claim_id(&body);

    assert_eq!(
        req(app, "POST", &format!("/expenses/{id}/submit"), &t, String::new()).await,
        StatusCode::OK
    );
    let linked: Option<String> = scoped_one(
        &pool,
        company,
        format!("SELECT approval_request_id::text FROM expenses.expenses WHERE id = '{id}'"),
    )
    .await;
    assert!(linked.is_some(), "armed-after-build port ⇒ submit links: {linked:?}");
}
