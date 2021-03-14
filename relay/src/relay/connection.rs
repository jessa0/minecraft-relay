use async_channel::{Receiver, Sender};
use async_io::Timer;
use async_net::{AsyncToSocketAddrs, TcpStream};
use bytes::BufMut;
use core::time::Duration;
use futures_lite::io::BufReader;
use futures_lite::{AsyncReadExt, AsyncWriteExt, FutureExt};
use minecraft_relay_protocol::RelayMessage;
use std::io;
use std::net::SocketAddr;
use std::{error::Error, sync::mpsc::RecvTimeoutError};

pub struct RelayConnection {
    peer_addr: SocketAddr,
    stream:    TcpStream,
    tx:        RelayConnectionHandle,
    rx:        Receiver<RelayMessage>,
}

#[derive(Clone)]
pub struct RelayConnectionHandle(Sender<RelayMessage>);

#[derive(Debug, thiserror::Error)]
#[error("connection closed")]
pub struct RelayConnectionClosedError;

#[derive(Debug, thiserror::Error)]
pub enum RelayConnectionError<T: Error + 'static> {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error(transparent)]
    Returned(T),
}

const PING_INTERVAL: Duration = Duration::from_secs(1);

//
// Relay impls
//

impl RelayConnection {
    pub fn new(addr: SocketAddr, stream: TcpStream) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        let (tx, rx) = async_channel::unbounded();
        let tx = RelayConnectionHandle(tx);
        Ok(Self { peer_addr: addr, stream, tx, rx })
    }

    pub async fn connect<A: AsyncToSocketAddrs>(addr: A, timeout: Duration) -> io::Result<Self> {
        let timeout = async {
            Timer::after(timeout).await;
            Err(io::ErrorKind::TimedOut.into())
        };
        let stream = TcpStream::connect(addr).or(timeout).await?;
        let addr = stream.peer_addr()?;
        Self::new(addr, stream)
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    pub fn handle(&self) -> RelayConnectionHandle {
        self.tx.clone()
    }

    pub async fn run<E, F>(self, fun: F) -> Result<(), RelayConnectionError<E>>
    where E: Error + 'static,
          F: FnMut(RelayMessage) -> Result<(), E>,
    {
        read_loop(self.stream.clone(), fun).or(write_loop(self.stream, self.rx)).await
    }
}

//
// RelayHandle impls
//

impl RelayConnectionHandle {
    pub fn send(&self, message: RelayMessage) -> Result<(), RelayConnectionClosedError> {
        self.0.try_send(message).map_err(|_| RelayConnectionClosedError)
    }
}

//
// private
//

async fn read_loop<E, F>(stream: TcpStream, mut fun: F) -> Result<(), RelayConnectionError<E>>
where E: Error + 'static,
      F: FnMut(RelayMessage) -> Result<(), E>,
{
    let mut len_buf = [0; 2];
    let mut data_buf = [0; u16::max_value() as usize];
    let mut input = BufReader::with_capacity(len_buf.len() + data_buf.len(), stream);
    loop {
        input.read_exact(&mut len_buf).await?;
        let data = &mut data_buf[..u16::from_be_bytes(len_buf) as usize];
        input.read_exact(data).await?;
        let message = RelayMessage::decode(&*data)?;
        fun(message).map_err(RelayConnectionError::Returned)?;
    }
}

async fn write_loop<E>(mut stream: TcpStream, rx: Receiver<RelayMessage>) -> Result<(), RelayConnectionError<E>>
where E: Error + 'static,
{
    let mut data_buf = [0; 2 + u16::max_value() as usize];
    let res = loop {
        let recv = async { rx.recv().await.map_err(|_| RecvTimeoutError::Disconnected) };
        let timeout = async {
            Timer::after(PING_INTERVAL).await;
            Err(RecvTimeoutError::Timeout)
        };
        match recv.or(timeout).await {
            Ok(message) => {
                let len = message.encoded_len();
                (&mut data_buf[..]).put_u16(len as u16);
                message.encode(&mut &mut data_buf[2..]).unwrap_or_else(|_| unreachable!());
                stream.write_all(&data_buf[..2 + len]).await?;
            }
            Err(RecvTimeoutError::Timeout) =>
                stream.write_all(&[0; 2]).await?,
            Err(RecvTimeoutError::Disconnected) =>
                break,
        }
    };
    Ok(res)
}
