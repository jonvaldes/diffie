//! HTTP client for Swarm's REST API. The [`SwarmApi`] trait is the seam
//! used by the loader so tests can substitute a fake.

use crate::swarm::model::{FileEntry, ReviewMeta};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("authentication failed")]
    Auth,
    #[error("not found")]
    NotFound,
    #[error("network: {0}")]
    Network(String),
    #[error("decode: {0}")]
    Decode(String),
}

pub trait SwarmApi: Send + Sync {
    fn login(&self, user: &str, password: &str) -> Result<String, Error>;
    fn get_review(&self, id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), Error>;
    fn get_change(&self, id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), Error>;
    /// Fetches the raw bytes of a depot file at the given revision.
    fn get_file_content(&self, depot_path: &str, rev: u32) -> Result<Vec<u8>, Error>;
    /// Cheap authenticated probe (used to detect expired tickets without prompting).
    fn probe(&self) -> Result<(), Error>;
}

#[cfg(feature = "gui")]
pub use ureq_impl::Client;

#[cfg(feature = "gui")]
mod ureq_impl {
    use super::*;
    use crate::swarm::model::{ChangeResponse, ReviewResponse};
    use std::io::Read;

    pub struct Client {
        agent: ureq::Agent,
        host: String,
        user: String,
        ticket: String,
    }

    impl Client {
        pub fn new(host: impl Into<String>, user: impl Into<String>, ticket: impl Into<String>) -> Self {
            Self {
                agent: ureq::AgentBuilder::new().build(),
                host: host.into(),
                user: user.into(),
                ticket: ticket.into(),
            }
        }

        fn auth(&self) -> String {
            use base64::Engine;
            let raw = format!("{}:{}", self.user, self.ticket);
            format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw))
        }

        fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
            let url = format!("{}{}", self.host, path);
            let res = self.agent.get(&url).set("Authorization", &self.auth()).call();
            match res {
                Ok(r) => r.into_json::<T>().map_err(|e| Error::Decode(e.to_string())),
                Err(ureq::Error::Status(401, _)) => Err(Error::Auth),
                Err(ureq::Error::Status(404, _)) => Err(Error::NotFound),
                Err(e) => Err(Error::Network(e.to_string())),
            }
        }
    }

    impl SwarmApi for Client {
        fn login(&self, user: &str, password: &str) -> Result<String, Error> {
            let url = format!("{}/api/v9/login", self.host);
            let res = self
                .agent
                .post(&url)
                .send_json(serde_json::json!({ "user": user, "password": password }));
            match res {
                Ok(r) => {
                    let v: serde_json::Value = r.into_json().map_err(|e| Error::Decode(e.to_string()))?;
                    v.get("ticket")
                        .or_else(|| v.get("user").and_then(|u| u.get("ticket")))
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| Error::Decode("no ticket in response".into()))
                }
                Err(ureq::Error::Status(401, _)) => Err(Error::Auth),
                Err(e) => Err(Error::Network(e.to_string())),
            }
        }

        fn get_review(&self, id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), Error> {
            let resp: ReviewResponse = self.get_json(&format!("/api/v9/reviews/{id}"))?;
            Ok(resp.into_meta_and_files(&self.host))
        }

        fn get_change(&self, id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), Error> {
            let resp: ChangeResponse = self.get_json(&format!("/api/v9/changes/{id}"))?;
            Ok(resp.into_meta_and_files(&self.host))
        }

        fn get_file_content(&self, depot_path: &str, rev: u32) -> Result<Vec<u8>, Error> {
            // Percent-encode the depot path; preserve '/' so segments stay visible.
            let encoded: String = depot_path.chars().map(|c| match c {
                '/' | 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c.to_string(),
                _ => format!("%{:02X}", c as u32),
            }).collect();
            let url = format!("{}/api/v10/files/{encoded}?rev={rev}&fields=content", self.host);
            let res = self.agent.get(&url).set("Authorization", &self.auth()).call();
            match res {
                Ok(r) => {
                    let mut buf = Vec::new();
                    r.into_reader().read_to_end(&mut buf).map_err(|e| Error::Network(e.to_string()))?;
                    Ok(buf)
                }
                Err(ureq::Error::Status(401, _)) => Err(Error::Auth),
                Err(ureq::Error::Status(404, _)) => Err(Error::NotFound),
                Err(e) => Err(Error::Network(e.to_string())),
            }
        }

        fn probe(&self) -> Result<(), Error> {
            let url = format!("{}/api/v9/projects?max=1", self.host);
            let res = self.agent.get(&url).set("Authorization", &self.auth()).call();
            match res {
                Ok(_) => Ok(()),
                Err(ureq::Error::Status(401, _)) => Err(Error::Auth),
                Err(e) => Err(Error::Network(e.to_string())),
            }
        }
    }
}
