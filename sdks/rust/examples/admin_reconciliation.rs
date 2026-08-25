//! Example: Admin Reconciliation Operations
//!
//! This example demonstrates how to:
//! - List reconciliation reports with pagination
//! - Retrieve details of a specific report
//! - Run a new reconciliation
//!
//! The example shows both successful operations and error handling,
//! including how to interpret discrepancy reports.

use synapse_sdk::models::ListReportsParams;
use synapse_sdk::AdminSynapseClient;

#[tokio::main]
async fn main() {
    // Initialize the admin client
    let admin = AdminSynapseClient::builder(
        "https://api.example.com", // Replace with your actual base URL
        "your-admin-api-key",      // Replace with your admin key
    )
    .build();

    let reconciliation = admin.reconciliation();

    // Example 1: List reconciliation reports
    println!("=== Listing Reconciliation Reports ===");
    match reconciliation
        .list_reports(ListReportsParams {
            limit: Some(10),
            offset: Some(0),
        })
        .await
    {
        Ok(reports) => {
            println!("Found {} total reports", reports.total);
            for (i, report) in reports.reports.iter().enumerate() {
                println!(
                    "  {}. Report ID: {} (generated: {})",
                    i + 1,
                    report.id,
                    report.generated_at
                );
                println!(
                    "     Discrepancies: missing={}, orphaned={}, amount_mismatches={}",
                    report.missing_on_chain_count,
                    report.orphaned_payments_count,
                    report.amount_mismatches_count
                );
            }
        }
        Err(e) => {
            eprintln!("Failed to list reports: {}", e);
            return;
        }
    }

    // Example 2: Get details of a specific report
    println!("\n=== Getting Report Details ===");
    // Replace with an actual report ID from your system
    let example_report_id =
        uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").expect("invalid UUID");

    match reconciliation.get_report(example_report_id).await {
        Ok(report) => {
            println!("Report ID: {}", report.id);
            println!("Period: {} to {}", report.period_start, report.period_end);
            println!("\nSummary:");
            println!(
                "  Total DB transactions: {}",
                report.summary.total_db_transactions
            );
            println!(
                "  Total chain payments: {}",
                report.summary.total_chain_payments
            );
            println!(
                "  Missing on chain: {}",
                report.summary.missing_on_chain_count
            );
            println!(
                "  Orphaned payments: {}",
                report.summary.orphaned_payments_count
            );
            println!(
                "  Amount mismatches: {}",
                report.summary.amount_mismatches_count
            );
            println!("  Has discrepancies: {}", report.summary.has_discrepancies);

            // Show details of discrepancies
            if !report.missing_on_chain.is_empty() {
                println!("\nMissing on chain transactions:");
                for tx in &report.missing_on_chain {
                    println!(
                        "  - ID: {}, Amount: {}, Account: {}",
                        tx.id, tx.amount, tx.stellar_account
                    );
                }
            }

            if !report.orphaned_payments.is_empty() {
                println!("\nOrphaned payments:");
                for payment in &report.orphaned_payments {
                    println!(
                        "  - ID: {}, Amount: {}, From: {} To: {}",
                        payment.payment_id, payment.amount, payment.from, payment.to
                    );
                }
            }

            if !report.amount_mismatches.is_empty() {
                println!("\nAmount mismatches:");
                for mismatch in &report.amount_mismatches {
                    println!(
                        "  - TX: {}, DB: {}, Chain: {}",
                        mismatch.transaction_id, mismatch.db_amount, mismatch.chain_amount
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to get report: {}", e);
        }
    }

    // Example 3: Run a new reconciliation
    println!("\n=== Running Reconciliation ===");
    let stellar_account = "GABC1234567890123456789012345678901234567890123456789012";
    let period_hours = 24;

    match reconciliation
        .run(stellar_account, Some(period_hours))
        .await
    {
        Ok(response) => {
            println!("{}", response.message);
            println!("Report Summary:");
            println!("  ID: {}", response.report.id);
            println!("  Generated: {}", response.report.generated_at);
            println!(
                "  DB Transactions: {}",
                response.report.total_db_transactions
            );
            println!("  Chain Payments: {}", response.report.total_chain_payments);

            if response.report.has_discrepancies {
                println!(
                    "\n⚠️  Discrepancies detected! Missing: {}, Orphaned: {}, Mismatches: {}",
                    response.report.missing_on_chain_count,
                    response.report.orphaned_payments_count,
                    response.report.amount_mismatches_count
                );
            } else {
                println!("\n✓ No discrepancies found!");
            }
        }
        Err(e) => {
            eprintln!("Reconciliation failed: {}", e);
            // Handle errors like invalid account or server issues
            match e {
                synapse_sdk::SynapseError::Api { status, message } => {
                    if status == 400 {
                        eprintln!("Invalid account format or parameters: {}", message);
                    } else {
                        eprintln!("API error ({status}): {message}");
                    }
                }
                _ => eprintln!("Network or other error: {}", e),
            }
        }
    }
}
