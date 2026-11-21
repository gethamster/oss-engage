//! Leased, restart-safe notifications for registered watcher consumers.
//! A notification is only a hint: the receiver must pull and acknowledge its own matches.

use std::collections::BTreeSet;

use chrono::{Duration, SecondsFormat};
use engage_core::BoardError;
use serde_json::{Value, json};

use crate::{Board, db::Tx};

impl Board {
    /// Leases notifications for consumers with runnable watcher matches.
    ///
    /// # Errors
    /// Returns a storage error if leases cannot be read or recorded atomically.
    pub async fn next_wakes(&self, limit: usize) -> Result<Vec<Value>, BoardError> {
        let mut tx = Tx::begin(&self.pool).await?;
        let at = tx.at.to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut pending = BTreeSet::new();
        for item in tx.list("match").await? {
            if item["done"] == true
                || item["until"]
                    .as_str()
                    .is_some_and(|until| until > at.as_str())
                || item["retryAt"]
                    .as_str()
                    .is_some_and(|retry| retry > at.as_str())
            {
                continue;
            }
            if let Some(owner) = item["owner"].as_str() {
                pending.insert(owner.to_owned());
            }
        }
        let mut result = Vec::new();
        for registration in tx.list("registration").await? {
            let Some(owner) = registration["owner"]
                .as_str()
                .filter(|owner| pending.contains(*owner))
            else {
                continue;
            };
            let Some(url) = registration["url"].as_str() else {
                continue;
            };
            let previous = tx.get("wake", owner).await?.unwrap_or(Value::Null);
            if previous["until"]
                .as_str()
                .is_some_and(|until| until > at.as_str())
                || previous["retryAt"]
                    .as_str()
                    .is_some_and(|retry| retry > at.as_str())
            {
                continue;
            }
            let lease = uuid::Uuid::new_v4().simple().to_string();
            let value = json!({"owner":owner,"lease":lease,"until":(tx.at+Duration::seconds(30)).to_rfc3339_opts(SecondsFormat::Millis,true),
                "attempt":previous["attempt"].as_u64().unwrap_or(0),"hash":registration["hash"],"url":url});
            tx.put("wake", owner, &value).await?;
            result.push(json!({"owner":owner,"lease":lease,"url":url,"hash":registration["hash"]}));
            if result.len() >= limit.clamp(1, 50) {
                break;
            }
        }
        tx.commit().await?;
        Ok(result)
    }

    /// Records a notification result only while this sender still holds its lease.
    ///
    /// # Errors
    /// Returns `lease_lost` for an expired or superseded sender, or a storage error.
    pub async fn ack_wake(
        &self,
        owner: &str,
        lease: &str,
        delivered: bool,
    ) -> Result<(), BoardError> {
        let mut tx = Tx::begin(&self.pool).await?;
        let mut value = tx
            .get("wake", owner)
            .await?
            .ok_or_else(|| BoardError::missing("wake_not_found"))?;
        let at = tx.at.to_rfc3339_opts(SecondsFormat::Millis, true);
        if value["lease"] != lease
            || value["until"]
                .as_str()
                .is_none_or(|until| until <= at.as_str())
        {
            return Err(BoardError::conflict(
                "lease_lost",
                "notification lease expired",
            ));
        }
        let attempt = if delivered {
            0
        } else {
            value["attempt"].as_u64().unwrap_or(0).saturating_add(1)
        };
        let delay = if delivered { 5 } else { 1i64 << attempt.min(8) };
        value["attempt"] = attempt.into();
        value["lease"] = Value::Null;
        value["until"] = Value::Null;
        value["retryAt"] = (tx.at + Duration::seconds(delay))
            .to_rfc3339_opts(SecondsFormat::Millis, true)
            .into();
        tx.put("wake", owner, &value).await?;
        tx.commit().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engage_core::Principal;

    #[tokio::test]
    async fn wake_hints_are_leased_fenced_and_only_queued_for_pending_matches() {
        let board = Board::connect("sqlite::memory:")
            .await
            .expect("board opens");
        let owner = Principal::default();
        board.request(&owner,"POST","/watchers",&[],json!({"watch":{"agent":{"tail":"work"}},"url":"https://agent.example/channels/engage"})).await.expect("watcher registers");
        assert!(board.next_wakes(10).await.expect("wakes read").is_empty());
        let author = Principal {
            profile: Some("human".into()),
            ..owner.clone()
        };
        board
            .request(
                &author,
                "POST",
                "/messages",
                &[],
                json!({"channel":"work","text":"Please investigate"}),
            )
            .await
            .expect("message posts");
        let wakes = board.next_wakes(10).await.expect("notification leases");
        assert_eq!(
            wakes.len(),
            1,
            "one consumer notification per pending batch"
        );
        assert!(
            board
                .next_wakes(10)
                .await
                .expect("another sender checks")
                .is_empty(),
            "active lease prevents duplicate senders"
        );
        let wake = &wakes[0];
        let wrong = board
            .ack_wake(&owner.id, "stale", true)
            .await
            .expect_err("stale sender is fenced");
        assert_eq!(wrong.code, "lease_lost");
        board
            .ack_wake(
                &owner.id,
                wake["lease"].as_str().expect("lease exists"),
                false,
            )
            .await
            .expect("failed notification schedules retry");
        assert!(
            board.next_wakes(10).await.expect("retry checks").is_empty(),
            "retry uses durable backoff"
        );
    }
}
