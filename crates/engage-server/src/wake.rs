use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{StreamExt, stream};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::http::App;

pub(crate) fn spawn(app: Arc<App>, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {()=cancel.cancelled()=>break,_=interval.tick()=>{}}
            let operation = async {
                let batch = match app.board.next_wakes(8).await {
                    Ok(batch) => batch,
                    Err(cause) => {
                        tracing::warn!(code = cause.code, "cannot lease watcher notifications");
                        return;
                    }
                };
                stream::iter(batch)
                    .map(|wake| {
                        let app = app.clone();
                        async move {
                            let delivered = send(&wake, app.config.wake_allow_private).await;
                            let owner = wake["owner"].as_str().unwrap_or("");
                            let lease = wake["lease"].as_str().unwrap_or("");
                            if let Err(cause) = app.board.ack_wake(owner, lease, delivered).await {
                                tracing::warn!(
                                    code = cause.code,
                                    "cannot acknowledge watcher notification"
                                );
                            }
                        }
                    })
                    .buffer_unordered(4)
                    .collect::<Vec<_>>()
                    .await;
            };
            tokio::select! {()=cancel.cancelled()=>break,()=operation=>{}}
        }
    })
}

async fn send(wake: &Value, allow_private: bool) -> bool {
    let Some(address) = wake["url"].as_str() else {
        return false;
    };
    let Ok(mut url) = url::Url::parse(address) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.scheme() != "https" && !(allow_private && url.scheme() == "http")
    {
        return false;
    }
    let Some(host) = url.host_str().map(|host| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned()
    }) else {
        return false;
    };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    let Ok(Ok(resolved)) = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    else {
        return false;
    };
    let addresses: Vec<SocketAddr> = resolved.take(33).collect();
    if addresses.is_empty()
        || addresses.len() > 32
        || !allow_private && addresses.iter().any(|address| !public_ip(address.ip()))
    {
        return false;
    }
    url.set_path(&format!("{}/wake", url.path().trim_end_matches('/')));
    // Pin the checked DNS answer so a second resolution cannot rebind to an internal host.
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&host, &addresses)
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return false;
    };
    client
        .post(url)
        .header("x-board-watchers", wake["hash"].as_str().unwrap_or(""))
        .header("user-agent", "oss-engage/0.1 watcher-wake")
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

fn public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 198 && matches!(b, 18 | 19))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4() {
                return public_ip(IpAddr::V4(v4));
            }
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] == 0xdb8 || s[1] == 0))
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] & 0xfff0) == 0x0010)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_cannot_resolve_to_private_or_special_addresses_by_default() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "0.1.2.3",
            "224.0.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                !public_ip(address.parse().expect("address parses")),
                "reject {address}"
            );
        }
        assert!(public_ip("1.1.1.1".parse().expect("public address parses")));
        assert!(public_ip(
            "2606:4700:4700::1111".parse().expect("public IPv6 parses")
        ));
    }

    #[tokio::test]
    async fn explicit_local_notifications_send_only_hash_without_credentials_or_payload() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("callback binds");
        let address = listener.local_addr().expect("callback address exists");
        let wake = serde_json::json!({"url":format!("http://{address}/channels/engage"),"hash":"registered-hash"});
        assert!(
            !send(&wake, false).await,
            "private callbacks require explicit configuration"
        );
        let receiver = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("notification arrives");
            let mut bytes = vec![0; 4096];
            let n = socket.read(&mut bytes).await.expect("notification reads");
            let request = String::from_utf8_lossy(&bytes[..n]).into_owned();
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("callback responds");
            request
        });
        assert!(send(&wake, true).await, "explicit local callback succeeds");
        let request = receiver.await.expect("receiver completes");
        assert!(request.starts_with("POST /channels/engage/wake HTTP/1.1"));
        assert!(request.contains("x-board-watchers: registered-hash"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(request.ends_with("\r\n\r\n"), "wake carries no payload");
    }
}
