# Minecraft Relay

A Minecraft (Java Edition) LAN game relay. LAN games discovered on the local network by listening to UDP multicasts can
be advertised and connected to by relay peers. Multi-hop relaying is supported, allowing users to connect to a central
server in order to avoid NAT port forwarding.

## Building

```
$ cargo build --release
```

## Running

```
$ target/release/minecraft-relay [peer ip:port] ...
```
