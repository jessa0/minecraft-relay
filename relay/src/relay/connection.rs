use bytes::BufMut;
use core::time::Duration;
use minecraft_relay_protocol::RelayMessage;
use std::error::Error;
use std::io::{self, Write};
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;

pub struct RelayConnection {
    peer_addr: SocketAddr,
    stream: TcpStream,
    tx: RelayConnectionHandle,
    rx: mpsc::Receiver<RelayMessage>,
}

#[derive(Clone)]
pub struct RelayConnectionHandle(mpsc::Sender<RelayMessage>);

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
        let (tx, rx) = mpsc::channel();
        let tx = RelayConnectionHandle(tx);
        Ok(Self { peer_addr: addr, stream, tx, rx })
    }

    pub fn connect(addr: SocketAddr, timeout: Duration) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        Self::new(addr, stream)
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    pub fn handle(&self) -> RelayConnectionHandle {
        self.tx.clone()
    }

    pub fn run<E, F>(self, fun: F) -> Result<(), RelayConnectionError<E>>
    where E: Error + 'static,
          F: FnMut(RelayMessage) -> Result<(), E>,
    {
        let Self { peer_addr: addr, stream, tx: _, rx } = self;
        start_writer_thread(addr, stream.try_clone()?, rx);
        read_loop(stream, fun)?;
        Ok(())
    }
}

//
// RelayHandle impls
//

impl RelayConnectionHandle {
    pub fn send(&self, message: RelayMessage) -> Result<(), RelayConnectionClosedError> {
        self.0.send(message).map_err(|_| RelayConnectionClosedError)
    }
}

//
// private
//

fn read_loop<E, F>(stream: TcpStream, mut fun: F) -> Result<(), RelayConnectionError<E>>
where E: Error + 'static,
      F: FnMut(RelayMessage) -> Result<(), E>,
{
    let mut len_buf = [0; 2];
    let mut data_buf = [0; u16::max_value() as usize];
    let mut input = io::BufReader::with_capacity(len_buf.len() + data_buf.len(), stream);
    loop {
        input.read_exact(&mut len_buf)?;
        let data = &mut data_buf[..u16::from_be_bytes(len_buf) as usize];
        input.read_exact(data)?;
        let message = RelayMessage::decode(&*data)?;
        fun(message).map_err(RelayConnectionError::Returned)?;
    }
}

fn start_writer_thread(addr: SocketAddr, stream: TcpStream, rx: mpsc::Receiver<RelayMessage>) {
    std::thread::spawn(move || {
        match write_loop(stream, rx) {
            Ok(()) => log::debug!("relay connection writer to {} closed", addr),
            Err(error) => log::warn!("error writing from relay connection {}: {}", addr, error),
        }
    });
}

fn write_loop(mut stream: TcpStream, rx: mpsc::Receiver<RelayMessage>) -> anyhow::Result<()> {
    let mut data_buf = [0; 2 + u16::max_value() as usize];
    let res = loop {
        match rx.recv_timeout(PING_INTERVAL) {
            Ok(message) => {
                let len = message.encoded_len();
                (&mut data_buf[..]).put_u16(len as u16);
                message.encode(&mut &mut data_buf[2..])?;
                stream.write_all(&data_buf[..2 + len])?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) =>
                stream.write_all(&[0; 2])?,
            Err(mpsc::RecvTimeoutError::Disconnected) =>
                break,
        }
    };
    Ok(res)
}
