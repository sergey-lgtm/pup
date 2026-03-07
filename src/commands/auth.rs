use anyhow::{bail, Result};

use crate::auth::storage;
use crate::config::Config;

/// Helper to run a closure with the storage lock held (non-async to avoid holding lock across await).
fn with_storage<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&mut dyn storage::Storage) -> Result<R>,
{
    let guard = storage::get_storage()?;
    let mut lock = guard.lock().unwrap();
    let store = lock.as_mut().unwrap();
    f(&mut **store)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn login(cfg: &Config, scopes: Vec<String>) -> Result<()> {
    use crate::auth::{dcr, pkce};

    let site = &cfg.site;
    let org = cfg.org.as_deref();

    // 1. Start callback server
    let mut server = crate::auth::callback::CallbackServer::new().await?;
    let redirect_uri = server.redirect_uri();
    let org_label = org.map(|o| format!(" (org: {o})")).unwrap_or_default();
    eprintln!("\n🔐 Starting OAuth2 login for site: {site}{org_label}\n");
    eprintln!("📡 Callback server started on: {redirect_uri}");

    let scope_strs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    if scopes.len() > 10 {
        eprintln!(
            "🔑 Requesting {} scope(s) (use --scopes to customize)",
            scopes.len()
        );
    } else {
        eprintln!(
            "🔑 Requesting {} scope(s): {}",
            scopes.len(),
            scopes.join(", ")
        );
    }

    // 2. Load existing client credentials (lock released before any await)
    // Client credentials are site-scoped (DCR is per-site, shared across orgs)
    let existing_creds = with_storage(|store| store.load_client_credentials(site))?;

    let creds = match existing_creds {
        Some(creds) => {
            eprintln!("✓ Using existing client registration");
            creds
        }
        None => {
            eprintln!("📝 Registering new OAuth2 client...");
            let dcr_client = dcr::DcrClient::new(site);
            let creds = dcr_client.register(&redirect_uri, &scope_strs).await?;
            with_storage(|store| store.save_client_credentials(site, &creds))?;
            eprintln!("✓ Registered client: {}", creds.client_id);
            creds
        }
    };

    // 3. Generate PKCE challenge + state
    let challenge = pkce::generate_pkce_challenge()?;
    let state = pkce::generate_state()?;

    // 4. Build authorization URL
    let dcr_client = dcr::DcrClient::new(site);
    let auth_url = dcr_client.build_authorization_url(
        &creds.client_id,
        &redirect_uri,
        &state,
        &challenge,
        &scope_strs,
    );

    // 5. Open browser
    eprintln!("\n🌐 Opening browser for authentication...");
    eprintln!("If the browser doesn't open, visit: {auth_url}");
    let _ = open::that(&auth_url);

    // 6. Wait for callback
    eprintln!("\n⏳ Waiting for authorization...");
    let result = server
        .wait_for_callback(std::time::Duration::from_secs(300))
        .await?;

    if let Some(err) = &result.error {
        let desc = result.error_description.as_deref().unwrap_or("");
        bail!("OAuth error: {err}: {desc}");
    }

    if result.state != state {
        bail!("OAuth state mismatch (possible CSRF attack)");
    }

    // 7. Exchange code for tokens
    eprintln!("🔄 Exchanging authorization code for tokens...");
    let tokens = dcr_client
        .exchange_code(&result.code, &redirect_uri, &challenge.verifier, &creds)
        .await?;

    let location = with_storage(|store| {
        store.save_tokens(site, org, &tokens)?;
        Ok(store.storage_location())
    })?;

    // Register this session in the session registry
    storage::save_session(site, org)?;

    let expires_at = chrono::DateTime::from_timestamp(tokens.issued_at + tokens.expires_in, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).to_rfc3339())
        .unwrap_or_else(|| format!("in {} hours", tokens.expires_in / 3600));

    eprintln!("\n✅ Login successful{org_label}!");
    eprintln!("   Access token expires: {expires_at}");
    eprintln!("   Token stored in: {location}");

    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn login(_cfg: &Config, _scopes: Vec<String>) -> Result<()> {
    bail!(
        "OAuth login is not available in WASM builds.\n\
         Use DD_ACCESS_TOKEN env var for bearer token auth,\n\
         or DD_API_KEY + DD_APP_KEY for API key auth."
    )
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn logout(cfg: &Config) -> Result<()> {
    let site = &cfg.site;
    let org = cfg.org.as_deref();
    with_storage(|store| {
        store.delete_tokens(site, org)?;
        // Only delete client credentials when logging out the default (no-org) session;
        // client credentials are site-scoped and shared across orgs
        if org.is_none() {
            store.delete_client_credentials(site)?;
        }
        Ok(())
    })?;
    storage::remove_session(site, org)?;
    let org_label = org.map(|o| format!(" (org: {o})")).unwrap_or_default();
    eprintln!("Logged out from {site}{org_label}. Tokens removed.");
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn logout(_cfg: &Config) -> Result<()> {
    bail!(
        "OAuth logout is not available in WASM builds.\n\
         Token storage is not available — credentials are read from environment variables."
    )
}

pub fn status(cfg: &Config) -> Result<()> {
    let site = &cfg.site;
    let org = cfg.org.as_deref();

    // In WASM, just report env var status
    #[cfg(target_arch = "wasm32")]
    {
        if cfg.has_bearer_token() || cfg.has_api_keys() {
            println!("✅ Authenticated for site: {site}");
        } else {
            println!("❌ Not authenticated for site: {site}");
        }
        return Ok(());
    }

    #[cfg(not(target_arch = "wasm32"))]
    with_storage(|store| {
        match store.load_tokens(site, org)? {
            Some(tokens) => {
                let expires_at_ts = tokens.issued_at + tokens.expires_in;
                let now = chrono::Utc::now().timestamp();
                let remaining_secs = expires_at_ts - now;

                let (status, remaining_str) = if tokens.is_expired() {
                    ("expired".to_string(), "expired".to_string())
                } else {
                    let mins = remaining_secs / 60;
                    let secs = remaining_secs % 60;
                    ("valid".to_string(), format!("{mins}m{secs}s"))
                };

                let org_label = org.map(|o| format!(" (org: {o})")).unwrap_or_default();
                if tokens.is_expired() {
                    eprintln!("⚠️  Token expired for site: {site}{org_label}");
                } else {
                    eprintln!("✅ Authenticated for site: {site}{org_label}");
                    eprintln!("   Token expires in: {remaining_str}");
                }

                let expires_at = chrono::DateTime::from_timestamp(expires_at_ts, 0)
                    .map(|dt| dt.with_timezone(&chrono::Local).to_rfc3339())
                    .unwrap_or_default();

                let scopes: Vec<&str> = tokens
                    .scope
                    .split_whitespace()
                    .filter(|s| !s.is_empty())
                    .collect();

                let json = serde_json::json!({
                    "authenticated": true,
                    "expires_at": expires_at,
                    "has_refresh": !tokens.refresh_token.is_empty(),
                    "org": org,
                    "scopes": scopes,
                    "site": site,
                    "status": status,
                    "token_type": tokens.token_type,
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            }
            None => {
                let org_label = org.map(|o| format!(" (org: {o})")).unwrap_or_default();
                eprintln!("❌ Not authenticated for site: {site}{org_label}");
                let json = serde_json::json!({
                    "authenticated": false,
                    "org": org,
                    "site": site,
                    "status": "no token",
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            }
        }
        Ok(())
    })
}

pub fn token(cfg: &Config) -> Result<()> {
    if let Some(token) = &cfg.access_token {
        println!("{token}");
        return Ok(());
    }

    #[cfg(target_arch = "wasm32")]
    bail!("no token available — set DD_ACCESS_TOKEN env var");

    #[cfg(not(target_arch = "wasm32"))]
    {
        let site = &cfg.site;
        let org = cfg.org.as_deref();
        with_storage(|store| match store.load_tokens(site, org)? {
            Some(tokens) => {
                if tokens.is_expired() {
                    bail!("token is expired — run 'pup auth login' to refresh");
                }
                println!("{}", tokens.access_token);
                Ok(())
            }
            None => bail!("no token available — run 'pup auth login' or set DD_ACCESS_TOKEN"),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn refresh(cfg: &Config) -> Result<()> {
    use crate::auth::dcr;

    let site = &cfg.site;
    let org = cfg.org.as_deref();

    let tokens = with_storage(|store| store.load_tokens(site, org))?.ok_or_else(|| {
        anyhow::anyhow!("no tokens found for site {site} — run 'pup auth login' first")
    })?;

    if tokens.refresh_token.is_empty() {
        bail!("no refresh token available — run 'pup auth login' to re-authenticate");
    }

    let creds = with_storage(|store| store.load_client_credentials(site))?.ok_or_else(|| {
        anyhow::anyhow!("no client credentials found for site {site} — run 'pup auth login' first")
    })?;

    let org_label = org.map(|o| format!(" (org: {o})")).unwrap_or_default();
    eprintln!("🔄 Refreshing access token for site: {site}{org_label}...");

    let dcr_client = dcr::DcrClient::new(site);
    let new_tokens = dcr_client
        .refresh_token(&tokens.refresh_token, &creds)
        .await?;

    let location = with_storage(|store| {
        store.save_tokens(site, org, &new_tokens)?;
        Ok(store.storage_location())
    })?;

    let expires_at =
        chrono::DateTime::from_timestamp(new_tokens.issued_at + new_tokens.expires_in, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).to_rfc3339())
            .unwrap_or_else(|| format!("in {} hours", new_tokens.expires_in / 3600));

    eprintln!("✅ Token refreshed successfully!");
    eprintln!("   Access token expires: {expires_at}");
    eprintln!("   Token stored in: {location}");

    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn refresh(_cfg: &Config) -> Result<()> {
    bail!(
        "Token refresh is not available in WASM builds.\n\
         Use DD_ACCESS_TOKEN env var for bearer token auth."
    )
}

/// List all stored org sessions from the session registry, enriched with token status.
#[cfg(not(target_arch = "wasm32"))]
pub fn list(cfg: &Config) -> Result<()> {
    let sessions = storage::list_sessions()?;

    let enriched: Vec<serde_json::Value> = sessions
        .into_iter()
        .map(|s| {
            let tokens = with_storage(|store| store.load_tokens(&s.site, s.org.as_deref()))
                .ok()
                .flatten();

            match tokens {
                Some(t) => {
                    let expires_at_ts = t.issued_at + t.expires_in;
                    let is_expired = t.is_expired();
                    let status = if is_expired { "expired" } else { "valid" };
                    let expires_at = chrono::DateTime::from_timestamp(expires_at_ts, 0)
                        .map(|dt| dt.with_timezone(&chrono::Local).to_rfc3339())
                        .unwrap_or_default();
                    let scopes: Vec<&str> = t
                        .scope
                        .split_whitespace()
                        .filter(|s| !s.is_empty())
                        .collect();
                    serde_json::json!({
                        "expires_at": expires_at,
                        "has_refresh": !t.refresh_token.is_empty(),
                        "org": s.org,
                        "scopes": scopes,
                        "site": s.site,
                        "status": status,
                    })
                }
                None => serde_json::json!({
                    "expires_at": null,
                    "has_refresh": false,
                    "org": s.org,
                    "scopes": [],
                    "site": s.site,
                    "status": "no token",
                }),
            }
        })
        .collect();

    crate::formatter::output(cfg, &enriched)
}

#[cfg(target_arch = "wasm32")]
pub fn list(_cfg: &Config) -> Result<()> {
    bail!(
        "pup auth list is not available in WASM builds.\n\
         Session storage is not available — credentials are read from environment variables."
    )
}
