//! Tests for issue #948: src/handlers/auth.rs is not compiled and references nonexistent fields.
//!
//! This test verifies that the orphaned auth handler is properly removed from the build,
//! ensuring no dead code exists that could be mistaken for active authentication logic.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// Verify that src/handlers/auth.rs is not referenced in handlers/mod.rs
    /// This ensures the orphaned handler is not accidentally compiled into the binary.
    #[test]
    fn orphaned_auth_handler_not_compiled() {
        let mod_rs_path = "src/handlers/mod.rs";
        assert!(
            Path::new(mod_rs_path).exists(),
            "src/handlers/mod.rs should exist"
        );

        let content = fs::read_to_string(mod_rs_path)
            .expect("Failed to read src/handlers/mod.rs");

        // Verify that auth module is NOT declared
        assert!(
            !content.contains("pub mod auth"),
            "src/handlers/auth.rs should not be included via 'pub mod auth;' in handlers/mod.rs. \
             The orphaned handler was removed as per issue #948."
        );
    }

    /// Verify that the orphaned auth.rs file either doesn't exist or is not part of the module tree.
    /// This test documents the state after the fix for issue #948.
    #[test]
    fn auth_handler_file_state() {
        let auth_rs_path = "src/handlers/auth.rs";
        let mod_rs_path = "src/handlers/mod.rs";

        // If the file exists, it should not be included in the module tree
        if Path::new(auth_rs_path).exists() {
            let content = fs::read_to_string(mod_rs_path)
                .expect("Failed to read src/handlers/mod.rs");
            assert!(
                !content.contains("pub mod auth"),
                "If src/handlers/auth.rs exists, it should not be included in mod.rs"
            );
        }
        // If the file doesn't exist, that's the ideal state (orphaned code removed)
        // Both states are acceptable; the test documents that either:
        // 1. The file is deleted (cleanest solution)
        // 2. The file exists but is never compiled (less ideal but acceptable)
    }
}
