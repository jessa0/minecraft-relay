use super::connection::{RelayConnectError, RelayConnection};
use std::convert::TryInto;
use std::error::Error;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};

pub struct RelayListener {
    listener: TcpListener,
}

#[derive(Debug, thiserror::Error)]
pub enum RelayListenerError<T: std::error::Error + 'static> {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Returned(T),
}

impl RelayListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        Ok(Self { listener })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn run<E: Error, F: FnMut(RelayConnection) -> Result<(), E>>(&self, mut fun: F) -> Result<(), RelayListenerError<E>> {
        for stream in self.listener.incoming() {
            match handle_connection(stream?) {
                Ok(connection) => fun(connection).map_err(RelayListenerError::Returned)?,
                Err(error) => log::info!("error accepting connection: {:?}", error),
            }
        }
        Ok(())
    }
}

fn handle_connection(stream: TcpStream) -> Result<RelayConnection, RelayConnectError> {
    let addr = stream.peer_addr()?;
    let connection = RelayConnection::accept(addr, stream.try_into()?)?;
    Ok(connection)
}
