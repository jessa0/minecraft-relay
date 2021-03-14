use async_channel::{Receiver, Sender};
use async_io::Timer;
use async_net::{AsyncToSocketAddrs, TcpStream};
use bytes::Bytes;
use core::time::Duration;
use futures_lite::{AsyncReadExt, AsyncWriteExt, FutureExt};
use std::error::Error;
use std::io;
use std::net::SocketAddr;

pub struct GameConnection {
    stream:    TcpStream,
    peer_addr: SocketAddr,
    tx:        GameConnectionHandle,
    rx:        Receiver<Bytes>,
}

#[derive(Clone)]
pub struct GameConnectionHandle(Sender<Bytes>);

#[derive(Debug, thiserror::Error)]
#[error("connection closed")]
pub struct GameConnectionClosedError;

#[derive(Debug, thiserror::Error)]
pub enum GameConnectionError<T: std::fmt::Debug> {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("returned error")]
    Returned(T),
}

impl GameConnection {
    pub fn new(peer_addr: SocketAddr, stream: TcpStream) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        let (tx, rx) = async_channel::unbounded();
        let tx = GameConnectionHandle(tx);
        Ok(Self { stream, peer_addr, tx, rx })
    }

    pub async fn connect<A: AsyncToSocketAddrs>(addr: A, timeout: Duration) -> io::Result<Self> {
        let timeout = async {
            Timer::after(timeout).await;
            Err(io::ErrorKind::TimedOut.into())
        };
        let stream = TcpStream::connect(addr).or(timeout).await?;
        let peer_addr = stream.peer_addr()?;
        Self::new(peer_addr, stream)
    }

    pub fn handle(&self) -> GameConnectionHandle {
        self.tx.clone()
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    pub async fn run<E, F>(self, fun: F) -> Result<(), GameConnectionError<E>>
    where E: Error + 'static,
          F: FnMut(&[u8]) -> Result<(), E>,
    {
        read_loop(self.stream.clone(), fun).or(write_loop(self.stream, self.rx)).await
    }
}

//
// GameConnectionHandle impls
//

impl GameConnectionHandle {
    pub fn send(&self, data: Bytes) -> Result<(), GameConnectionClosedError> {
        self.0.try_send(data).map_err(|_| GameConnectionClosedError)
    }
}

//
// private
//

async fn write_loop<E>(mut stream: TcpStream, rx: Receiver<Bytes>) -> Result<(), GameConnectionError<E>>
where E: Error + 'static,
{
    while let Ok(data) = rx.recv().await {
        stream.write_all(&data).await?;
    }
    Ok(())
}

async fn read_loop<E, F>(mut stream: TcpStream, mut fun: F) -> Result<(), GameConnectionError<E>>
    where E: Error + 'static,
          F: FnMut(&[u8]) -> Result<(), E>,
{
    let mut buf = [0; 16384];
    loop {
        let len = stream.read(&mut buf).await?;
        if len == 0 {
            return Ok(());
        }
        fun(&buf[..len]).map_err(GameConnectionError::Returned)?;
    }
}
