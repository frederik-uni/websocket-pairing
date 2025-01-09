#[cfg(feature = "proxy")]
mod bore;

use std::collections::{HashMap, VecDeque};
#[cfg(feature = "proxy")]
use std::env::args;
use std::sync::Arc;

#[cfg(feature = "proxy")]
use bore::Client;
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

type MySession = Option<Arc<Mutex<Session>>>;
type WsSender = Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>;
type MyHost = Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>;

impl Action {
    async fn run(
        &self,
        host: &mut bool,
        session: &mut MySession,
        sessions: &Arc<Sessions>,
        ws_sender: &WsSender,
        text: String,
    ) -> bool {
        match self.action {
            0 => {
                close_session(session).await;
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
                        *session = Some(ses);
                        *host = false;
                        false
                    }
                    None => {
                        let mut ses = Session::public(ws_sender.clone());
                        let _ = ses.send_message_to_host("{action: 1}".to_owned()).await;
                        let ses = Arc::new(Mutex::new(ses));
                        *session = Some(ses.clone());
                        *host = true;

                        sessions.open.lock().await.push_back(ses);
                        false
                    }
                }
            }
            1 | 2 | 3 | 5 | 7 | 8 => true,
            4 => {
                close_session(session).await;
                let mut ses = Session::public(ws_sender.clone());
                let secret: String = rand::thread_rng()
                    .sample_iter(&Alphanumeric)
                    .take(6)
                    .map(char::from)
                    .collect();
                let _ = ses
                    .send_message_to_host(format!("{}action: 5, secret: \"{secret}\"{}", '{', '}'))
                    .await;
                let ses = Arc::new(Mutex::new(ses));

                *session = Some(ses.clone());
                *host = true;
                sessions.closed.lock().await.insert(secret, ses);
                false
            }
            6 => {
                close_session(session).await;
                let key = self.secret.clone().unwrap_or_default();
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
                        *session = Some(ses);
                        *host = false;
                    }
                    None => {
                        let _ = ws_sender
                            .lock()
                            .await
                            .send(Message::Text("{action: 8}".to_owned()))
                            .await;
                    }
                }
                false
            }
            9 => {
                close_session(session).await;
                false
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
                false
            }
        }
    }
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

async fn close_session(session: &mut Option<Arc<Mutex<Session>>>) {
    if let Some(session) = session.take() {
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
                if action
                    .run(&mut host, &mut session, &sessions, &ws_sender, text)
                    .await
                {
                    continue;
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

    close_session(&mut session).await;
    Ok(())
}

#[derive(Default)]
struct Sessions {
    open: Mutex<VecDeque<Arc<Mutex<Session>>>>,
    closed: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
}

struct Session {
    sender_host: MyHost,
    sender_sec: Option<WsSender>,
}

impl Session {
    fn public(host: MyHost) -> Self {
        Self {
            sender_host: host,
            sender_sec: None,
        }
    }

    fn set_sec(&mut self, sender: WsSender) {
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
    #[cfg(feature = "proxy")]
    let args = args().collect::<Vec<_>>();
    #[cfg(feature = "proxy")]
    if let Some(v) = args.get(1) {
        if v.to_lowercase().trim() == "proxy" {
            let client = Client::new("127.0.0.1", 9001, "bore.pub", 0, None)
                .await
                .unwrap();
            let port = client.remote_port();
            println!("Opend on bore.pub:{port}");
            tokio::spawn(async move { client.listen().await });
        }
    }
    println!("Opend on 127.0.0.1:9001");
    let listener = TcpListener::bind("127.0.0.1:9001")
        .await
        .expect("Can't bind to address");
    let sessions = Arc::new(Sessions::default());
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(handle_connection(stream, sessions.clone()));
    }
}
