use bytes::BufMut;
use core::str;
use nom::IResult;
use socket2::Socket;
use std::collections::HashMap;
use std::convert::Infallible;
use std::error::Error;
use std::io;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

pub struct DiscoveryListener {
    socket: UdpSocket,
}

pub struct DiscoverySender {
    socket: UdpSocket,
}

#[derive(Clone, Debug)]
pub struct DiscoveryPacket<'a> {
    pub port: u16,
    pub motd: &'a [u8],
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryListenerError<T: Error + 'static> {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Returned(T),
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoverySendError {
    #[error("encode error: {0}")]
    EncodeError(DiscoveryPacketEncodeError),
    #[error("send io error: {0}")]
    IoError(#[from] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryPacketParseError<'a> {
    #[error("parse error: {0}")]
    ParseError(nom::Err<nom::error::Error<&'a [u8]>>),
    #[error("invalid port: {0:?}")]
    InvalidPort(&'a [u8]),
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryPacketEncodeError {
    #[error("invalid MOTD: {0:?}")]
    InvalidMotd(Box<[u8]>),
    #[error("buffer too small")]
    BufferTooSmall,
}

const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
const PORT: u16 = 4445;
const MAX_PACKET_SIZE: usize = 1500;

//
// DiscoveryListener impls
//

impl DiscoveryListener {
    pub fn new() -> io::Result<Self> {
        let socket = Socket::new(socket2::Domain::ipv4(), socket2::Type::dgram(), None)?;
        socket.set_reuse_address(true)?;
        socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
        socket.join_multicast_v4(&MULTICAST_GROUP, &Ipv4Addr::UNSPECIFIED)?;
        Ok(Self { socket: socket.into() })
    }

    pub fn run<E, F>(&self, mut callback: F) -> Result<Infallible, DiscoveryListenerError<E>>
    where E: Error + 'static,
          F: FnMut(SocketAddr, DiscoveryPacket) -> Result<(), E>,
    {
        let mut buf = [0; MAX_PACKET_SIZE];
        loop {
            let (len, from) = self.socket.recv_from(&mut buf)?;
            match DiscoveryPacket::parse(&buf[..len]) {
                Ok(packet) => callback(from, packet).map_err(DiscoveryListenerError::Returned)?,
                Err(error) => log::warn!("error parsing discovery packet from {}: {:?}", &from, error),
            }
        }
    }
}

//
// DiscoverySender impls
//

impl DiscoverySender {
    pub fn new() -> io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_multicast_loop_v4(true)?;
        Ok(Self { socket })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn send<'a>(&self, packet: &DiscoveryPacket<'a>) -> Result<(), DiscoverySendError> {
        let mut data_buf = [0; 1500];
        let mut data_buf_unused = &mut data_buf[..];
        packet.encode(&mut data_buf_unused).map_err(DiscoverySendError::EncodeError)?;
        let data_buf_unused_len = data_buf_unused.len();
        let data = &data_buf[..data_buf.len() - data_buf_unused_len];
        self.socket.send_to(data, (MULTICAST_GROUP, PORT))?;
        Ok(())
    }
}

//
// DiscoveryPacket impls
//

impl<'a> DiscoveryPacket<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, DiscoveryPacketParseError<'a>> {
        let (_, map) = elements(data).map_err(DiscoveryPacketParseError::ParseError)?;

        let port_data = map.get(&b"AD"[..]).ok_or(DiscoveryPacketParseError::InvalidPort(b""))?;
        let port = str::from_utf8(port_data).map_err(|_| DiscoveryPacketParseError::InvalidPort(port_data))?;
        let port = port.parse().map_err(|_| DiscoveryPacketParseError::InvalidPort(port_data))?;
        let motd = map.get(&b"MOTD"[..]).map(|motd| *motd).unwrap_or_default();
        Ok(Self { port, motd })
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<(), DiscoveryPacketEncodeError> {
        if self.motd.contains(&b"[/"[0]) || self.motd.len() > 256 {
            return Err(DiscoveryPacketEncodeError::InvalidMotd(self.motd.to_vec().into()));
        }

        write!(buf.writer(), "[MOTD]").map_err(|_| DiscoveryPacketEncodeError::BufferTooSmall)?;
        if buf.remaining_mut() < self.motd.len() {
            return Err(DiscoveryPacketEncodeError::BufferTooSmall);
        }
        buf.put_slice(self.motd);
        write!(buf.writer(), "[/MOTD][AD]{}[/AD]", self.port).map_err(|_| DiscoveryPacketEncodeError::BufferTooSmall)?;

        Ok(())
    }
}

//
// private
//

fn elements(input: &[u8]) -> IResult<&[u8], HashMap<&[u8], &[u8]>> {
    nom::multi::fold_many0(element, Default::default(), |mut map: HashMap<_, _>, (k, v)| {
        map.insert(k, v);
        map
    })(input)
}

fn element(input: &[u8]) -> IResult<&[u8], (&[u8], &[u8])> {
    let (input, name) = nom::sequence::delimited(
        nom::character::complete::char('['),
        nom::bytes::complete::is_not("]"),
        nom::character::complete::char(']'),
    )(input)?;

    let (input, data) = nom::bytes::complete::take_until("[/")(input)?;

    let (input, _) = nom::sequence::tuple((
        nom::bytes::complete::tag("[/"),
        nom::bytes::complete::tag(name),
        nom::character::complete::char(']'),
    ))(input)?;
    Ok((input, (name, data)))
}
