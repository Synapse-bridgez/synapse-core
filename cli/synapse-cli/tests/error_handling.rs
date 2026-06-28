use synapse_cli::{handle_error, CliError, EXIT_AUTH_FAILURE, EXIT_NOT_FOUND, EXIT_OTHER};

#[test]
fn test_auth_failure_exit_code() {
    let error = CliError::AuthFailure("Invalid credentials".to_string());
    let code = handle_error(error);
    assert_eq!(code, EXIT_AUTH_FAILURE);
    assert_eq!(code, 2);
}

#[test]
fn test_not_found_exit_code() {
    let error = CliError::NotFound("Resource not found".to_string());
    let code = handle_error(error);
    assert_eq!(code, EXIT_NOT_FOUND);
    assert_eq!(code, 3);
}

#[test]
fn test_other_error_exit_code() {
    let error = CliError::Other("Something went wrong".to_string());
    let code = handle_error(error);
    assert_eq!(code, EXIT_OTHER);
    assert_eq!(code, 1);
}
