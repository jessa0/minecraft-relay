mod connection;
mod listener;

pub use self::connection::{RelayConnection, RelayConnectionHandle};
pub use self::listener::RelayListener;

use bytes::Bytes;
use crate::discovery::{DiscoveryPacket, DiscoverySender};
use crate::game::{GameConnection, GameConnectionHandle, GameListener, GameListenerHandle};
use crate::id_map::IdMap;
use derive_more::{From, Into};
use minecraft_relay_protocol::{RelayConnect, RelayData, RelayGame, RelayMessage, relay_message};
use std::collections::{HashMap, hash_map};
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Relay {
    peers: IdMap<PeerId, Peer>,
    games: IdMap<LocalGameId, Game>,
    local_games: HashMap<SocketAddr, LocalGameId>,
    discovery_tx: DiscoverySender,
    tx: RelayHandle,
    rx: mpsc::Receiver<Message>,
}

#[derive(Clone)]
pub struct RelayHandle(mpsc::Sender<Message>);

#[derive(Debug, thiserror::Error)]
#[error("relay closed")]
pub struct RelayClosedError;

#[derive(Clone, Copy, Debug, Default, From, Hash, Eq, Into, PartialEq, PartialOrd, Ord)]
struct PeerId(u64);

struct Peer {
    handle: RelayConnectionHandle,
    games: HashMap<RemoteGameId, RelayedGame>,
    local_connections: HashMap<ConnectionId, LocalConnection>,
    relayed_connections: HashMap<ConnectionId, RelayedConnection>,
    next_connection_id: ConnectionId,
}

struct RelayedGame {
    local_game_id: LocalGameId,
}

#[derive(Clone, Copy, Debug, Default, Hash, Eq, PartialEq, PartialOrd, Ord)]
struct ConnectionId {
    id: u64,
    local: bool,
}

struct LocalConnection {
    tx: GameConnectionHandle,
}

struct RelayedConnection {
    peer: PeerId,
    peer_connection_id: ConnectionId,
}

#[derive(Clone, Copy, Debug, Default, From, Hash, Eq, Into, PartialEq, PartialOrd, Ord)]
struct LocalGameId(u64);

#[derive(Clone, Copy, Debug, Default, From, Hash, Eq, Into, PartialEq, PartialOrd, Ord)]
struct RemoteGameId(u64);

enum Game {
    Local(LocalGame),
    Remote(RemoteGame),
}

struct LocalGame {
    addr: SocketAddr,
}

struct RemoteGame {
    peer_id: PeerId,
    remote_game_id: RemoteGameId,
    listener: Option<RemoteGameListener>,
}

struct RemoteGameListener {
    handle: GameListenerHandle,
    addr: SocketAddr,
}

enum Message {
    AcceptPeer(RelayConnection),
    AcceptGameConnection(LocalGameId, GameConnection),
    AddGameConnection(PeerId, ConnectionId, LocalGameId, GameConnectionHandle),
    AddPeer(RelayConnectionHandle, mpsc::Sender<PeerId>),
    AddLocalGame(LocalGame, Bytes),
    Recv(PeerId, RelayMessage),
    RemovePeer(PeerId),
}

const MAX_FORWARDING_HOPS: u64 = 2;
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const GAME_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

impl Relay {
    pub fn new() -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            peers: Default::default(),
            games: Default::default(),
            local_games: Default::default(),
            discovery_tx: DiscoverySender::new()?,
            tx: RelayHandle(tx),
            rx,
        })
    }

    pub fn handle(&self) -> RelayHandle {
        self.tx.clone()
    }

    pub fn listen<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        let tx = self.tx.clone();
        let listener = RelayListener::bind(addr)?;
        log::info!("relay listening on {}", listener.local_addr()?);
        std::thread::spawn(move || {
            loop {
                match listener.run(|connection| tx.send(Message::AcceptPeer(connection))) {
                    Ok(()) => (),
                    Err(error) =>
                        log::warn!("error listening for relay connections: {:?}", error),
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        Ok(())
    }

    pub fn connect(&self, addr: SocketAddr) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            loop {
                let reconnect_time = Instant::now() + RELAY_CONNECT_TIMEOUT;
                match RelayConnection::connect(addr, RELAY_CONNECT_TIMEOUT) {
                    Ok(connection) => {
                        let addr = connection.peer_addr();
                        log::info!("connected to relay {}", addr);
                        let (reply_tx, reply_rx) = mpsc::channel();
                        if let Err(_) = tx.send(Message::AddPeer(connection.handle(), reply_tx)) {
                            return;
                        }
                        let id = match reply_rx.recv() {
                            Ok(id) => id,
                            Err(_) => break,
                        };
                        match connection.run(|message| tx.send(Message::Recv(id, message))) {
                            Ok(()) => log::debug!("relay connection to {} closed", addr),
                            Err(error) => log::info!("error reading from relay connection {}: {}", addr, error),
                        }
                        if let Err(_) = tx.send(Message::RemovePeer(id)) {
                            return;
                        }
                    }
                    Err(error) =>
                        log::info!("error connecting to relay {}: {}", addr, error),
                }
                if let Some(sleep_duration) = reconnect_time.checked_duration_since(Instant::now()) {
                    std::thread::sleep(sleep_duration);
                }
            }
        });
    }

    pub fn run(&mut self) {
        while let Ok(message) = self.rx.recv() {
            match message {
                Message::AcceptGameConnection(game_id, connection) =>
                    self.accept_game_connection(game_id, connection),
                Message::AcceptPeer(connection) =>
                    self.accept_peer_connection(connection),
                Message::AddGameConnection(peer_id, connection_id, game_id, tx) =>
                    self.add_game_connection(peer_id, connection_id, game_id, tx),
                Message::AddPeer(handle, reply_tx) =>
                    drop(reply_tx.send(self.peers.add(Peer::new(handle)))),
                Message::AddLocalGame(local_game, motd) =>
                    self.add_local_game(local_game, motd),
                Message::Recv(from, message) =>
                    self.handle_peer_message(from, message),
                Message::RemovePeer(peer_id) =>
                    self.remove_peer(&peer_id),
            }
        }
    }

    fn accept_game_connection(&mut self, game_id: LocalGameId, connection: GameConnection) {
        let game = match self.games.map.get_mut(&game_id) {
            Some(Game::Remote(game)) => game,
            _ => {
                log::debug!("dropping accepted connection for unknown {:?}", game_id);
                return;
            }
        };
        let remote_game_id = game.remote_game_id;

        let peer_id = game.peer_id;
        let peer = match self.peers.map.get_mut(&peer_id) {
            Some(peer) => peer,
            None => {
                log::debug!("dropping accepted connection for {:?} on unknown {:?}", game_id, peer_id);
                return;
            }
        };

        let connection_id = peer.next_connection_id;
        peer.next_connection_id.id += 1;
        peer.local_connections.insert(connection_id, LocalConnection { tx: connection.handle() });

        log::debug!("accepted {:?} for {:?} from {} for {:?} on {:?}", connection_id, game_id, connection.peer_addr(), remote_game_id, peer_id);

        let peer_addr = connection.peer_addr();
        let peer_tx = peer.handle.clone();
        std::thread::spawn(move || {
            if let Err(_) = peer_tx.send(RelayMessage {
                inner: Some(relay_message::Inner::Connect(RelayConnect {
                    game_id: remote_game_id.0,
                    connection_id: connection_id.id,
                    hops: 0,
                })),
            }) {
                log::debug!("relay connection for {:?} for {:?} from {} for {:?} on {:?} closed",
                            connection_id, game_id, peer_addr, remote_game_id, peer_id);
                return;
            }
            match connection.run(|data| peer_tx.send(RelayMessage {
                inner: Some(relay_message::Inner::Data(RelayData {
                    connection_id: connection_id.id,
                    connection_local: connection_id.local,
                    data: data.to_vec().into(),
                })),
            })) {
                Ok(()) => log::debug!("{:?} for {:?} from {} for {:?} on {:?} closed",
                                      connection_id, game_id, peer_addr, remote_game_id, peer_id),
                Err(error) => log::debug!("error reading on {:?} for {:?}from {} for {:?} on {:?}: {:?}",
                                          connection_id, game_id, peer_addr, remote_game_id, peer_id, error),
            }
        });
    }

    fn accept_peer_connection(&mut self, connection: RelayConnection) {
        let peer_id = self.peers.add(Peer::new(connection.handle()));
        log::info!("accepted relay connection from {} for {:?}", connection.peer_addr(), peer_id);

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let addr = connection.peer_addr();
            match connection.run(|message| tx.send(Message::Recv(peer_id, message))) {
                Ok(()) => log::debug!("relay connection from {} for {:?} closed", addr, peer_id),
                Err(error) => log::info!("error reading from relay connection from {} for {:?}: {}", addr, peer_id, error),
            }
        });
    }

    fn add_game_connection(&mut self, from: PeerId, connection_id: ConnectionId, game_id: LocalGameId, tx: GameConnectionHandle) {
        let peer = match self.peers.map.get_mut(&from) {
            Some(peer) => peer,
            None => {
                log::debug!("dropping new {:?} for {:?} on unknown {:?}", connection_id, game_id, from);
                return;
            }
        };

        peer.local_connections.insert(connection_id, LocalConnection { tx });

        if let Err(_) = peer.handle.send(RelayMessage {
            inner: Some(relay_message::Inner::Data(RelayData {
                connection_id: connection_id.id,
                connection_local: connection_id.local,
                data: Default::default(),
            })),
        }) {
            self.remove_peer(&from);
        }
    }

    fn add_local_game(&mut self, local_game: LocalGame, motd: Bytes) {
        let addr = local_game.addr;
        let games = &mut self.games;
        let game_id = self.local_games.entry(addr).or_insert_with(|| {
            let game_id = games.add(Game::Local(local_game));
            log::debug!("adding {:?} from {}", game_id, addr);
            game_id
        });
        log::debug!("forwarding advertisement for {:?}", game_id);
        let mut errors = Vec::new();
        for (peer_id, peer) in &self.peers.map {
            if let Err(_) = peer.handle.send(RelayMessage {
                inner: Some(relay_message::Inner::Game(RelayGame {
                    id: game_id.0,
                    motd: motd.clone(),
                    hops: 0,
                })),
            }) {
                errors.push(*peer_id);
            }
        }
        for peer_id in errors {
            self.remove_peer(&peer_id);
        }
    }

    fn handle_peer_message(&mut self, from: PeerId, message: RelayMessage) {
        let inner = if let Some(inner) = message.inner { inner } else { return };
        match inner {
            relay_message::Inner::Game(game_message) =>
                self.handle_game(from, game_message),
            relay_message::Inner::Connect(connect_message) =>
                self.handle_connect(from, connect_message),
            relay_message::Inner::Data(data_message) =>
                self.handle_data(from, data_message),
        }
    }

    fn handle_game(&mut self, from: PeerId, game_message: RelayGame) {
        let relayed_game_entry = match self.add_game(from, &game_message) {
            Some(relayed_game_entry) => relayed_game_entry,
            None => {
                log::debug!("dropping game advertisement from unknown {:?}", from);
                return;
            }
        };

        let local_game_id = relayed_game_entry.local_game_id;
        self.forward_game(from, local_game_id, &game_message);
    }

    fn add_game(&mut self, from: PeerId, game_message: &RelayGame) -> Option<&mut RelayedGame> {
        let peer = self.peers.map.get_mut(&from)?;
        let remote_game_id = RemoteGameId(game_message.id);
        let games = &mut self.games;
        let relayed_game_entry = peer.games.entry(remote_game_id).or_insert_with(|| {
            RelayedGame { local_game_id: games.next_id() }
        });

        let local_game_id = relayed_game_entry.local_game_id;
        let game = match self.games.map.entry(local_game_id) {
            hash_map::Entry::Vacant(game_entry) => {
                game_entry.insert(Game::Remote(RemoteGame {
                    peer_id: from,
                    remote_game_id,
                    listener: None,
                }))
            }
            hash_map::Entry::Occupied(game_entry) =>
                game_entry.into_mut(),
        };

        let remote_game = match game {
            Game::Remote(remote_game) => remote_game,
            _ => return None,
        };

        if let Some(listener) = &mut remote_game.listener {
            if let Err(_) = listener.handle.keepalive() {
                remote_game.listener = None;
            }
        }

        if let None = remote_game.listener {
            remote_game.listener = match GameListener::new() {
                Ok(listener) => {
                    let listener_addr = listener.local_addr();
                    let listener_handle = listener.handle();
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        match listener.run(move |connection| {
                            tx.send(Message::AcceptGameConnection(local_game_id, connection))
                        }) {
                            Ok(()) =>
                                log::info!("listener for {:?} on {} for {:?} on {:?} closed",
                                           local_game_id, listener_addr, remote_game_id, from),
                            Err(error) =>
                                log::info!("error listening for {:?} on {} for {:?} on {:?}: {:?}",
                                           local_game_id, listener_addr, remote_game_id, from, error),
                        }
                    });
                    log::info!("started listener for {:?} on {} for {:?} on {:?}",
                               local_game_id, listener_addr, remote_game_id, from);
                    Some(RemoteGameListener { handle: listener_handle, addr: listener_addr })
                }
                Err(error) => {
                    log::warn!("error starting game listener for {:?} for {:?} on {:?}: {:?}",
                               local_game_id, remote_game_id, from, error);
                    None
                }
            };
        }

        if let Some(listener) = &mut remote_game.listener {
            log::debug!("sending advertisement for listener for {:?} on {} for {:?} on {:?}",
                        local_game_id, listener.addr, remote_game_id, from);
            let packet = DiscoveryPacket {
                port: listener.addr.port(),
                motd: &game_message.motd,
            };
            match self.discovery_tx.send(&packet) {
                Ok(()) => (),
                Err(error) => log::warn!("error sending game advertisement: {:?}", error),
            }
        }

        Some(relayed_game_entry)
    }

    fn forward_game(&mut self, from: PeerId, game_id: LocalGameId, game_message: &RelayGame) {
        if game_message.hops >= MAX_FORWARDING_HOPS {
            log::debug!("not forwarding {:?} from {:?} after {} forwarding hops", game_id, from, game_message.hops);
            return;
        }
        if self.peers.map.len() > 1 {
            log::debug!("forwarding {:?} from {:?} to {} peers", game_id, from, self.peers.map.len() - 1);
        }
        let mut errors = Vec::new();
        for (peer_id, peer) in &self.peers.map {
            if *peer_id != from {
                if let Err(_) = peer.handle.send(RelayMessage {
                    inner: Some(relay_message::Inner::Game(RelayGame {
                        id: game_id.0,
                        motd: game_message.motd.clone(),
                        hops: game_message.hops + 1,
                    })),
                }) {
                    errors.push(*peer_id);
                }
            }
        }
        for peer_id in errors {
            self.remove_peer(&peer_id);
        }
    }

    fn handle_connect(&mut self, from: PeerId, connect_message: RelayConnect) {
        let game_id = LocalGameId(connect_message.game_id);
        let connection_id = ConnectionId { id: connect_message.connection_id, local: false };
        match self.games.map.get_mut(&game_id) {
            Some(&mut Game::Local(LocalGame { addr })) =>
                self.connect_local(from, game_id, connection_id, addr),
            Some(&mut Game::Remote(RemoteGame { peer_id: to, remote_game_id, .. })) =>
                self.forward_connect(from, to, remote_game_id, connect_message),
            None =>
                log::debug!("dropping {:?} from {:?} for unknown {:?}", connection_id, from, game_id),
        }
    }

    fn connect_local(&mut self, from: PeerId, game_id: LocalGameId, connection_id: ConnectionId, addr: SocketAddr) {
        let peer = match self.peers.map.get_mut(&from) {
            Some(peer) => peer,
            None => {
                log::debug!("not connecting for unknown {:?} to {:?}", from, game_id);
                return;
            }
        };
        let tx = self.tx.clone();
        let peer_tx = peer.handle.clone();
        std::thread::spawn(move || {
            match GameConnection::connect(addr, GAME_CONNECT_TIMEOUT) {
                Ok(connection) => {
                    log::debug!("connected {:?} for {:?} to {:?} at {}", connection_id, from, game_id, addr);
                    if let Err(_) = tx.send(Message::AddGameConnection(from, connection_id, game_id, connection.handle())) {
                        return;
                    }
                    match connection.run(move |data| peer_tx.send(RelayMessage {
                        inner: Some(relay_message::Inner::Data(RelayData {
                            connection_id: connection_id.id,
                            connection_local: connection_id.local,
                            data: data.to_vec().into(),
                        })),
                    })) {
                        Ok(()) => log::debug!("connection {:?} for {:?} to {:?} at {} closed",
                                              connection_id, from, game_id, addr),
                        Err(error) => log::debug!("error reading on connection {:?} for {:?} to {:?} at {}: {:?}",
                                                  connection_id, from, game_id, addr, error),
                    }
                }
                Err(error) =>
                    log::warn!("error connecting {:?} for {:?} to {:?} at {}: {:?}",
                               connection_id, from, game_id, addr, error),
            }
        });
    }

    fn forward_connect(&mut self, from: PeerId, to: PeerId, to_game_id: RemoteGameId, connect_message: RelayConnect) {
        if connect_message.hops >= MAX_FORWARDING_HOPS {
            log::debug!("not forwarding from {:?} after {} forwarding hops", from, connect_message.hops);
            return;
        }

        let from_peer = match self.peers.map.get(&from) {
            Some(from_peer) => from_peer,
            None => return,
        };

        let from_connection_id = ConnectionId { id: connect_message.connection_id, local: false };
        if from_peer.relayed_connections.contains_key(&from_connection_id) {
            log::debug!("dropping duplicate connection attempt from {:?} to {:?} with id {:?}", from, to, from_connection_id);
            return;
        }

        let to_peer = match self.peers.map.get_mut(&to) {
            Some(to_peer) => to_peer,
            None => return,
        };

        let to_connection_id = to_peer.next_connection_id;
        to_peer.next_connection_id.id += 1;
        to_peer.relayed_connections.insert(to_connection_id, RelayedConnection {
            peer: from,
            peer_connection_id: from_connection_id,
        });

        log::debug!("forwarding connection {:?} from {:?} to {:?} with {:?} on {:?}",
                    from_connection_id, from, to_game_id, to_connection_id, to);

        if let Err(_) = to_peer.handle.send(RelayMessage {
            inner: Some(relay_message::Inner::Connect(RelayConnect {
                game_id: to_game_id.0,
                connection_id: to_connection_id.id,
                hops: connect_message.hops + 1,
            })),
        }) {
            self.remove_peer(&to);
            return;
        }

        let from_peer = match self.peers.map.get_mut(&from) {
            Some(from_peer) => from_peer,
            None => unreachable!(),
        };
        from_peer.relayed_connections.insert(from_connection_id, RelayedConnection {
            peer: to,
            peer_connection_id: to_connection_id,
        });
    }

    fn handle_data(&mut self, from: PeerId, data_message: RelayData) {
        let from_connection_id = ConnectionId { id: data_message.connection_id, local: data_message.connection_local };

        let from_peer = match self.peers.map.get_mut(&from) {
            Some(from_peer) => from_peer,
            None => {
                log::debug!("dropping data packet for {:?} from unknown {:?}", from_connection_id, from);
                return;
            }
        };

        if let hash_map::Entry::Occupied(local_connection) = from_peer.local_connections.entry(from_connection_id) {
            if let Err(_) = local_connection.get().tx.send(data_message.data) {
                local_connection.remove();
            }
        } else {
            let from_connection = match from_peer.relayed_connections.get(&from_connection_id) {
                Some(from_connection) => from_connection,
                None => {
                    log::debug!("dropping data packet for unknown {:?} from {:?}", from_connection_id, from);
                    return;
                }
            };

            let &RelayedConnection { peer: to, peer_connection_id: to_connection_id } = from_connection;
            let to_peer = match self.peers.map.get(&to) {
                Some(to_peer) => to_peer,
                None => {
                    log::debug!("dropping data packet for {:?} from {:?} to {:?} on unknown {:?}",
                                from_connection_id, from, to_connection_id, to);
                    return;
                }
            };

            if let Err(_) = to_peer.handle.send(RelayMessage {
                inner: Some(relay_message::Inner::Data(RelayData {
                    connection_id: to_connection_id.id,
                    connection_local: to_connection_id.local,
                    data: data_message.data,
                })),
            }) {
                self.remove_peer(&to);
            }
        }
    }

    fn remove_peer(&mut self, peer_id: &PeerId) {
        let peer = match self.peers.map.get(peer_id) {
            Some(peer) => peer,
            None => return,
        };

        for (_, game) in &peer.games {
            self.games.map.remove(&game.local_game_id);
        }
        if self.games.map.len() < self.games.map.capacity() / 4 {
            self.games.map.shrink_to_fit();
        }
    }
}

impl RelayHandle {
    fn send(&self, message: Message) -> Result<(), RelayClosedError> {
        self.0.send(message).map_err(|_| RelayClosedError)
    }

    pub fn add_local_game(&self, addr: SocketAddr, motd: Bytes) -> Result<(), RelayClosedError> {
        self.send(Message::AddLocalGame(LocalGame { addr }, motd))
    }
}

impl Peer {
    pub fn new(handle: RelayConnectionHandle) -> Self {
        Self {
            handle,
            games: Default::default(),
            local_connections: Default::default(),
            relayed_connections: Default::default(),
            next_connection_id: Default::default(),
        }
    }
}
