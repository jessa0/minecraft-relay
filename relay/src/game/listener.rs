use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::thread::JoinHandle;
use super::GameConnection;

pub struct GameListener {
    listener: TcpListener,
    local_addr: SocketAddr,
}

pub struct GameListenerHandle {
    thread: JoinHandle<anyhow::Result<Infallible>>,
}

impl GameListener {
    pub fn new() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        let local_addr = listener.local_addr()?;
        Ok(Self { listener, local_addr })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn incoming(&self) -> impl Iterator<Item = io::Result<GameConnection>> + '_ {
        self.listener.incoming().flat_map(move |stream| {
            let stream = match stream {
                Ok(stream) => stream,
                Err(error) => return Some(Err(error)),
            };
            let game_connection = match handle_connection(stream) {
                Ok(game_connection) => game_connection,
                Err(error) => {
                    log::debug!("error accepting connection for game listener {}: {:?}", self.local_addr, error);
                    return None;
                }
            };
            Some(Ok(game_connection))
        })
    }

    pub fn start<E, F>(self, mut fun: F) -> GameListenerHandle
    where anyhow::Error: From<E>,
          F: FnMut(GameConnection) -> Result<(), E> + Send + 'static,
    {
        let thread = std::thread::spawn(move || -> anyhow::Result<Infallible> {
            for connection in self.incoming() {
                fun(connection?)?;
            }
            unreachable!()
        });
        GameListenerHandle { thread }
    }
}

//
// GameListenerHandle impls
//

impl GameListenerHandle {
    pub fn join(self) -> anyhow::Result<Infallible> {
        self.thread.join().map_err(|_| anyhow::anyhow!("game listener panicked"))?
    }
}

//
// private
//

fn handle_connection(stream: TcpStream) -> io::Result<GameConnection> {
    let addr = stream.peer_addr()?;
    let connection = GameConnection::new(addr, stream)?;
    Ok(connection)
}
