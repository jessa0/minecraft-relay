use async_io::{Async, Timer};
use futures_lite::FutureExt;
use std::convert::TryFrom;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use super::GameConnection;

pub struct GameListener {
    listener: TcpListener,
    local_addr: SocketAddr,
    keepalive: Arc<AtomicBool>,
}

pub struct GameListenerHandle {
    keepalive: Weak<AtomicBool>,
}

#[derive(Debug, thiserror::Error)]
#[error("listener closed")]
pub struct GameListenerClosedError;

const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

impl GameListener {
    pub fn new() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        let local_addr = listener.local_addr()?;
        Ok(Self { listener, local_addr, keepalive: Default::default() })
    }

    pub fn handle(&self) -> GameListenerHandle {
        GameListenerHandle { keepalive: Arc::downgrade(&self.keepalive) }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn run<E, F>(self, mut fun: F) -> anyhow::Result<()>
    where anyhow::Error: From<E>,
          F: FnMut(GameConnection) -> Result<(), E> + Send + 'static,
    {
        let Self { listener, mut keepalive, local_addr: _ } = self;
        let listener = Async::try_from(listener)?;
        async_io::block_on(async {
            loop {
                match listener.accept().or(timeout(ACCEPT_TIMEOUT)).await {
                    Ok((stream, peer_addr)) => {
                        let stream = stream.into_inner()?;
                        stream.set_nonblocking(false)?;
                        let connection = GameConnection::new(peer_addr, stream)?;
                        fun(connection)?;
                        keepalive.store(false, Ordering::Release);
                    }
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                        if !keepalive.swap(false, Ordering::AcqRel) {
                            keepalive = match Arc::try_unwrap(keepalive) {
                                Ok(_) => return Ok(()),
                                Err(keepalive) => keepalive,
                            };
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        })
    }
}

//
// GameListenerHandle impls
//

impl GameListenerHandle {
    pub fn keepalive(&self) -> Result<(), GameListenerClosedError> {
        let keepalive = self.keepalive.upgrade().ok_or(GameListenerClosedError)?;
        keepalive.store(true, Ordering::Release);
        Ok(())
    }
}

//
// private
//

async fn timeout<T>(duration: Duration) -> Result<T, io::Error> {
    Timer::after(duration).await;
    Err(io::ErrorKind::TimedOut.into())
}
