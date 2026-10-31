pub mod ffi;

mod channels;
mod protocol;
mod wire;

pub use channels::SendResult;
pub use protocol::{FrameOut, NUM_CHANNELS, Protocol, Status, TransportConfig};
pub use wire::MAX_GROUP_BYTES;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::MIN_MTU;
use wire::{HEADER_BYTES, KIND_CONNECT, MAX_MTU};

pub const ABI_VERSION: u32 = 2;

pub const MAX_EVENT_INLINE: usize = 1200;
pub const MAX_UNRELIABLE_BYTES: usize = 8191;

const POLL_BURST: usize = 256;
const RECV_BUFFER: usize = 65536;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Delivery {
    Unreliable = 1,
    UnreliableSequenced = 2,
    ReliableUnordered = 34,
    ReliableSequenced = 35,
    ReliableOrdered = 67,
}

impl Delivery {
    pub fn kind(self, sequence_channel: u8) -> u8 {
        self as u8 + sequence_channel
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConnectionId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportEvent {
    Connected(ConnectionId),
    Disconnected(ConnectionId),
    Received(ConnectionId),
}

#[derive(Clone, Copy, Default, Debug)]
pub struct TransportStats {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub datagrams_sent: u64,
    pub datagrams_received: u64,
    pub datagrams_dropped: u64,
    pub retransmits: u64,
    pub connections: u32,
}

struct Wire {
    socket: UdpSocket,
    stats: TransportStats,
    loss_rate: f32,
    rng: u64,
}

impl Wire {
    fn send_datagram(&mut self, addr: SocketAddr, datagram: &[u8]) -> bool {
        if self.should_drop() {
            self.stats.datagrams_dropped += 1;
            return false;
        }
        match self.socket.send_to(datagram, addr) {
            Ok(_) => {
                self.stats.datagrams_sent += 1;
                self.stats.bytes_sent += datagram.len() as u64;
                true
            }
            Err(_) => false,
        }
    }

    fn should_drop(&mut self) -> bool {
        if self.loss_rate <= 0.0 {
            return false;
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let unit = (self.rng >> 40) as f32 / 16_777_216.0f32;
        unit < self.loss_rate
    }
}

struct Connection {
    id: ConnectionId,
    protocol: Protocol,
}

pub struct UdpTransport {
    wire: Wire,
    connections: HashMap<SocketAddr, Connection>,
    by_id: HashMap<ConnectionId, SocketAddr>,
    next_id: u64,
    events: Vec<(TransportEvent, Vec<u8>)>,
    overflow: VecDeque<Vec<u8>>,
    reasons: HashMap<ConnectionId, String>,
    config: TransportConfig,
    unique_id: u64,
    recv_buffer: Vec<u8>,
}

impl UdpTransport {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpTransport> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(UdpTransport {
            wire: Wire {
                socket,
                stats: TransportStats::default(),
                loss_rate: 0.0,
                rng: 0x9e3779b97f4a7c15,
            },
            connections: HashMap::new(),
            by_id: HashMap::new(),
            next_id: 1,
            events: Vec::new(),
            overflow: VecDeque::new(),
            reasons: HashMap::new(),
            config: TransportConfig::default(),
            unique_id: unique_identifier(),
            recv_buffer: vec![0u8; RECV_BUFFER],
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.wire.socket.local_addr()
    }

    pub fn remote_endpoint(&self, conn: ConnectionId) -> Option<SocketAddr> {
        self.by_id.get(&conn).copied()
    }

    pub fn set_timings(
        &mut self,
        ping_interval: Duration,
        timeout: Duration,
        resend_delay: Duration,
    ) {
        self.config.ping_interval = ping_interval;
        self.config.timeout = timeout;
        self.config.resend_delay = if resend_delay.is_zero() {
            None
        } else {
            Some(resend_delay)
        };
        if let Some(delay) = self.config.resend_delay {
            for connection in self.connections.values_mut() {
                connection.protocol.set_resend_delay(delay);
            }
        }
    }

    pub fn set_mtu(&mut self, mtu: usize) {
        self.config.mtu = mtu.clamp(MIN_MTU, MAX_MTU);
    }

    pub fn mtu(&self) -> usize {
        self.config.mtu
    }

    pub fn set_app_identifier(&mut self, app_identifier: &str) {
        self.config.app_identifier = app_identifier.to_owned();
    }

    pub fn set_loss_rate(&mut self, rate: f32) {
        self.wire.loss_rate = rate.clamp(0.0, 0.95);
    }

    pub fn connect(&mut self, addr: SocketAddr) -> ConnectionId {
        let id = ConnectionId(self.next_id);
        self.next_id += 1;
        self.connections.insert(
            addr,
            Connection {
                id,
                protocol: Protocol::new(true, Instant::now()),
            },
        );
        self.by_id.insert(id, addr);
        id
    }

    pub fn send(&mut self, conn: ConnectionId, delivery: Delivery, payload: &[u8]) -> bool {
        self.send_kind(conn, delivery.kind(0), payload)
    }

    pub fn send_kind(&mut self, conn: ConnectionId, kind: u8, payload: &[u8]) -> bool {
        let Some(addr) = self.by_id.get(&conn).copied() else {
            return false;
        };
        let result = match self.connections.get_mut(&addr) {
            Some(connection) => connection.protocol.send_user(&self.config, kind, payload),
            None => return false,
        };
        matches!(result, SendResult::Sent | SendResult::Queued)
    }

    pub fn set_key(&mut self, conn: ConnectionId, key: &[u8; 32]) -> bool {
        let Some(addr) = self.by_id.get(&conn).copied() else {
            return false;
        };
        match self.connections.get_mut(&addr) {
            Some(connection) => {
                connection.protocol.set_key(key);
                true
            }
            None => false,
        }
    }

    pub fn close(&mut self, conn: ConnectionId) {
        self.disconnect(conn, "Disconnected");
    }

    pub fn disconnect(&mut self, conn: ConnectionId, reason: &str) {
        self.disconnect_with(conn, reason, true);
    }

    pub fn disconnect_with(&mut self, conn: ConnectionId, reason: &str, send_bye: bool) {
        let Some(addr) = self.by_id.get(&conn).copied() else {
            return;
        };
        let mut out = FrameOut::new(self.config.mtu);
        if let Some(connection) = self.connections.get_mut(&addr) {
            connection.protocol.disconnect(reason, send_bye, &mut out);
        }
        for datagram in out.take() {
            self.wire.send_datagram(addr, &datagram);
        }
        if let Some(connection) = self.connections.remove(&addr) {
            self.by_id.remove(&connection.id);
            self.reasons.insert(connection.id, reason.to_owned());
            self.events
                .push((TransportEvent::Disconnected(connection.id), Vec::new()));
        }
    }

    pub fn rtt(&self, conn: ConnectionId) -> f32 {
        let Some(addr) = self.by_id.get(&conn) else {
            return -1.0;
        };
        match self.connections.get(addr) {
            Some(connection) => connection.protocol.rtt(),
            None => -1.0,
        }
    }

    pub fn time_offset(&self, conn: ConnectionId) -> f32 {
        let Some(addr) = self.by_id.get(&conn) else {
            return 0.0;
        };
        match self.connections.get(addr) {
            Some(connection) => connection.protocol.time_offset(),
            None => 0.0,
        }
    }

    pub fn disconnect_reason(&self, conn: ConnectionId) -> Option<&str> {
        self.reasons.get(&conn).map(String::as_str)
    }

    pub fn poll(&mut self, out: &mut Vec<(TransportEvent, Vec<u8>)>) {
        out.clear();
        self.reasons.clear();
        loop {
            let mut drained = 0;
            for _ in 0..POLL_BURST {
                let (len, from) = match self.wire.socket.recv_from(&mut self.recv_buffer) {
                    Ok(received) => received,
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                };
                drained += 1;
                let datagram = self.recv_buffer[..len].to_vec();
                self.route_datagram(from, &datagram);
            }

            let addresses: Vec<SocketAddr> = self.connections.keys().copied().collect();
            for addr in addresses {
                self.pump_connection(addr);
            }

            if drained < POLL_BURST {
                break;
            }
        }

        std::mem::swap(out, &mut self.events);
        self.events.clear();
    }

    pub fn stats(&self) -> TransportStats {
        let mut stats = self.wire.stats;
        stats.connections = self.connections.len() as u32;
        stats.retransmits = self
            .connections
            .values()
            .map(|connection| connection.protocol.retransmits)
            .sum();
        stats
    }

    pub fn queue_overflow(&mut self, payload: Vec<u8>) {
        self.overflow.push_back(payload);
    }

    pub fn fetch_overflow(&mut self, buf: &mut [u8]) -> usize {
        let Some(payload) = self.overflow.front() else {
            return 0;
        };
        if payload.len() > buf.len() {
            return payload.len();
        }
        let payload = self.overflow.pop_front().expect("front checked");
        buf[..payload.len()].copy_from_slice(&payload);
        payload.len()
    }

    fn route_datagram(&mut self, from: SocketAddr, datagram: &[u8]) {
        self.wire.stats.datagrams_received += 1;
        self.wire.stats.bytes_received += datagram.len() as u64;
        if datagram.len() < HEADER_BYTES {
            self.wire.stats.datagrams_dropped += 1;
            return;
        }
        if !self.connections.contains_key(&from) {
            if datagram[0] != KIND_CONNECT {
                self.wire.stats.datagrams_dropped += 1;
                return;
            }
            let id = ConnectionId(self.next_id);
            self.next_id += 1;
            self.connections.insert(
                from,
                Connection {
                    id,
                    protocol: Protocol::new(false, Instant::now()),
                },
            );
            self.by_id.insert(id, from);
        }
        let now = Instant::now();
        let unique_id = self.unique_id;
        let mut out = FrameOut::new(self.config.mtu);
        if let Some(connection) = self.connections.get_mut(&from) {
            connection
                .protocol
                .handle_datagram(now, &self.config, unique_id, datagram, &mut out);
        }
        for datagram in out.take() {
            self.wire.send_datagram(from, &datagram);
        }
        self.collect_connection(from);
    }

    fn pump_connection(&mut self, addr: SocketAddr) {
        let now = Instant::now();
        let unique_id = self.unique_id;
        let mut out = FrameOut::new(self.config.mtu);
        {
            let Some(connection) = self.connections.get_mut(&addr) else {
                return;
            };
            connection
                .protocol
                .tick(now, &self.config, unique_id, &mut out);
        }
        for datagram in out.take() {
            self.wire.send_datagram(addr, &datagram);
        }
        self.collect_connection(addr);
    }

    fn collect_connection(&mut self, addr: SocketAddr) {
        let mut finished: Option<(ConnectionId, String)> = None;
        let mut incoming: Vec<(TransportEvent, Vec<u8>)> = Vec::new();
        if let Some(connection) = self.connections.get_mut(&addr) {
            let id = connection.id;
            if connection.protocol.take_connected() {
                incoming.push((TransportEvent::Connected(id), Vec::new()));
            }
            for payload in connection.protocol.take_released() {
                incoming.push((TransportEvent::Received(id), payload));
            }
            if connection.protocol.is_dead() {
                let reason = connection
                    .protocol
                    .reason()
                    .unwrap_or("Connection closed")
                    .to_owned();
                finished = Some((id, reason));
            }
        }
        self.events.extend(incoming);
        if let Some((id, reason)) = finished {
            self.connections.remove(&addr);
            self.by_id.remove(&id);
            self.reasons.insert(id, reason);
            self.events
                .push((TransportEvent::Disconnected(id), Vec::new()));
        }
    }
}

fn unique_identifier() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0x5eed_0000_0000_0001);
    let pid = u64::from(std::process::id());
    (nanos ^ (pid << 32) ^ 0x9e37_79b9_7f4a_7c15) | 1
}
