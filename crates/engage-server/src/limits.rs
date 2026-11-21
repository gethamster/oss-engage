use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engage_core::BoardError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error;

const MAX_IDENTITIES: usize = 4096;

/// Bounded transport limits. Database leases remain the authority for task ownership.
pub(crate) struct Limits {
    requests: Mutex<HashMap<String, (Instant, u32)>>,
    streams: Mutex<HashMap<String, (Instant, Arc<Semaphore>)>>,
    requests_per_minute: u32,
    streams_per_principal: usize,
}

impl Limits {
    pub fn new(requests_per_minute: u32, streams_per_principal: usize) -> Self {
        Self {
            requests: Mutex::default(),
            streams: Mutex::default(),
            requests_per_minute,
            streams_per_principal,
        }
    }

    pub fn request(&self, peer: &str) -> Result<(), BoardError> {
        let mut requests = self.requests.lock().expect("request limiter lock poisoned");
        let now = Instant::now();
        if requests.len() >= MAX_IDENTITIES {
            requests.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        }
        if !requests.contains_key(peer) && requests.len() >= MAX_IDENTITIES {
            return Err(limited());
        }
        let (start, count) = requests.entry(peer.to_owned()).or_insert((now, 0));
        if now.duration_since(*start) >= Duration::from_secs(60) {
            *start = now;
            *count = 0;
        }
        if *count >= self.requests_per_minute {
            return Err(limited());
        }
        *count += 1;
        Ok(())
    }

    pub fn stream(&self, principal: &str) -> Result<OwnedSemaphorePermit, BoardError> {
        let mut streams = self.streams.lock().expect("stream limiter lock poisoned");
        let now = Instant::now();
        if streams.len() >= MAX_IDENTITIES {
            streams.retain(|_, (last, semaphore)| {
                now.duration_since(*last) < Duration::from_secs(300)
                    || semaphore.available_permits() < self.streams_per_principal
            });
        }
        if !streams.contains_key(principal) && streams.len() >= MAX_IDENTITIES {
            return Err(limited());
        }
        let (last, semaphore) = streams
            .entry(principal.to_owned())
            .or_insert_with(|| (now, Arc::new(Semaphore::new(self.streams_per_principal))));
        *last = now;
        semaphore.clone().try_acquire_owned().map_err(|_| limited())
    }
}

fn limited() -> BoardError {
    error(
        429,
        "rate_limited",
        "request allowance is exhausted; retry shortly",
    )
}
