//! Example demonstrating the TenantManagerClient functionality
//!
//! This example shows how to use the TenantManagerClient to:
//! 1. Create a new tenant
//! 2. List tenants
//! 3. Find a tenant by API key
//! 4. Update a tenant
//! 5. Delete (soft-delete) a tenant

use anyhow::Result;
use log_gateway::tenant_manager::{TenantCreate, TenantUpdate};

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Tenant Manager Example ===\n");

    // Note: In a real application, you would connect to an actual ClickHouse instance
    // For this example, we'll just demonstrate the API usage

    // Create a mock client (in reality, you'd use TenantManagerClient::new())
    println!("1. Creating TenantManagerClient");
    println!("   (In reality: TenantManagerClient::new(\"http://clickhouse:8123\", \"bgp\"))");

    // Example 1: Creating a tenant
    println!("\n2. Creating a new tenant");
    let tenant_create = TenantCreate {
        name: "Example Corp".to_string(),
        api_key: "secret-api-key-123".to_string(),
        rate_limit_per_sec: Some(1000),
        plan: "pro".to_string(),
    };

    println!("   Name: {}", tenant_create.name);
    println!("   API Key: {} (will be hashed)", tenant_create.api_key);
    println!(
        "   Rate Limit: {} req/s",
        tenant_create.rate_limit_per_sec.unwrap_or(1000)
    );
    println!("   Plan: {}", tenant_create.plan);

    // Example 2: Listing tenants
    println!("\n3. Listing all active tenants");
    println!("   (In reality: client.list_tenants().await)");
    println!("   Returns: Vec<Tenant> with id, name, api_key_hash, rate_limit_per_sec, plan, created_at, updated_at, enabled");

    // Example 3: Finding tenant by API key
    println!("\n4. Finding tenant by API key");
    println!("   (In reality: client.find_by_api_key(\"secret-api-key-123\").await)");
    println!("   Returns: Option<Tenant> if found and enabled");

    // Example 4: Updating a tenant
    println!("\n5. Updating a tenant");
    let tenant_update = TenantUpdate {
        name: Some("Example Corp Updated".to_string()),
        rate_limit_per_sec: Some(2000),
        plan: Some("enterprise".to_string()),
        enabled: None, // Keep current value
    };

    println!("   New name: {:?}", tenant_update.name);
    println!("   New rate limit: {:?}", tenant_update.rate_limit_per_sec);
    println!("   New plan: {:?}", tenant_update.plan);
    println!("   (In reality: client.update_tenant(id, update).await)");

    // Example 5: Deleting (soft-deleting) a tenant
    println!("\n6. Soft-deleting a tenant");
    println!("   (In reality: client.delete_tenant(id).await)");
    println!("   Sets enabled = false (soft delete)");

    // Example 6: Tenant struct
    println!("\n7. Tenant struct fields:");
    println!("   - id: Uuid");
    println!("   - name: String");
    println!("   - api_key_hash: String (SHA-256 hex)");
    println!("   - rate_limit_per_sec: u32 (req/s, default: 1000)");
    println!("   - plan: String (\"free\" | \"pro\" | \"enterprise\")");
    println!("   - created_at: DateTime<Utc>");
    println!("   - updated_at: DateTime<Utc>");
    println!("   - enabled: bool");

    // Example of hash function
    println!("\n8. API Key Hashing:");
    println!("   Plaintext: \"my-secret-key\"");
    println!("   Hashed (SHA-256 hex): {}", hash_example("my-secret-key"));
    println!("   Length: 64 characters");

    println!("\n=== Example Complete ===");

    Ok(())
}

fn hash_example(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}
