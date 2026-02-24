use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[test]
fn test_hmac_signature_generation() {
    let secret = "test_secret_key";
    let payload = r#"{"id":"123","status":"completed"}"#;
    
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    let result = mac.finalize();
    let signature = hex::encode(result.into_bytes());
    
    // Verify signature is a valid hex string
    assert_eq!(signature.len(), 64); // SHA256 produces 32 bytes = 64 hex chars
    assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_hmac_signature_verification() {
    let secret = "test_secret_key";
    let payload = r#"{"id":"123","status":"completed"}"#;
    
    // Generate signature
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    let expected_signature = mac.finalize().into_bytes();
    
    // Verify signature
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    assert!(mac.verify_slice(&expected_signature).is_ok());
}

#[test]
fn test_hmac_signature_mismatch() {
    let secret = "test_secret_key";
    let payload = r#"{"id":"123","status":"completed"}"#;
    let wrong_payload = r#"{"id":"456","status":"pending"}"#;
    
    // Generate signature for original payload
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    let signature = mac.finalize().into_bytes();
    
    // Try to verify with wrong payload
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(wrong_payload.as_bytes());
    assert!(mac.verify_slice(&signature).is_err());
}

#[test]
fn test_constant_time_comparison() {
    // The hmac crate uses constant-time comparison internally
    // This test verifies that different signatures fail verification
    let secret = "test_secret_key";
    let payload1 = "payload1";
    let payload2 = "payload2";
    
    let mut mac1 = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac1.update(payload1.as_bytes());
    let sig1 = mac1.finalize().into_bytes();
    
    let mut mac2 = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac2.update(payload2.as_bytes());
    
    // Verification should fail for different payloads
    assert!(mac2.verify_slice(&sig1).is_err());
}
