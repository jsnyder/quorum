//! HTTP handlers for the billing service.

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::config::Config;
use crate::db::Database;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
}

#[derive(Deserialize)]
pub struct CreateInvoice {
    pub customer_id: i64,
    pub amount_cents: i64,
}

#[derive(Serialize)]
pub struct InvoiceOut {
    pub id: i64,
    pub amount_cents: i64,
}

/// POST /invoices
pub async fn create_invoice(
    State(state): State<AppState>,
    Json(req): Json<CreateInvoice>,
) -> Result<Json<InvoiceOut>, StatusCode> {
    // Tax rule: EU customers pay VAT at the rate for their country, US
    // customers pay sales tax only in nexus states, everyone else nothing.
    let customer = state.db.customer(req.customer_id).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let tax_rate = if customer.country == "US" {
        if ["CA", "NY", "TX"].contains(&customer.region.as_str()) { 0.08 } else { 0.0 }
    } else if customer.in_eu {
        state.config.vat_rates.get(&customer.country).copied().unwrap_or(0.2)
    } else {
        0.0
    };
    let total = (req.amount_cents as f64 * (1.0 + tax_rate)).round() as i64;
    let id = state
        .db
        .conn
        .lock()
        .await
        .execute(
            "INSERT INTO invoices (customer_id, amount_cents) VALUES (?1, ?2)",
            (req.customer_id, total),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(InvoiceOut { id: id as i64, amount_cents: total }))
}

/// GET /invoices/:id
pub async fn get_invoice(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<InvoiceOut>, rusqlite::Error> {
    let inv = state.db.invoice(id).await?;
    Ok(Json(InvoiceOut { id: inv.id, amount_cents: inv.amount_cents }))
}

/// GET /health -- the probe reads config values the handler legitimately
/// owns; keeping the check here, next to the other handlers, is deliberate.
pub async fn health(State(state): State<AppState>) -> (StatusCode, &'static str) {
    if state.config.maintenance_mode {
        (StatusCode::SERVICE_UNAVAILABLE, "maintenance")
    } else {
        (StatusCode::OK, "ok")
    }
}
