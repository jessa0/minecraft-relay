use anyhow::Context;
use bytes::Bytes;
use core::time::Duration;
use std::io;
use minecraft_relay::discovery::DiscoveryListener;
use minecraft_relay::relay::{Relay, RelayConnectHandle};

const DEFAULT_PORT: u16 = 25566;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    let args = std::env::args().skip(1);

    let discovery_listener = DiscoveryListener::new().context("error listening for UDP game advertisements")?;

    let mut relay = Relay::new()?;

    relay.listen("0.0.0.0:25566").context("error listening for relay connections on tcp port 25566")?;

    if args.len() == 0 {
        let relay_handle = relay.handle();
        std::thread::spawn(move || {
            let mut handle: Option<RelayConnectHandle> = None;
            loop {
                println!(">>>> Enter host or host:port to connect to (e.g. example.com or example.com:25566): <<<<");
                if handle.is_some() {
                    println!(">>>> Press Enter or enter a new host to stop trying to connect: <<<<");
                }
                let mut line = String::new();
                let addr_str = match io::stdin().read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_len) => line.trim().to_string(),
                    Err(error) => {
                        log::warn!("error reading input: {:?}", error);
                        break;
                    }
                };
                if let Some(handle) = handle.take() {
                    handle.cancel();
                }
                if !addr_str.is_empty() {
                    let new_handle = if addr_str.contains(':') {
                        relay_handle.connect(addr_str)
                    } else {
                        relay_handle.connect((addr_str, DEFAULT_PORT))
                    };
                    handle = Some(new_handle);
                }
            }
        });
    }
    for connect_addr in args {
        relay.connect(connect_addr);
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
