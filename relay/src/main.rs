use anyhow::Context;
use bytes::Bytes;
use core::convert::Infallible;
use core::time::Duration;
use std::io;
use std::net::SocketAddr;
use std::sync::mpsc;
use minecraft_relay::discovery::DiscoveryListener;
use minecraft_relay::relay::{Relay, RelayConnectHandle, RelayHandle, RelayListener, RelayKeypair, RelayStaticKey};

const DEFAULT_PORT: u16 = 25566;

enum Message {
    Input(String),
    AcceptConnection(SocketAddr, RelayStaticKey, mpsc::Sender<bool>),
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    let args = std::env::args().skip(1);

    let discovery_listener = DiscoveryListener::new().context("error listening for UDP game advertisements")?;

    let mut relay = Relay::new()?;
    let keypair = RelayKeypair::generate();
    log::info!("generated static key {}", keypair);

    let listener = async_io::block_on(RelayListener::bind("0.0.0.0:25566", keypair.clone()))
        .context("error listening for relay connections on tcp port 25566")?;
    log::info!("relay listening on {}", listener.local_addr()?);

    let (tx, rx) = mpsc::channel();
    start_listen_thread(listener, relay.handle(), tx.clone());
    start_stdin_thread(tx);

    start_ui_thread(relay.handle(), keypair.clone(), rx);

    for connect_addr in args {
        relay.connect(connect_addr, keypair.clone());
    }

    let relay_handle = relay.handle();
    std::thread::spawn(move || {
        loop {
            let run_res = discovery_listener.run(|from, packet| {
                relay_handle.add_local_game(from, packet.port, Bytes::from(packet.motd.to_vec()))
            });
            match run_res {
                Ok(never) => match never {},
                Err(error) =>
                    log::warn!("error listening for UDP game advertisements: {:?}", error),
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    relay.run();

    Ok(())
}

fn start_stdin_thread(tx: mpsc::Sender<Message>) {
    std::thread::spawn(move || stdin_loop(tx));
}

fn stdin_loop(tx: mpsc::Sender<Message>) {
    loop {
        let mut line = String::new();
        let line = match io::stdin().read_line(&mut line) {
            Ok(0) => break,
            Ok(_len) => line.trim().to_string(),
            Err(error) => {
                log::warn!("error reading input: {:?}", error);
                break;
            }
        };
        if let Err(_) = tx.send(Message::Input(line)) {
            break;
        }
    }
}

fn start_listen_thread(listener: RelayListener, relay: RelayHandle, tx: mpsc::Sender<Message>) {
    std::thread::spawn(move || async_io::block_on(listen_loop(listener, relay, tx)));
}

async fn listen_loop(listener: RelayListener, relay: RelayHandle, tx: mpsc::Sender<Message>) -> Infallible {
    relay.listen(listener, |connection| {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ignore = tx.send(Message::AcceptConnection(connection.peer_addr(), connection.peer_static(), reply_tx));
        reply_rx.recv().unwrap_or(false)
    }).await
}

fn start_ui_thread(relay: RelayHandle, keypair: RelayKeypair, rx: mpsc::Receiver<Message>) {
    std::thread::spawn(move || ui_loop(relay, keypair, rx));
}

fn ui_loop(relay: RelayHandle, keypair: RelayKeypair, rx: mpsc::Receiver<Message>) {
    let mut connect_handle: Option<RelayConnectHandle> = None;
    let mut waiting_peer: Option<(SocketAddr, RelayStaticKey, mpsc::Sender<bool>)> = Default::default();
    loop {
        if let Some((peer_addr, peer_static, _reply_tx)) = &waiting_peer {
            println!(">>>> Press Enter to accept connection from relay {} ({}), or enter 'no' to refuse: <<<<", peer_addr, peer_static);
        } else if let Some(_) = &connect_handle {
            println!(">>>> Press Enter or enter a new host to stop trying to connect: <<<<");
        } else {
            println!(">>>> Enter host or host:port to connect to (e.g. example.com or example.com:25566): <<<<");
        }
        match rx.recv() {
            Ok(Message::Input(mut addr_str)) => {
                if let Some((peer_addr, peer_static, reply_tx)) = waiting_peer.take() {
                    addr_str.make_ascii_lowercase();
                    match &*addr_str {
                        "y" | "yes" | "" => drop(reply_tx.send(true)),
                        "n" | "no" => (),
                        _ => waiting_peer = Some((peer_addr, peer_static, reply_tx)),
                    }
                } else {
                    if let Some(connect_handle) = connect_handle.take() {
                        connect_handle.cancel();
                    }
                    if !addr_str.is_empty() {
                        let new_connect_handle = if addr_str.contains(':') {
                            relay.connect(addr_str, keypair.clone())
                        } else {
                            relay.connect((addr_str, DEFAULT_PORT), keypair.clone())
                        };
                        connect_handle = Some(new_connect_handle);
                    }
                }
            }
            Ok(Message::AcceptConnection(peer_addr, peer_static, reply_tx)) => {
                if peer_static == keypair.public() {
                    log::info!("refusing connection from self");
                } else {
                    waiting_peer = Some((peer_addr, peer_static, reply_tx));
                }
            }
            Err(_) => break,
        }
    }
}
