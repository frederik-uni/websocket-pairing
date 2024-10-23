use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use rand::distributions::Alphanumeric;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

#[derive(Deserialize, Serialize)]
struct Action {
    action: u8,
    secret: Option<String>,
}

/// Dont handle CloseConnection to strictly as there can come connections after it
#[allow(unused)]
pub enum Codes {
    // Joins/Creates
    /// Creates or joins open game
    JoinOpen = 0,
    /// Creates closed game
    CreateClosed = 4,
    /// Joins closed game
    /// requires secret: String attribute
    JoinClosed = 6,
    // Info
    /// Return info from server
    /// Does not conain any data
    YouAreHostAndWaithing = 1,
    /// Return info from server
    /// Does not conain any data
    YouAreNotHostAndJoined = 2,
    /// Return info from server
    /// Does not conain any data
    PlayerJoinedAndReady = 3,
    // Actions
    /// Return info from server
    /// Contains key which is needed for JoinClosed
    /// has secret: String attribute
    SetClosedToken = 5,
    /// Connection was overwritten or user left
    CloseConnection = 9,
    // Errors
    /// Coudlnt find session
    NoValidSessionToken = 8,
    /// Didnt register session before
    NoSession = 7,
}

async fn close_session(session: &Option<Arc<Mutex<Session>>>) {
    if let Some(session) = session {
        let _ = session
            .lock()
            .await
            .send_message_to_sec("{action: 9}".to_string())
            .await;
        let _ = session
            .lock()
            .await
            .send_message_to_host("{action: 9}".to_string())
            .await;
    }
}

async fn handle_connection(
    raw_stream: tokio::net::TcpStream,
    sessions: Arc<Sessions>,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let ws_stream = accept_async(raw_stream).await?;
    let (ws_sender, mut ws_receiver) = ws_stream.split();
    let ws_sender = Arc::new(Mutex::new(ws_sender));
    let mut session: Option<Arc<Mutex<Session>>> = None;
    let mut host = true;
    while let Some(msg) = ws_receiver.next().await {
        if let Ok(Ok(text)) = msg.map(|v| v.to_text().map(|v| v.to_string())) {
            if let Ok(action) = serde_json::from_str::<Action>(&text) {
                match action.action {
                    0 => {
                        close_session(&session).await;
                        let v = sessions.open.lock().await.pop_front();
                        match v {
                            Some(ses) => {
                                ses.lock().await.set_sec(ws_sender.clone());
                                let _ = ses
                                    .as_ref()
                                    .lock()
                                    .await
                                    .send_message_to_host("{action: 3}".to_owned())
                                    .await;
                                let _ = ses
                                    .as_ref()
                                    .lock()
                                    .await
                                    .send_message_to_sec("{action: 2}".to_owned())
                                    .await;
                                session = Some(ses);
                                host = false;
                            }
                            None => {
                                let mut ses = Session::public(ws_sender.clone());
                                let _ = ses.send_message_to_host("{action: 1}".to_owned()).await;
                                let ses = Arc::new(Mutex::new(ses));
                                session = Some(ses.clone());
                                host = true;

                                sessions.open.lock().await.push_back(ses);
                            }
                        }
                    }
                    1 | 2 | 3 | 5 | 7 | 8 | 9 => {
                        continue;
                    }
                    4 => {
                        close_session(&session).await;
                        let mut ses = Session::public(ws_sender.clone());
                        let secret: String = rand::thread_rng()
                            .sample_iter(&Alphanumeric)
                            .take(6)
                            .map(char::from)
                            .collect();
                        let _ = ses
                            .send_message_to_host(format!(
                                "{}action: 5, secret: \"{secret}\"{}",
                                '{', '}'
                            ))
                            .await;
                        let ses = Arc::new(Mutex::new(ses));

                        session = Some(ses.clone());
                        host = true;
                        sessions.closed.lock().await.insert(secret, ses);
                    }
                    6 => {
                        close_session(&session).await;
                        let key = action.secret.unwrap_or_default();
                        let value = sessions.closed.lock().await.remove(&key);
                        match value {
                            Some(ses) => {
                                ses.lock().await.set_sec(ws_sender.clone());
                                let _ = ses
                                    .as_ref()
                                    .lock()
                                    .await
                                    .send_message_to_host("{action: 3}".to_owned())
                                    .await;
                                let _ = ses
                                    .as_ref()
                                    .lock()
                                    .await
                                    .send_message_to_sec("{action: 2}".to_owned())
                                    .await;
                                session = Some(ses);
                                host = false;
                            }
                            None => {
                                let _ = ws_sender
                                    .lock()
                                    .await
                                    .send(Message::Text("{action: 8}".to_owned()))
                                    .await;
                            }
                        }
                    }
                    _ => {
                        if let Some(session) = &session {
                            match host {
                                true => {
                                    let _ = session
                                        .lock()
                                        .await
                                        .send_message_to_sec(text.to_string())
                                        .await;
                                }
                                false => {
                                    let _ = session
                                        .lock()
                                        .await
                                        .send_message_to_host(text.to_string())
                                        .await;
                                }
                            }
                        } else {
                            let _ = ws_sender
                                .lock()
                                .await
                                .send(Message::Text("{action: 7}".to_owned()))
                                .await;
                        }
                    }
                }
            } else if let Some(session) = &session {
                match host {
                    true => {
                        let _ = session
                            .lock()
                            .await
                            .send_message_to_sec(text.to_string())
                            .await;
                    }
                    false => {
                        let _ = session
                            .lock()
                            .await
                            .send_message_to_host(text.to_string())
                            .await;
                    }
                }
            } else {
                let _ = ws_sender
                    .lock()
                    .await
                    .send(Message::Text("{action: 7}".to_owned()))
                    .await;
            }
        }
    }

    close_session(&session).await;
    Ok(())
}

#[derive(Default)]
struct Sessions {
    open: Mutex<VecDeque<Arc<Mutex<Session>>>>,
    closed: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
}

struct Session {
    sender_host: Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>,
    sender_sec: Option<Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>>,
}

impl Session {
    fn public(host: Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>) -> Self {
        Self {
            sender_host: host,
            sender_sec: None,
        }
    }

    fn set_sec(&mut self, sender: Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>) {
        self.sender_sec = Some(sender);
    }

    async fn send_message_to_host(
        &mut self,
        msg: String,
    ) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        self.sender_host
            .lock()
            .await
            .send(Message::Text(msg))
            .await?;
        Ok(())
    }

    async fn send_message_to_sec(
        &mut self,
        msg: String,
    ) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        if let Some(sender) = &self.sender_sec {
            sender.lock().await.send(Message::Text(msg)).await?;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("127.0.0.1:9001")
        .await
        .expect("Can't bind to address");
    let sessions = Arc::new(Sessions::default());
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(handle_connection(stream, sessions.clone()));
    }
}
