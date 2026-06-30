use std::fmt;

#[derive(Debug)]
pub enum CliError {
    AuthFailure(String),
    NotFound(String),
    Other(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::AuthFailure(msg) => write!(f, "{}", msg),
            CliError::NotFound(msg) => write!(f, "{}", msg),
            CliError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for CliError {}

pub const EXIT_AUTH_FAILURE: i32 = 2;
pub const EXIT_NOT_FOUND: i32 = 3;
pub const EXIT_OTHER: i32 = 1;

pub fn handle_error(error: CliError) -> i32 {
    eprintln!("error: {}", error);
    match error {
        CliError::AuthFailure(_) => EXIT_AUTH_FAILURE,
        CliError::NotFound(_) => EXIT_NOT_FOUND,
        CliError::Other(_) => EXIT_OTHER,
    }
}

pub fn map_http_error(status: u16, body: String) -> CliError {
    match status {
        401 | 403 => CliError::AuthFailure(format!("Authentication failed: {}", body)),
        404 => CliError::NotFound(format!("Not found: {}", body)),
        _ => CliError::Other(format!("HTTP error {}: {}", status, body)),
    }
}

pub fn map_network_error(msg: String) -> CliError {
    CliError::Other(format!("Network error: {}", msg))
}
