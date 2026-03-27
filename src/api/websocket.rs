use axum::extract::ws::{Message, WebSocket};
use axum::extract::State;
use axum::extract::WebSocketUpgrade;
use axum::http::header::SEC_WEBSOCKET_PROTOCOL;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use tokio::sync::broadcast;

use crate::api::auth::HttpAuth;

#[derive(Clone)]
pub struct WsState {
    pub auth: HttpAuth,
    pub event_tx: broadcast::Sender<String>,
}

/// WebSocket upgrade handler. Auth via `Sec-WebSocket-Protocol`: the browser
/// sends the password as a subprotocol value. We check it against both passwords
/// and reject with 401 if neither matches.
pub async fn handler(
    State(state): State<WsState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    // Sec-WebSocket-Protocol header may contain comma-separated protocol values.
    let protocol = headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .flat_map(|v| {
            v.to_str()
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim().to_string())
        })
        .find(|p| p == &state.auth.full_password || p == &state.auth.read_only_password)
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let rx = state.event_tx.subscribe();

    Ok(ws
        .protocols([protocol])
        .on_upgrade(move |socket| stream_events(socket, rx)))
}

async fn stream_events(mut socket: WebSocket, mut rx: broadcast::Receiver<String>) {
    loop {
        match rx.recv().await {
            Ok(msg) => {
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite;

    fn test_state() -> WsState {
        let (tx, _) = broadcast::channel(16);
        WsState {
            auth: HttpAuth {
                full_password: "full_secret".into(),
                read_only_password: "readonly_secret".into(),
            },
            event_tx: tx,
        }
    }

    fn test_router(state: WsState) -> axum::Router {
        axum::Router::new()
            .route("/websocket", axum::routing::get(handler))
            .with_state(state)
    }

    async fn serve(router: axum::Router) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        port
    }

    #[tokio::test]
    async fn rejects_without_protocol() {
        let state = test_state();
        let port = serve(test_router(state)).await;

        let err = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/websocket"))
            .await
            .unwrap_err();

        match err {
            tungstenite::Error::Http(resp) => {
                assert_eq!(resp.status(), 401);
            }
            other => panic!("expected HTTP error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_wrong_password() {
        let state = test_state();
        let port = serve(test_router(state)).await;

        let request = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/websocket"))
            .header("Sec-WebSocket-Protocol", "wrong_password")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Host", format!("127.0.0.1:{port}"))
            .body(())
            .unwrap();

        let err = tokio_tungstenite::connect_async(request).await.unwrap_err();

        match err {
            tungstenite::Error::Http(resp) => {
                assert_eq!(resp.status(), 401);
            }
            other => panic!("expected HTTP error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn accepts_full_password_and_echoes_protocol() {
        let state = test_state();
        let port = serve(test_router(state)).await;

        let request = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/websocket"))
            .header("Sec-WebSocket-Protocol", "full_secret")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Host", format!("127.0.0.1:{port}"))
            .body(())
            .unwrap();

        let (_, resp) = tokio_tungstenite::connect_async(request).await.unwrap();
        let protocol = resp.headers().get("Sec-WebSocket-Protocol").unwrap();
        assert_eq!(protocol, "full_secret");
    }

    #[tokio::test]
    async fn accepts_readonly_password() {
        let state = test_state();
        let port = serve(test_router(state)).await;

        let request = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/websocket"))
            .header("Sec-WebSocket-Protocol", "readonly_secret")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Host", format!("127.0.0.1:{port}"))
            .body(())
            .unwrap();

        let (_, resp) = tokio_tungstenite::connect_async(request).await.unwrap();
        let protocol = resp.headers().get("Sec-WebSocket-Protocol").unwrap();
        assert_eq!(protocol, "readonly_secret");
    }

    #[tokio::test]
    async fn broadcasts_events_to_connected_client() {
        let state = test_state();
        let tx = state.event_tx.clone();
        let port = serve(test_router(state)).await;

        let request = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/websocket"))
            .header("Sec-WebSocket-Protocol", "full_secret")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Host", format!("127.0.0.1:{port}"))
            .body(())
            .unwrap();

        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();

        // Small delay so the subscription is active before sending.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let payload = r#"{"event":"payment_received","paymentHash":"abc123"}"#;
        tx.send(payload.to_string()).unwrap();

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .expect("timed out waiting for message")
            .expect("stream ended")
            .expect("ws error");

        match msg {
            tungstenite::Message::Text(text) => {
                assert_eq!(text, payload);
            }
            other => panic!("expected text message, got: {other:?}"),
        }

        // Clean close.
        ws.close(None).await.ok();
    }

    #[tokio::test]
    async fn accepts_password_in_comma_separated_list() {
        let state = test_state();
        let port = serve(test_router(state)).await;

        let request = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/websocket"))
            .header("Sec-WebSocket-Protocol", "bogus, full_secret")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Host", format!("127.0.0.1:{port}"))
            .body(())
            .unwrap();

        let (_, resp) = tokio_tungstenite::connect_async(request).await.unwrap();
        let protocol = resp.headers().get("Sec-WebSocket-Protocol").unwrap();
        assert_eq!(protocol, "full_secret");
    }

    #[tokio::test]
    async fn survives_lagged_receiver() {
        // Channel capacity of 2 — easy to overflow.
        let (tx, _) = broadcast::channel(2);
        let state = WsState {
            auth: HttpAuth {
                full_password: "full_secret".into(),
                read_only_password: "readonly_secret".into(),
            },
            event_tx: tx.clone(),
        };
        let port = serve(test_router(state)).await;

        let request = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/websocket"))
            .header("Sec-WebSocket-Protocol", "full_secret")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Host", format!("127.0.0.1:{port}"))
            .body(())
            .unwrap();

        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Overflow the buffer (capacity 2) without reading — causes Lagged.
        for i in 0..5 {
            let _ = tx.send(format!("msg_{i}"));
        }

        // Send one more after the lag — client should still receive this.
        let final_msg = r#"{"event":"final"}"#;
        tx.send(final_msg.to_string()).unwrap();

        // Drain until we see the final message (earlier ones may be lost to lag).
        let received = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(Ok(tungstenite::Message::Text(text))) = ws.next().await {
                if text == final_msg {
                    return true;
                }
            }
            false
        })
        .await
        .expect("timed out");

        assert!(received, "should have received the final message after lag");
        ws.close(None).await.ok();
    }
}
