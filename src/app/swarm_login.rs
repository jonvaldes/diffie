//! Modal login dialog shown on launch when the user supplied a Swarm URL.
//!
//! Owns a small state machine:
//!   Pending(Idle)      -> user is editing the form
//!   Pending(Probing/LoggingIn) -> background thread is checking creds
//!   Ready              -> credentials confirmed; App picks them up
//!   Cancelled          -> user hit Cancel; App should exit

use std::sync::mpsc::{Receiver, channel};
use std::thread;

use crate::swarm::client::{Client, SwarmApi};
use crate::swarm::url::SwarmUrl;

#[derive(Debug)]
pub enum SwarmAuth {
    Pending {
        url: SwarmUrl,
        user_input: String,
        password_input: String,
        error: Option<String>,
        /// In-flight background result: `Ok((user, ticket))` or `Err(msg)`.
        rx: Option<Receiver<Result<(String, String), String>>>,
    },
    Ready { url: SwarmUrl, user: String, ticket: String },
    Cancelled,
}

impl SwarmAuth {
    /// Build the initial state, kicking off a probe if cached credentials exist.
    pub fn new(url: SwarmUrl) -> Self {
        let prefill_user = super::swarm_creds::load_user(&url.host).unwrap_or_default();
        let mut state = SwarmAuth::Pending {
            url: url.clone(),
            user_input: prefill_user,
            password_input: String::new(),
            error: None,
            rx: None,
        };
        if let Some((user, ticket)) = super::swarm_creds::load(&url.host) {
            let (tx, rx) = channel();
            let host = url.host.clone();
            let user2 = user.clone();
            let ticket2 = ticket.clone();
            thread::spawn(move || {
                let c = Client::new(host, user2.clone(), ticket2.clone());
                let res = c.probe().map(|_| (user2, ticket2)).map_err(|e| e.to_string());
                let _ = tx.send(res);
            });
            if let SwarmAuth::Pending { rx: r, .. } = &mut state {
                *r = Some(rx);
            }
        }
        state
    }
}

/// Render the modal. Returns `true` if the App should consume the auth
/// result (state advanced to Ready or Cancelled).
pub fn render(ui: &imgui::Ui, auth: &mut SwarmAuth) -> bool {
    // Try to advance a pending background result first.
    if let SwarmAuth::Pending { rx, .. } = auth {
        if let Some(r) = rx.as_ref() {
            if let Ok(result) = r.try_recv() {
                match result {
                    Ok((user, ticket)) => {
                        let url = match auth {
                            SwarmAuth::Pending { url, .. } => url.clone(),
                            _ => unreachable!(),
                        };
                        super::swarm_creds::store(&url.host, &user, &ticket);
                        *auth = SwarmAuth::Ready { url, user, ticket };
                        return true;
                    }
                    Err(e) => {
                        if let SwarmAuth::Pending { url, error, rx, .. } = auth {
                            super::swarm_creds::clear_ticket(&url.host);
                            *error = Some(e);
                            *rx = None;
                        }
                    }
                }
            }
        }
    }

    let SwarmAuth::Pending { url, user_input, password_input, error, rx } = auth else {
        return false;
    };

    let busy = rx.is_some();
    let mut want_login = false;
    let mut want_cancel = false;

    ui.open_popup("Swarm login");
    if let Some(_t) = ui.modal_popup_config("Swarm login")
        .always_auto_resize(true)
        .resizable(false)
        .begin_popup()
    {
        ui.text(format!("Host: {}", url.host));
        ui.separator();
        ui.input_text("Username", user_input).build();
        ui.input_text("Password", password_input).password(true).build();
        if let Some(e) = error.as_deref() {
            ui.text_colored([1.0, 0.4, 0.4, 1.0], e);
        }
        if busy { ui.text("Logging in\u{2026}"); }
        ui.separator();
        if !busy {
            if ui.button("Login") { want_login = true; }
            ui.same_line();
            if ui.button("Cancel") { want_cancel = true; }
        }
    }

    if want_login {
        let (tx, new_rx) = channel();
        let host = url.host.clone();
        let user = user_input.clone();
        let pw = password_input.clone();
        thread::spawn(move || {
            let c = Client::new(host.clone(), String::new(), String::new());
            let res = c.login(&user, &pw)
                .map(|t| (user, t))
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
        *rx = Some(new_rx);
        *error = None;
        password_input.clear();
    }

    if want_cancel {
        *auth = SwarmAuth::Cancelled;
        return true;
    }

    false
}
