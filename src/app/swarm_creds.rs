//! OS-keychain storage for Swarm credentials.
//!
//! Per-host the service "diffie-swarm" holds two entries:
//!   account "{host}:user"   -> username
//!   account "{host}:ticket" -> P4 ticket (used as Basic-Auth password)
//!
//! All operations are best-effort: if the platform keychain is unavailable
//! the helpers degrade to None/no-op rather than crashing the GUI.

const SERVICE: &str = "diffie-swarm";

fn entry(host: &str, kind: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, &format!("{host}:{kind}")).ok()
}

pub fn load(host: &str) -> Option<(String, String)> {
    let user = entry(host, "user")?.get_password().ok()?;
    let ticket = entry(host, "ticket")?.get_password().ok()?;
    Some((user, ticket))
}

pub fn store(host: &str, user: &str, ticket: &str) {
    if let Some(e) = entry(host, "user") { let _ = e.set_password(user); }
    if let Some(e) = entry(host, "ticket") { let _ = e.set_password(ticket); }
}

pub fn clear_ticket(host: &str) {
    if let Some(e) = entry(host, "ticket") { let _ = e.delete_credential(); }
}

pub fn load_user(host: &str) -> Option<String> {
    entry(host, "user")?.get_password().ok()
}
