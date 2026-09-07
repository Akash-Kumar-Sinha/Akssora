use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxEvent {
    Created,
    Paused,
    Resumed,
    Destroyed,
    Crashed,
    Forked,
}

impl std::fmt::Display for SandboxEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxEvent::Created => write!(f, "created"),
            SandboxEvent::Paused => write!(f, "paused"),
            SandboxEvent::Resumed => write!(f, "resumed"),
            SandboxEvent::Destroyed => write!(f, "destroyed"),
            SandboxEvent::Crashed => write!(f, "crashed"),
            SandboxEvent::Forked => write!(f, "forked"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    pub event: SandboxEvent,
    pub sandbox_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRegistration {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default)]
    pub events: Vec<SandboxEvent>,
    pub created_at: DateTime<Utc>,
}

pub struct WebhookDispatcher {
    registrations: Arc<dashmap::DashMap<Uuid, WebhookRegistration>>,
    http_client: reqwest::Client,
}

impl WebhookDispatcher {
    pub fn new() -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        Self {
            registrations: Arc::new(dashmap::DashMap::new()),
            http_client,
        }
    }

    pub fn register(&self, reg: WebhookRegistration) {
        tracing::info!(webhook_id = %reg.id, url = %reg.url, "webhook registered");
        self.registrations.insert(reg.id, reg);
    }

    pub fn unregister(&self, id: &Uuid) -> bool {
        let removed = self.registrations.remove(id).is_some();
        if removed {
            tracing::info!(webhook_id = %id, "webhook unregistered");
        }
        removed
    }

    pub fn list_for_owner(&self, owner_id: &Uuid) -> Vec<WebhookRegistration> {
        self.registrations
            .iter()
            .filter(|r| r.value().owner_id == *owner_id)
            .map(|r| r.value().clone())
            .collect()
    }

    pub fn dispatch(&self, payload: WebhookPayload) {
        let matching: Vec<WebhookRegistration> = self
            .registrations
            .iter()
            .filter(|r| {
                let reg = r.value();
                reg.events.is_empty() || reg.events.contains(&payload.event)
            })
            .map(|r| r.value().clone())
            .collect();

        if matching.is_empty() {
            return;
        }

        let client = self.http_client.clone();

        for reg in matching {
            let payload = payload.clone();
            let client = client.clone();

            tokio::spawn(async move {
                deliver_with_retry(&client, &reg, &payload).await;
            });
        }
    }
}

impl Default for WebhookDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

async fn deliver_with_retry(
    client: &reqwest::Client,
    reg: &WebhookRegistration,
    payload: &WebhookPayload,
) {
    let body = match serde_json::to_vec(payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize webhook payload");
            return;
        }
    };

    let max_attempts = 3;
    let mut delay_ms = 1000u64;

    for attempt in 1..=max_attempts {
        let mut request = client
            .post(&reg.url)
            .header("Content-Type", "application/json")
            .header("X-Akssora-Event", format!("{}", payload.event))
            .header("X-Akssora-Delivery", &payload.sandbox_id.to_string())
            .body(body.clone());

        if let Some(secret) = &reg.secret {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            type HmacSha256 = Hmac<Sha256>;

            let mut mac =
                HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
            mac.update(&body);
            let signature = hex::encode(mac.finalize().into_bytes());
            request = request.header("X-Akssora-Signature", format!("sha256={}", signature));
        }

        match request.send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(
                    webhook_id = %reg.id,
                    attempt,
                    status = %resp.status(),
                    "webhook delivered"
                );
                return;
            }
            Ok(resp) => {
                tracing::warn!(
                    webhook_id = %reg.id,
                    attempt,
                    status = %resp.status(),
                    "webhook delivery returned non-2xx"
                );
            }
            Err(e) => {
                tracing::warn!(
                    webhook_id = %reg.id,
                    attempt,
                    error = %e,
                    "webhook delivery failed"
                );
            }
        }

        if attempt < max_attempts {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            delay_ms *= 2;
        }
    }

    tracing::error!(
        webhook_id = %reg.id,
        attempts = max_attempts,
        "webhook delivery failed after all retries"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_payload_serializes() {
        let payload = WebhookPayload {
            event: SandboxEvent::Created,
            sandbox_id: Uuid::new_v4(),
            owner_id: Some(Uuid::new_v4()),
            timestamp: Utc::now(),
            metadata: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("created"));
    }

    #[test]
    fn register_and_list() {
        let dispatcher = WebhookDispatcher::new();
        let owner = Uuid::new_v4();

        let reg = WebhookRegistration {
            id: Uuid::new_v4(),
            owner_id: owner,
            url: "https://example.com/hook".to_string(),
            secret: None,
            events: vec![SandboxEvent::Created],
            created_at: Utc::now(),
        };

        dispatcher.register(reg.clone());
        let listed = dispatcher.list_for_owner(&owner);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, reg.id);
    }

    #[test]
    fn unregister() {
        let dispatcher = WebhookDispatcher::new();
        let id = Uuid::new_v4();

        let reg = WebhookRegistration {
            id,
            owner_id: Uuid::new_v4(),
            url: "https://example.com/hook".to_string(),
            secret: None,
            events: vec![],
            created_at: Utc::now(),
        };

        dispatcher.register(reg);
        assert!(dispatcher.unregister(&id));
        assert!(!dispatcher.unregister(&id));
    }

    #[test]
    fn event_matching() {
        let dispatcher = WebhookDispatcher::new();
        let owner = Uuid::new_v4();

        dispatcher.register(WebhookRegistration {
            id: Uuid::new_v4(),
            owner_id: owner,
            url: "https://a.com".to_string(),
            secret: None,
            events: vec![SandboxEvent::Created, SandboxEvent::Destroyed],
            created_at: Utc::now(),
        });

        dispatcher.register(WebhookRegistration {
            id: Uuid::new_v4(),
            owner_id: owner,
            url: "https://b.com".to_string(),
            secret: None,
            events: vec![],
            created_at: Utc::now(),
        });

        let matching: Vec<_> = dispatcher
            .registrations
            .iter()
            .filter(|r| {
                let reg = r.value();
                reg.events.is_empty()
                    || reg.events.contains(&SandboxEvent::Created)
            })
            .collect();

        assert_eq!(matching.len(), 2);

        let matching: Vec<_> = dispatcher
            .registrations
            .iter()
            .filter(|r| {
                let reg = r.value();
                reg.events.is_empty()
                    || reg.events.contains(&SandboxEvent::Paused)
            })
            .collect();

        assert_eq!(matching.len(), 1);
    }
}
