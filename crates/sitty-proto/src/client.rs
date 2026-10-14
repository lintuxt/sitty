use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Hello, HelloResult, PROTOCOL_VERSION, Principal, Request, Response, RpcError};

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Rpc(RpcError),
    Decode(serde_json::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "io: {e}"),
            ClientError::Rpc(e) => write!(f, "{e}"),
            ClientError::Decode(e) => write!(f, "decode: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError::Io(e)
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        ClientError::Decode(e)
    }
}

/// Blocking JSON-RPC client. One connection = one principal (fixed at `hello`).
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    principal: Principal,
}

impl Client {
    pub fn connect(socket: &Path, client: &str, token: Option<String>) -> Result<Client, ClientError> {
        let stream = UnixStream::connect(socket)?;
        let writer = stream.try_clone()?;
        let mut c = Client {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            principal: Principal::External,
        };
        let hello: HelloResult = c.call(
            "hello",
            Hello { protocol_version: PROTOCOL_VERSION, client: client.into(), token },
        )?;
        c.principal = hello.principal;
        Ok(c)
    }

    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    pub fn call<P: Serialize, R: DeserializeOwned>(&mut self, method: &str, params: P) -> Result<R, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request::new(id, method, serde_json::to_value(params)?);
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        let mut buf = String::new();
        if self.reader.read_line(&mut buf)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "daemon closed connection").into());
        }
        let resp: Response = serde_json::from_str(&buf)?;
        if let Some(e) = resp.error {
            return Err(ClientError::Rpc(e));
        }
        Ok(serde_json::from_value(resp.result.unwrap_or(serde_json::Value::Null))?)
    }
}
