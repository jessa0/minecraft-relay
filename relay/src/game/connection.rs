use bytes::Bytes;
use core::time::Duration;
use std::error::Error;
use std::io::{self, Write, Read};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;

pub struct GameConnection {
    stream: TcpStream,
    peer_addr: SocketAddr,
    tx: GameConnectionHandle,
    rx: mpsc::Receiver<Bytes>,
}

#[derive(Clone)]
pub struct GameConnectionHandle(mpsc::Sender<Bytes>);

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
        let (tx, rx) = mpsc::channel();
        let tx = GameConnectionHandle(tx);
        Ok(Self { stream, peer_addr, tx, rx })
    }

    pub fn connect(addr: SocketAddr, timeout: Duration) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        Self::new(addr, stream)
    }

    pub fn handle(&self) -> GameConnectionHandle {
        self.tx.clone()
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    pub fn run<E, F>(self, fun: F) -> Result<(), GameConnectionError<E>>
    where E: Error + 'static,
          F: FnMut(&[u8]) -> Result<(), E>,
    {
        start_writer_thread(self.peer_addr, self.stream.try_clone()?, self.rx);
        read_loop(self.stream, fun)
    }
}

//
// GameConnectionHandle impls
//

impl GameConnectionHandle {
    pub fn send(&self, data: Bytes) -> Result<(), GameConnectionClosedError> {
        self.0.send(data).map_err(|_| GameConnectionClosedError)
    }
}

//
// private
//

fn start_writer_thread(peer_addr: SocketAddr, stream: TcpStream, rx: mpsc::Receiver<Bytes>) {
    std::thread::spawn(move || {
        match write_loop(stream, rx) {
            Ok(()) => log::debug!("game connection {} closed", peer_addr),
            Err(error) => log::debug!("error writing on game connection {}: {:?}", peer_addr, error),
        }
    });
}

fn write_loop(mut stream: TcpStream, rx: mpsc::Receiver<Bytes>) -> io::Result<()> {
    for data in rx {
        stream.write_all(&data)?;
    }
    Ok(())
}

fn read_loop<E, F>(mut stream: TcpStream, mut fun: F) -> Result<(), GameConnectionError<E>>
    where E: Error + 'static,
          F: FnMut(&[u8]) -> Result<(), E>,
{
    let mut buf = [0; 16384];
    loop {
        let len = stream.read(&mut buf)?;
        if len == 0 {
            return Ok(());
        }
        fun(&buf[..len]).map_err(GameConnectionError::Returned)?;
    }
}
