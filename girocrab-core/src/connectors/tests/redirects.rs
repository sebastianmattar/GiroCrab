//! Credential-posting clients must not follow a redirect to another origin:
//! on 307/308 the HTTP client would send the login body there again.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::connectors::{http_client_same_host, same_host_redirects};

/// Counts the requests that reach a local port; returns its base URL.
fn collector() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let _ = s.read(&mut [0u8; 4096]);
            counter.fetch_add(1, Ordering::SeqCst);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    (url, hits)
}

/// Answers every request with `status` and `Location: <target>` (`{SELF}` = own URL).
fn redirector(status: u16, target: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let location = target.replace("{SELF}", &url);
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let _ = s.read(&mut [0u8; 4096]);
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            );
        }
    });
    url
}

/// Sends a GET to a server that answers 302 with a redirect to another port.
/// Asserts that the client follows it (used for document downloads).
pub(crate) async fn assert_follows_cross_origin_get(client: &reqwest::Client) {
    let (target, hits) = collector();
    let start = redirector(302, format!("{target}/doc.pdf"));
    let resp = client.get(format!("{start}/doc")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

/// Posts a dummy login form to a server that answers 307 and 308 with a
/// redirect to another port. Asserts that the other port receives nothing.
pub(crate) async fn assert_no_cross_origin_replay(client: &reqwest::Client) {
    for status in [307, 308] {
        let (target, hits) = collector();
        let start = redirector(status, format!("{target}/collect"));
        let resp = client
            .post(format!("{start}/login"))
            .form(&[("user", "dummy"), ("pin", "0000")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), status);
        assert_eq!(hits.load(Ordering::SeqCst), 0, "body replayed on {status}");
    }
}

#[tokio::test]
async fn same_host_client_stops_at_another_port() {
    assert_no_cross_origin_replay(&http_client_same_host().unwrap()).await;
}

#[tokio::test]
async fn same_host_policy_stops_at_another_host() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(same_host_redirects())
        .build()
        .unwrap();
    let (target, hits) = collector();
    let port = target.rsplit(':').next().unwrap();
    let start = redirector(307, format!("http://localhost:{port}/collect"));
    let resp = client.post(&start).body("pin=0000").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 307);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn same_host_policy_follows_same_origin() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(same_host_redirects())
        .build()
        .unwrap();
    // The redirector points to itself: the chain ends after ten hops.
    let start = redirector(302, "{SELF}/next".into());
    let resp = client.get(&start).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 302);
    assert!(resp.url().path().ends_with("/next"));
}
