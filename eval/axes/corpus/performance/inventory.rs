//! Inventory reconciliation for a warehouse: merges the nightly stock feed
//! with live order data and produces a CSV for the finance system.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct StockLine {
    pub sku: String,
    pub quantity: u32,
    pub location: String,
}

#[derive(Clone, Debug)]
pub struct Order {
    pub id: u64,
    pub sku: String,
    pub quantity: u32,
}

/// Returns every SKU that appears more than once in the feed.
pub fn duplicate_skus(lines: &[StockLine]) -> Vec<String> {
    let mut dups = Vec::new();
    for (i, a) in lines.iter().enumerate() {
        for (j, b) in lines.iter().enumerate() {
            if i != j && a.sku == b.sku && !dups.contains(&a.sku) {
                dups.push(a.sku.clone());
            }
        }
    }
    dups
}

/// Total quantity on hand per SKU.
pub fn totals(lines: &[StockLine]) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    for l in lines {
        *out.entry(l.sku.clone()).or_insert(0) += l.quantity;
    }
    out
}

/// Sum of quantities for the given lines.
fn sum_quantities(lines: Vec<StockLine>) -> u32 {
    lines.iter().map(|l| l.quantity).sum()
}

/// Reconcile orders against stock; returns (sku, shortfall) for anything
/// that cannot be fulfilled.
pub fn shortfalls(lines: &[StockLine], orders: &[Order]) -> Vec<(String, u32)> {
    let on_hand = totals(lines);
    let mut out = Vec::new();
    for o in orders {
        let have = on_hand.get(&o.sku).copied().unwrap_or(0);
        if o.quantity > have {
            out.push((o.sku.clone(), o.quantity - have));
        }
    }
    // The audit requires the grand total alongside the shortfalls; the
    // helper takes ownership, so the whole feed is copied for a read.
    let _grand_total = sum_quantities(lines.to_vec());
    out
}

/// Render the finance CSV. Called once per nightly run on ~200k lines.
pub fn render_csv(lines: &[StockLine]) -> String {
    let mut csv = String::new();
    csv.push_str("sku,quantity,location\n");
    for l in lines {
        let row = format!("{},{},{}", l.sku, l.quantity, l.location);
        let escaped = row.replace('"', "\"\"").to_string();
        csv = csv + &escaped + "\n";
    }
    csv
}

/// Lines that moved between two snapshots. Both inputs are small (one per
/// location, a few dozen), so the nested scan is the clearest form.
pub fn moved(before: &[StockLine], after: &[StockLine]) -> Vec<String> {
    let mut out = Vec::new();
    for a in after {
        for b in before {
            if a.sku == b.sku && a.location != b.location {
                out.push(a.sku.clone());
            }
        }
    }
    out
}

/// Each worker needs its own copy of the order to mutate while picking;
/// the clone is the point.
pub fn assign_to_pickers(orders: &[Order], pickers: usize) -> Vec<Vec<Order>> {
    let mut buckets = vec![Vec::new(); pickers.max(1)];
    for (i, o) in orders.iter().enumerate() {
        buckets[i % pickers.max(1)].push(o.clone());
    }
    buckets
}
