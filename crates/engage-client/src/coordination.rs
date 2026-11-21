use std::time::Duration;

use serde_json::{Value, json};

use crate::{Client, ClientError, TaskClaim};

impl Client {
    /// Creates a batch of tasks atomically, including references to tasks in the batch.
    pub async fn create_tasks(&self, body: &Value) -> Result<Value, ClientError> {
        self.write("/tasks/batch", body).await
    }
    /// Claims one task and returns its private fencing credentials once.
    pub async fn claim_task(
        &self,
        id: &str,
        lease: Option<Duration>,
    ) -> Result<Value, ClientError> {
        let mut body = json!({});
        if let Some(lease) = lease {
            body["lease"] = lease.as_secs_f64().into();
        }
        self.task_action(id, "claim", &body).await
    }
    /// Claims a ready task using channel, owner, label, and wait options.
    pub async fn claim_ready(&self, body: &Value) -> Result<Value, ClientError> {
        self.write("/tasks/claim", body).await
    }
    /// Renews a claim while preserving its fencing epoch and token.
    pub async fn renew_task(
        &self,
        id: &str,
        claim: &TaskClaim,
        lease: Option<Duration>,
    ) -> Result<Value, ClientError> {
        let mut body = capability(claim);
        if let Some(lease) = lease {
            body["lease"] = lease.as_secs_f64().into();
        }
        self.task_action(id, "renew", &body).await
    }
    /// Returns a claimed task to the ready queue.
    pub async fn release_task(&self, id: &str, claim: &TaskClaim) -> Result<Value, ClientError> {
        self.task_action(id, "release", &capability(claim)).await
    }
    /// Completes a claimed task with optional proof or a result summary.
    pub async fn complete_task(
        &self,
        id: &str,
        claim: &TaskClaim,
        result: Option<&str>,
    ) -> Result<Value, ClientError> {
        let mut body = capability(claim);
        if let Some(result) = result {
            body["result"] = result.into();
        }
        self.task_action(id, "complete", &body).await
    }
    /// Fails a claimed task, leaving its dependents waiting.
    pub async fn fail_task(
        &self,
        id: &str,
        claim: &TaskClaim,
        reason: &str,
    ) -> Result<Value, ClientError> {
        let mut body = capability(claim);
        body["reason"] = reason.into();
        self.task_action(id, "fail", &body).await
    }
    /// Registers watchers and optional sync entries for the authenticated principal.
    pub async fn register_watchers(&self, registration: &Value) -> Result<Value, ClientError> {
        self.write("/watchers", registration).await
    }
    /// Pulls the caller's leased watcher matches.
    pub async fn matches(&self, query: &[(String, String)]) -> Result<Value, ClientError> {
        self.request("GET", "/watchers/matches", query, None).await
    }
    /// Acknowledges a match, or reports an error for a retry.
    pub async fn acknowledge_match(
        &self,
        id: &str,
        lease: &str,
        error: Option<&str>,
    ) -> Result<Value, ClientError> {
        let mut body = json!({"lease":lease});
        if let Some(error) = error {
            body["error"] = error.into();
        }
        self.write(
            &format!("/watchers/matches/{}/ack", Self::segment(id)),
            &body,
        )
        .await
    }
    /// Creates a one-time or recurring scheduled message.
    pub async fn schedule(&self, body: &Value) -> Result<Value, ClientError> {
        self.write("/schedules", body).await
    }
    /// Lists schedules visible to the caller.
    pub async fn schedules(&self, query: &[(String, String)]) -> Result<Value, ClientError> {
        self.request("GET", "/schedules", query, None).await
    }
    /// Cancels a schedule without deleting its history.
    pub async fn cancel_schedule(&self, id: &str) -> Result<Value, ClientError> {
        self.write(
            &format!("/schedules/{}/cancel", Self::segment(id)),
            &json!({}),
        )
        .await
    }
    /// Lists immutable schema versions.
    pub async fn schemas(&self) -> Result<Value, ClientError> {
        self.request("GET", "/schemas", &[], None).await
    }
    /// Reads a schema by name and version, such as `sandbox@1`.
    pub async fn schema(&self, id: &str) -> Result<Value, ClientError> {
        self.request("GET", &format!("/schemas/{}", Self::segment(id)), &[], None)
            .await
    }
    /// Registers an immutable JSON Schema version.
    pub async fn register_schema(&self, body: &Value) -> Result<Value, ClientError> {
        self.write("/schemas", body).await
    }
    /// Lists grants. Requires board-wide management permission.
    pub async fn grants(&self) -> Result<Value, ClientError> {
        self.request("GET", "/grants", &[], None).await
    }
    /// Creates one grant.
    pub async fn create_grant(&self, body: &Value) -> Result<Value, ClientError> {
        self.write("/grants", body).await
    }
    /// Creates a grant batch atomically, including combined lockout checks.
    pub async fn create_grants(&self, body: &Value) -> Result<Value, ClientError> {
        self.write("/grants/batch", body).await
    }
    /// Pushes deduplicated provider events into a sync inbox.
    pub async fn push_inbox(&self, key: &str, body: &Value) -> Result<Value, ClientError> {
        self.write(&format!("/inbox/{}", Self::segment(key)), body)
            .await
    }
    /// Pulls leased inbox events, ordered within external threads.
    pub async fn next_inbox(
        &self,
        key: &str,
        query: &[(String, String)],
    ) -> Result<Value, ClientError> {
        self.request(
            "GET",
            &format!("/inbox/{}/next", Self::segment(key)),
            query,
            None,
        )
        .await
    }
    /// Imports a provider write through the server's authenticated sync boundary.
    pub async fn import(&self, body: &Value) -> Result<Value, ClientError> {
        self.write("/sync/imports", body).await
    }
    /// Records a provider receipt for delivery, attachment, or failure.
    pub async fn receipt(&self, body: &Value) -> Result<Value, ClientError> {
        self.write("/sync/receipts", body).await
    }
}

fn capability(claim: &TaskClaim) -> Value {
    json!({"epoch":claim.epoch,"token":claim.token})
}
