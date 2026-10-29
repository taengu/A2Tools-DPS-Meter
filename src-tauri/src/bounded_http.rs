//! Small HTTP replies are bounded while they arrive, including chunked bodies
//! without a Content-Length. Checking after `Response::bytes` is too late: that
//! method has already collected the whole reply.

use std::time::Duration;

const BODY_TIMEOUT: Duration = Duration::from_secs(30);

/// Read at most `limit` response bytes. Status handling belongs to the caller,
/// so an error reply can be read under the same limit as a successful one.
pub async fn bytes(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    bytes_with_timeout(response, limit, BODY_TIMEOUT).await
}

async fn bytes_with_timeout(
    mut response: reqwest::Response,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(format!("HTTP response exceeds {limit} bytes"));
    }
    tokio::time::timeout(timeout, async {
        let mut body = Vec::with_capacity(limit.min(8 * 1024));
        while let Some(chunk) = response.chunk().await.map_err(|e| format!("HTTP response body: {e}"))? {
            // Test before extending, without overflowing even at a very large
            // limit. Content-Length is only an early rejection, never a reason
            // to trust the amount the server actually sends.
            if chunk.len() > limit - body.len() {
                return Err(format!("HTTP response exceeds {limit} bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    })
    .await
    .map_err(|_| "HTTP response body timed out".to_string())?
}

/// Read a bounded UTF-8 reply. Malformed UTF-8 is refused rather than changed
/// before a signature or JSON parser sees it.
pub async fn text(response: reqwest::Response, limit: usize) -> Result<String, String> {
    String::from_utf8(bytes(response, limit).await?)
        .map_err(|_| "HTTP response body is not UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A local HTTP peer with the exact wire framing under test. No external
    /// endpoint or proxy is involved.
    async fn response(wire: &'static [u8]) -> reqwest::Response {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut block = [0u8; 512];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut block).await.unwrap();
                if n == 0 || request.len() + n > 8 * 1024 {
                    return;
                }
                request.extend_from_slice(&block[..n]);
            }
            let _ = socket.write_all(wire).await;
        });
        reqwest::Client::builder().no_proxy().build().unwrap()
            .get(format!("http://{address}/")).send().await.unwrap()
    }

    #[tokio::test]
    async fn an_oversized_declared_body_is_refused_before_waiting_for_it() {
        let reply = response(b"HTTP/1.1 200 OK\r\nContent-Length: 4294967296\r\n\r\n").await;
        assert_eq!(bytes(reply, 5).await.unwrap_err(), "HTTP response exceeds 5 bytes");
    }

    #[tokio::test]
    async fn a_chunked_body_cannot_grow_past_the_limit() {
        let reply = response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n").await;
        assert_eq!(bytes(reply, 5).await.unwrap_err(), "HTTP response exceeds 5 bytes");
    }

    #[tokio::test]
    async fn a_body_without_a_length_is_also_bounded() {
        let reply = response(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabcdef").await;
        assert_eq!(bytes(reply, 5).await.unwrap_err(), "HTTP response exceeds 5 bytes");
    }

    #[tokio::test]
    async fn the_exact_limit_and_an_empty_reply_are_accepted() {
        let reply = response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").await;
        assert_eq!(bytes(reply, 5).await.unwrap(), b"abcde");
        let reply = response(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        assert!(bytes(reply, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn incomplete_bodies_and_invalid_text_are_errors() {
        let reply = response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nab").await;
        assert!(bytes(reply, 5).await.unwrap_err().starts_with("HTTP response body:"));
        let reply = response(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n\xff").await;
        assert_eq!(text(reply, 5).await.unwrap_err(), "HTTP response body is not UTF-8");
    }

    #[tokio::test]
    async fn a_body_read_has_an_absolute_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n").await.unwrap();
            std::future::pending::<()>().await;
        });
        let reply = reqwest::Client::builder().no_proxy().build().unwrap()
            .get(format!("http://{address}/")).send().await.unwrap();
        let result = bytes_with_timeout(reply, 5, Duration::from_millis(20)).await;
        server.abort();
        assert_eq!(result.unwrap_err(), "HTTP response body timed out");
    }
}
