use async_channel::{Receiver, Sender};
use async_io::Timer;
use async_net::{AsyncToSocketAddrs, TcpStream};
use bytes::BufMut;
use core::time::Duration;
use futures_lite::io::BufReader;
use futures_lite::{AsyncReadExt, AsyncWriteExt, FutureExt};
use minecraft_relay_protocol::RelayMessage;
use std::cell::RefCell;
use std::io;
use std::net::SocketAddr;
use std::{error::Error, sync::mpsc::RecvTimeoutError};

pub struct RelayConnection {
    peer_addr: SocketAddr,
    stream:    TcpStream,
    handshake: Box<snow::HandshakeState>,
    tx:        RelayConnectionHandle,
    rx:        Receiver<RelayMessage>,
}

#[derive(Clone)]
pub struct RelayConnectionHandle(Sender<RelayMessage>);

#[derive(Debug, thiserror::Error)]
#[error("connection closed")]
pub struct RelayConnectionClosedError;

#[derive(Debug, thiserror::Error)]
pub enum RelayConnectError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("encryption error: {0}")]
    Encryption(#[from] snow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum RelayConnectionError<T: Error + 'static> {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("decrypt error: {0}")]
    Decrypt(snow::Error),
    #[error("encrypt error: {0}")]
    Encrypt(snow::Error),
    #[error(transparent)]
    Returned(T),
}

const MAX_ENCODED_LEN: usize = 65535;
const MAX_ENCRYPTED_LEN: usize = 16 + 65535;
const MESSAGE_BUFFER_SIZE: usize = 2 + MAX_ENCODED_LEN;
const PATTERN: &'static str = "Noise_NN_25519_ChaChaPoly_BLAKE2s";
const PING_INTERVAL: Duration = Duration::from_secs(1);

//
// Relay impls
//

impl RelayConnection {
    pub fn new(addr: SocketAddr, stream: TcpStream, handshake: Box<snow::HandshakeState>) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        let (tx, rx) = async_channel::unbounded();
        let tx = RelayConnectionHandle(tx);
        Ok(Self { peer_addr: addr, stream, handshake, tx, rx })
    }

    pub fn accept(addr: SocketAddr, stream: TcpStream) -> Result<Self, RelayConnectError> {
        let handshake_builder = snow::Builder::new(PATTERN.parse().unwrap());
        let handshake = Box::new(handshake_builder.build_responder()?);
        Ok(Self::new(addr, stream, handshake)?)
    }

    pub async fn connect<A: AsyncToSocketAddrs>(addr: A, timeout: Duration) -> Result<Self, RelayConnectError> {
        let timeout = async {
            Timer::after(timeout).await;
            Err(io::ErrorKind::TimedOut.into())
        };
        let stream = TcpStream::connect(addr).or(timeout).await?;
        let addr = stream.peer_addr()?;
        let handshake_builder = snow::Builder::new(PATTERN.parse().unwrap());
        let handshake = Box::new(handshake_builder.build_initiator()?);
        Ok(Self::new(addr, stream, handshake)?)
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
        drop(self.tx);
        let mut data_buf = [0; MESSAGE_BUFFER_SIZE];
        let mut read_buf = [0; MAX_ENCRYPTED_LEN];
        let mut input = BufReader::with_capacity(MESSAGE_BUFFER_SIZE, self.stream.clone());
        let transport = RefCell::new(handshake_loop(&mut input, self.handshake, &mut data_buf).await?);
        read_loop(input, &transport, &mut read_buf, fun)
            .or(write_loop(self.stream, &transport, self.rx, &mut data_buf)).await
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

async fn handshake_loop<E>(
    input: &mut BufReader<TcpStream>,
    mut handshake: Box<snow::HandshakeState>,
    data_buf: &mut [u8; MESSAGE_BUFFER_SIZE],
) -> Result<snow::TransportState, RelayConnectionError<E>>
where E: Error + 'static,
{
    while !handshake.is_handshake_finished() {
        if handshake.is_my_turn() {
            let len = handshake.write_message(&[], &mut data_buf[2..]).map_err(RelayConnectionError::Encrypt)?;
            (&mut data_buf[..2]).put_u16(len as u16);
            input.get_mut().write_all(&data_buf[..2 + len]).await?;
        }
        if !handshake.is_handshake_finished() {
            let mut len_buf = [0; 2];
            input.read_exact(&mut len_buf).await?;
            let data = &mut data_buf[..u16::from_be_bytes(len_buf) as usize];
            input.read_exact(data).await?;
            handshake.read_message(data, &mut []).map_err(RelayConnectionError::Decrypt)?;
        }
    }
    Ok(handshake.into_transport_mode().map_err(RelayConnectionError::Encrypt)?)
}

async fn read_loop<E, F>(
    mut input: BufReader<TcpStream>,
    transport: &RefCell<snow::TransportState>,
    data_buf: &mut [u8; MAX_ENCRYPTED_LEN],
    mut fun: F,
) -> Result<(), RelayConnectionError<E>>
where E: Error + 'static,
      F: FnMut(RelayMessage) -> Result<(), E>,
{
    let mut decrypt_buf = [0; MAX_ENCODED_LEN];
    loop {
        let mut len_buf = [0; 2];
        input.read_exact(&mut len_buf).await?;
        let data = &mut data_buf[..u16::from_be_bytes(len_buf) as usize];
        input.read_exact(data).await?;
        let decrypted = if data.len() != 0 {
            let decrypted_len =
                transport.borrow_mut()
                         .read_message(&data, &mut decrypt_buf)
                         .map_err(RelayConnectionError::Decrypt)?;
            &decrypt_buf[..decrypted_len]
        } else {
            &[]
        };
        let message = RelayMessage::decode(decrypted)?;
        fun(message).map_err(RelayConnectionError::Returned)?;
    }
}

async fn write_loop<E>(
    mut stream: TcpStream,
    transport: &RefCell<snow::TransportState>,
    rx: Receiver<RelayMessage>,
    data_buf: &mut [u8; MESSAGE_BUFFER_SIZE],
) -> Result<(), RelayConnectionError<E>>
where E: Error + 'static,
{
    let mut encode_buf = [0; u16::max_value() as usize];
    let res = loop {
        let recv = async { rx.recv().await.map_err(|_| RecvTimeoutError::Disconnected) };
        let timeout = async {
            Timer::after(PING_INTERVAL).await;
            Err(RecvTimeoutError::Timeout)
        };
        match recv.or(timeout).await {
            Ok(message) => {
                let encoded_len = message.encoded_len();
                message.encode(&mut &mut encode_buf[..encoded_len]).unwrap_or_else(|_| unreachable!());
                let encrypted_len =
                    transport.borrow_mut()
                             .write_message(&encode_buf[..encoded_len], &mut data_buf[2..])
                             .map_err(RelayConnectionError::Encrypt)?;
                (&mut data_buf[..2]).put_u16(encrypted_len as u16);
                stream.write_all(&data_buf[..2 + encrypted_len]).await?;
            }
            Err(RecvTimeoutError::Timeout) =>
                stream.write_all(&[0; 2]).await?,
            Err(RecvTimeoutError::Disconnected) =>
                break,
        }
    };
    Ok(res)
}
