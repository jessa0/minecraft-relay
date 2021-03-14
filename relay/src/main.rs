use bytes::Bytes;
use minecraft_relay::discovery::DiscoveryListener;
use minecraft_relay::relay::Relay;

fn main() {
    env_logger::init();

    let mut args = std::env::args().skip(1);

    let discovery_listener = DiscoveryListener::new().unwrap();

    let mut relay = Relay::new().unwrap();
    relay.listen("0.0.0.0:25566").unwrap();

    if let Some(connect_addr) = args.next() {
        relay.connect(connect_addr);

        let relay_handle = relay.handle();
        std::thread::spawn(move || {
            discovery_listener.run(|mut from, packet| {
                from.set_port(packet.port);
                relay_handle.add_local_game(from, Bytes::from(packet.motd.to_vec())).unwrap();
            }).unwrap();
        });
    }

    relay.run();
}
