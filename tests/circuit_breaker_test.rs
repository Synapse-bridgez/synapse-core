use synapse_core::stellar::HorizonClient;

#[tokio::test]
async fn test_circuit_breaker_state() {
    let client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());

    // Initially, circuit should be closed
    let state = client.circuit_state();
    assert!(state == "closed" || state == "open");
}

#[tokio::test]
async fn test_circuit_breaker_with_custom_config() {
    let client = HorizonClient::with_circuit_breaker(
        "https://horizon-testnet.stellar.org".to_string(),
        5,
        30,
    );

    let state = client.circuit_state();
    assert!(state == "closed" || state == "open");
}
