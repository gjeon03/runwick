//! Unix-socket server: one reader and one writer task per client.

use crate::host::{Client, Ctx, Host};
use crate::router::Router;
use runwick_core::rpc::{Message, ResBody, RpcError};
use runwick_core::wire::{Decoder, Frame};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

pub async fn serve(host: Arc<Host>, router: Arc<Router>, listener: UnixListener) {
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let peer = stream.peer_cred().ok().and_then(|c| c.pid()).and_then(|p| u32::try_from(p).ok());
                    let (r, w) = stream.into_split();
                    tokio::spawn(handle_client_from(host.clone(), router.clone(), r, w, peer));
                }
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
            _ = host.shutdown.notified() => break,
        }
    }
}

pub async fn handle_client<R, W>(host: Arc<Host>, router: Arc<Router>, reader: R, writer: W)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    handle_client_from(host, router, reader, writer, None).await
}

/// A client whose process id the socket reported (`peer`), when it did.
pub async fn handle_client_from<R, W>(
    host: Arc<Host>,
    router: Arc<Router>,
    mut reader: R,
    mut writer: W,
    peer: Option<u32>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
    let client = Arc::new(Client::new(host.next_client_id(), tx));
    if let Some(pid) = peer {
        client.set_peer_pid(pid);
    }
    host.bus.add(client.clone());
    tracing::debug!(client = client.id, "client connected");

    let (out_host, out_client) = (host.clone(), client.clone());
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let output = match &frame {
                Frame::Output { data, .. } => Some(data.len()),
                _ => None,
            };
            let bytes = match frame.encode() {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("encode failed: {e}");
                    // A request still gets an answer, or its caller would wait forever.
                    match too_large_answer(&frame) {
                        Some(b) => b,
                        None => continue,
                    }
                }
            };
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
            if let Some(n) = output
                && out_client.output_written(n)
            {
                // Caught up after dropping output: send those sessions a fresh snapshot.
                let (host, client) = (out_host.clone(), out_client.clone());
                tokio::task::spawn_blocking(move || crate::sessions::resync(&host, &client));
            }
            // Coalesce: flush only when nothing else is queued.
            if rx.is_empty() && writer.flush().await.is_err() {
                break;
            }
        }
    });

    let mut decoder = Decoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    'read: loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        decoder.push(&buf[..n]);
        loop {
            match decoder.next_frame() {
                Ok(Some(frame)) => on_frame(&host, &router, &client, frame),
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(client = client.id, "bad frame, closing: {e}");
                    break 'read;
                }
            }
        }
    }

    host.bus.remove(client.id);
    crate::sessions::on_client_gone(&host, client.id);
    writer_task.abort();
    tracing::debug!(client = client.id, "client disconnected");
}

/// The error answer for a response that could not be encoded (larger than a frame allows).
fn too_large_answer(frame: &Frame) -> Option<Vec<u8>> {
    let Frame::Message(Message::Res { id, .. }) = frame else {
        return None;
    };
    let err = RpcError::internal("응답이 너무 커서 보내지 못했어요");
    Frame::Message(Message::Res { id: *id, body: ResBody::Err(err) }).encode().ok()
}

fn on_frame(host: &Arc<Host>, router: &Arc<Router>, client: &Arc<Client>, frame: Frame) {
    match frame {
        Frame::Message(Message::Req { id, method, params }) => {
            // Requests run concurrently; settle a window claim before any later request of
            // this connection is checked against it.
            if method == "host.hello" {
                let interactive = params.get("interactive").and_then(serde_json::Value::as_bool) == Some(true);
                let token = params.get("token").and_then(serde_json::Value::as_str);
                crate::gui_token::apply_claim(host, client, interactive, token, true);
            }
            let ctx = Ctx { host: host.clone(), client: client.clone() };
            let router = router.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let body = match router.dispatch(ctx, &method, params).await {
                    Ok(v) => ResBody::Ok(v),
                    Err(e) => {
                        tracing::debug!(%method, "request failed: {e}");
                        ResBody::Err(e)
                    }
                };
                client.send(Frame::Message(Message::Res { id, body }));
            });
        }
        Frame::Input { session, data } => crate::sessions::on_input(host, client, session, data),
        Frame::Message(_) | Frame::Output { .. } => {
            tracing::debug!(client = client.id, "ignored unexpected frame from client");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_too_large_for_a_frame_is_answered_with_an_error() {
        let huge = serde_json::Value::String("x".repeat(runwick_core::wire::MAX_FRAME + 1));
        let res = Frame::Message(Message::Res { id: 7, body: ResBody::Ok(huge) });
        assert!(res.encode().is_err());
        let answer = too_large_answer(&res).expect("an answer");
        let mut d = runwick_core::wire::Decoder::new();
        d.push(&answer);
        match d.next_frame().unwrap() {
            Some(Frame::Message(Message::Res { id: 7, body: ResBody::Err(e) })) => {
                assert!(e.message.contains("너무 커서"), "{}", e.message)
            }
            other => panic!("{other:?}"),
        }
        // Only responses are answered this way; output frames are never that large.
        let event = Frame::Message(Message::Event { topic: "x".into(), data: serde_json::Value::Null });
        assert!(too_large_answer(&event).is_none());
    }
}
