mod documents;
mod sqlite;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::Message as WsMessage, MaybeTlsStream, WebSocketStream,
};

pub use documents::{Document, DocumentMeta, DocumentsClient};
pub use sqlite::SqliteClient;
pub use tokio_tungstenite::tungstenite::Error as WsError;
pub use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("websocket error: {0}")]
    WebSocket(#[from] WsError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("xdg base directories error: {0}")]
    Xdg(#[from] xdg::BaseDirectoriesError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Payload received from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
}

/// A connection to a fyde server.
pub struct Client {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Client {
    /// Connects to a fyde server at the given `ws://` or `wss://` URL.
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        let url = Url::parse(url.as_ref())?;
        let (stream, _response) = tokio_tungstenite::connect_async(url.as_str()).await?;
        Ok(Self { stream })
    }

    /// Sends a text message to the server.
    pub async fn send_text(&mut self, text: impl Into<String>) -> Result<()> {
        self.stream.send(WsMessage::Text(text.into().into())).await?;
        Ok(())
    }

    /// Sends a binary message to the server.
    pub async fn send_binary(&mut self, data: impl Into<Vec<u8>>) -> Result<()> {
        self.stream.send(WsMessage::Binary(data.into().into())).await?;
        Ok(())
    }

    /// Waits for the next message from the server.
    ///
    /// Returns `Ok(None)` once the connection has been closed.
    pub async fn recv(&mut self) -> Result<Option<Message>> {
        loop {
            match self.stream.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    return Ok(Some(Message::Text(text.to_string())))
                }
                Some(Ok(WsMessage::Binary(data))) => {
                    return Ok(Some(Message::Binary(data.to_vec())))
                }
                Some(Ok(WsMessage::Close(_))) | None => return Ok(None),
                Some(Ok(_)) => continue,
                Some(Err(err)) => return Err(err.into()),
            }
        }
    }

    /// Closes the connection.
    pub async fn close(&mut self) -> Result<()> {
        self.stream.close(None).await?;
        Ok(())
    }
}
