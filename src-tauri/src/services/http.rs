//! The one way to get a `reqwest::Client` in SundayEdit.
//!
//! reqwest 0.13 runs on `rustls-no-provider` (see Cargo.toml). In that mode it
//! PANICS on client construction when no rustls crypto provider is installed in
//! the process — it does not pick one from the compiled-in features.
//! tauri-plugin-updater installs `ring` the same way right before it builds its
//! own client, but nothing guarantees the updater has run before a user's first
//! transcription, model download or LLM request — so every client goes through
//! here.

/// Install `ring` as the process-wide rustls crypto provider, once.
/// Idempotent: a provider that is already installed (ours or the updater's) wins.
pub fn ensure_rustls_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

/// A plain `reqwest::Client`, with the crypto provider guaranteed.
pub fn client() -> reqwest::Client {
    ensure_rustls_provider();
    reqwest::Client::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn client_builds_in_a_process_with_no_provider_installed() {
        // A test process starts with no provider installed — exactly like a fresh
        // launch before the updater has run. Without `ensure_rustls_provider` this
        // panics inside reqwest; it compiles fine either way.
        let _ = super::client();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
