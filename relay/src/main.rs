use anyhow::Context;
use bytes::Bytes;
use core::time::Duration;
use minecraft_relay::discovery::DiscoveryListener;
use minecraft_relay::relay::Relay;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    let args = std::env::args().skip(1);

    let discovery_listener = DiscoveryListener::new().context("error listening for UDP game advertisements")?;

    let mut relay = Relay::new()?;

    if args.len() == 0 {
        relay.listen("0.0.0.0:25566").context("error listening for relay connections on tcp port 25566")?;
    } else {
        for connect_addr in args {
            relay.connect(connect_addr);
        }

        let relay_handle = relay.handle();
        std::thread::spawn(move || {
            loop {
                let run_res = discovery_listener.run(|mut from, packet| {
                    from.set_port(packet.port);
                    relay_handle.add_local_game(from, Bytes::from(packet.motd.to_vec()))
                });
                match run_res {
                    Ok(never) => match never {},
                    Err(error) =>
                        log::warn!("error listening for UDP game advertisements: {:?}", error),
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    relay.run();

    Ok(())
}
