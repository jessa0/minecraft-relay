use async_net::{AsyncToSocketAddrs, TcpListener, TcpStream};
use super::connection::{RelayConnectError, RelayConnection, RelayKeypair};
use std::convert::Infallible;
use std::error::Error;
use std::io;
use std::net::SocketAddr;

pub struct RelayListener {
    listener: TcpListener,
    keypair:  RelayKeypair,
}

#[derive(Debug, thiserror::Error)]
pub enum RelayListenerError<T: std::error::Error + 'static> {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Returned(T),
}

impl RelayListener {
    pub async fn bind<A: AsyncToSocketAddrs>(addr: A, keypair: RelayKeypair) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self { listener, keypair })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub async fn run<E: Error + 'static, F: FnMut(RelayConnection) -> Result<(), E>>(&self, mut fun: F) -> Result<Infallible, RelayListenerError<E>> {
        loop {
            let (stream, peer_addr) = self.listener.accept().await?;
            match handle_connection(stream, peer_addr, &self.keypair) {
                Ok(connection) => fun(connection).map_err(RelayListenerError::Returned)?,
                Err(error) => log::info!("error accepting connection: {:?}", error),
            }
        }
    }
}

fn handle_connection(stream: TcpStream, peer_addr: SocketAddr, keypair: &RelayKeypair) -> Result<RelayConnection, RelayConnectError> {
    let connection = RelayConnection::accept(peer_addr, stream, &keypair)?;
    Ok(connection)
}
