pub mod ffi;

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

pub const ABI_VERSION: u32 = 1;

pub const MAX_DATAGRAM: usize = 1200;

pub const KIND_PAYLOAD_UNRELIABLE: u8 = 0;
pub const KIND_PAYLOAD_RELIABLE: u8 = 1;
pub const KIND_ACK: u8 = 2;
pub const KIND_HELLO: u8 = 3;
pub const KIND_HELLO_ACK: u8 = 4;
pub const KIND_DISCONNECT: u8 = 5;
pub const KIND_HEARTBEAT: u8 = 6;

pub const HEADER_BYTES: usize = 9;

pub const DEFAULT_HEARTBEAT: Duration = Duration::from_millis(100);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1000);
pub const DEFAULT_RTO: Duration = Duration::from_millis(60);

const POLL_BURST: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Delivery {
    Unreliable,
    ReliableOrdered,
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
    send_buffer: Vec<u8>,
    stats: TransportStats,
    loss_rate: f32,
    rng: u64,
}

impl Wire {
    fn send(&mut self, addr: SocketAddr, kind: u8, seq: u64, payload: &[u8]) -> bool {
        if self.should_drop() {
            self.stats.datagrams_dropped += 1;
            return false;
        }
        self.send_buffer.clear();
        self.send_buffer.push(kind);
        self.send_buffer.extend_from_slice(&seq.to_le_bytes());
        self.send_buffer.extend_from_slice(payload);
        match self.socket.send_to(&self.send_buffer, addr) {
            Ok(_) => {
                self.stats.datagrams_sent += 1;
                self.stats.bytes_sent += self.send_buffer.len() as u64;
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
    outgoing: u64,
    reliable_outgoing: u64,
    expected_incoming: u64,
    pending: Option<(u64, Vec<u8>, Instant)>,
    outbox: Vec<Vec<u8>>,
    last_received: Instant,
    last_heartbeat: Instant,
    established: bool,
    closed: bool,
}

pub struct UdpTransport {
    wire: Wire,
    connections: HashMap<SocketAddr, Connection>,
    by_id: HashMap<ConnectionId, SocketAddr>,
    next_id: u64,
    events: Vec<(TransportEvent, Vec<u8>)>,
    recv_buffer: Vec<u8>,
    heartbeat: Duration,
    timeout: Duration,
    rto: Duration,
}

impl UdpTransport {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpTransport> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(UdpTransport {
            wire: Wire {
                socket,
                send_buffer: Vec::new(),
                stats: TransportStats::default(),
                loss_rate: 0.0,
                rng: 0x9e3779b97f4a7c15,
            },
            connections: HashMap::new(),
            by_id: HashMap::new(),
            next_id: 1,
            events: Vec::new(),
            recv_buffer: vec![0u8; MAX_DATAGRAM + HEADER_BYTES],
            heartbeat: DEFAULT_HEARTBEAT,
            timeout: DEFAULT_TIMEOUT,
            rto: DEFAULT_RTO,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.wire.socket.local_addr()
    }

    pub fn remote_endpoint(&self, conn: ConnectionId) -> Option<SocketAddr> {
        self.by_id.get(&conn).copied()
    }

    pub fn set_timings(&mut self, heartbeat: Duration, timeout: Duration, rto: Duration) {
        self.heartbeat = heartbeat;
        self.timeout = timeout;
        self.rto = rto;
    }

    pub fn set_loss_rate(&mut self, rate: f32) {
        self.wire.loss_rate = rate.clamp(0.0, 0.95);
    }

    pub fn connect(&mut self, addr: SocketAddr) -> ConnectionId {
        let id = ConnectionId(self.next_id);
        self.next_id += 1;
        let now = Instant::now();
        self.connections.insert(
            addr,
            Connection {
                id,
                outgoing: 0,
                reliable_outgoing: 0,
                expected_incoming: 0,
                pending: None,
                outbox: Vec::new(),
                last_received: now,
                last_heartbeat: now,
                established: false,
                closed: false,
            },
        );
        self.by_id.insert(id, addr);
        self.wire.send(addr, KIND_HELLO, 0, &[]);
        id
    }

    pub fn send(&mut self, conn: ConnectionId, delivery: Delivery, payload: &[u8]) -> bool {
        let Some(addr) = self.by_id.get(&conn).copied() else {
            return false;
        };
        let Some(connection) = self.connections.get_mut(&addr) else {
            return false;
        };
        if connection.closed || payload.len() > MAX_DATAGRAM {
            return false;
        }

        match delivery {
            Delivery::ReliableOrdered => {
                connection.outbox.push(payload.to_vec());
            }
            Delivery::Unreliable => {
                let seq = connection.outgoing;
                connection.outgoing += 1;
                return self.wire.send(addr, KIND_PAYLOAD_UNRELIABLE, seq, payload);
            }
        }

        self.pump_connection(addr);
        true
    }

    pub fn poll(&mut self, out: &mut Vec<(TransportEvent, Vec<u8>)>) {
        out.clear();
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
                self.handle_datagram(from, &datagram);
            }

            let addresses: Vec<SocketAddr> = self.connections.keys().copied().collect();
            for addr in addresses {
                self.pump_connection(addr);
            }
            self.check_timeouts();

            if drained < POLL_BURST {
                break;
            }
        }

        std::mem::swap(out, &mut self.events);
        self.events.clear();
    }

    pub fn close(&mut self, conn: ConnectionId) {
        let Some(addr) = self.by_id.get(&conn).copied() else {
            return;
        };
        self.wire.send(addr, KIND_DISCONNECT, 0, &[]);
        self.remove_connection(addr);
    }

    pub fn stats(&self) -> TransportStats {
        let mut stats = self.wire.stats;
        stats.connections = self.connections.len() as u32;
        stats
    }

    fn pump_connection(&mut self, addr: SocketAddr) {
        let now = Instant::now();
        let mut action = PumpAction::Idle;
        let mut beat = false;
        {
            let Some(connection) = self.connections.get_mut(&addr) else {
                return;
            };
            if connection.closed {
                return;
            }
            if let Some((seq, payload, last_sent)) = connection.pending.as_mut() {
                if now.duration_since(*last_sent) >= self.rto {
                    *last_sent = now;
                    action = PumpAction::Resend(*seq, payload.clone());
                }
            } else if let Some(payload) = connection.outbox.first().cloned() {
                let seq = connection.reliable_outgoing;
                connection.reliable_outgoing += 1;
                connection.outbox.remove(0);
                connection.pending = Some((seq, payload.clone(), now));
                action = PumpAction::Send(seq, payload);
            }

            if now.duration_since(connection.last_heartbeat) >= self.heartbeat {
                connection.last_heartbeat = now;
                beat = true;
            }
        }

        match action {
            PumpAction::Send(seq, payload) => {
                self.wire.send(addr, KIND_PAYLOAD_RELIABLE, seq, &payload);
            }
            PumpAction::Resend(seq, payload) => {
                self.wire.stats.retransmits += 1;
                self.wire.send(addr, KIND_PAYLOAD_RELIABLE, seq, &payload);
            }
            PumpAction::Idle => {}
        }

        if beat {
            self.wire.send(addr, KIND_HEARTBEAT, 0, &[]);
        }
    }

    fn check_timeouts(&mut self) {
        let now = Instant::now();
        let expired: Vec<SocketAddr> = self
            .connections
            .iter()
            .filter(|(_, connection)| now.duration_since(connection.last_received) >= self.timeout)
            .map(|(addr, _)| *addr)
            .collect();
        for addr in expired {
            self.remove_connection(addr);
        }
    }

    fn remove_connection(&mut self, addr: SocketAddr) {
        if let Some(connection) = self.connections.remove(&addr) {
            self.by_id.remove(&connection.id);
            self.events
                .push((TransportEvent::Disconnected(connection.id), Vec::new()));
        }
    }

    fn handle_datagram(&mut self, from: SocketAddr, datagram: &[u8]) {
        self.wire.stats.datagrams_received += 1;
        if datagram.len() < HEADER_BYTES {
            self.wire.stats.datagrams_dropped += 1;
            return;
        }
        let kind = datagram[0];
        let seq = u64::from_le_bytes(datagram[1..HEADER_BYTES].try_into().unwrap());
        let payload = &datagram[HEADER_BYTES..];
        self.wire.stats.bytes_received += datagram.len() as u64;

        match kind {
            KIND_HELLO => self.handle_hello(from),
            KIND_HELLO_ACK => {
                if let Some(connection) = self.connections.get_mut(&from) {
                    connection.last_received = Instant::now();
                    if !connection.established {
                        connection.established = true;
                        let id = connection.id;
                        self.events
                            .push((TransportEvent::Connected(id), Vec::new()));
                    }
                } else {
                    self.wire.stats.datagrams_dropped += 1;
                }
            }
            KIND_HEARTBEAT => {
                if let Some(connection) = self.connections.get_mut(&from) {
                    connection.last_received = Instant::now();
                } else {
                    self.wire.stats.datagrams_dropped += 1;
                }
            }
            KIND_DISCONNECT => {
                if self.connections.contains_key(&from) {
                    self.remove_connection(from);
                }
            }
            KIND_ACK => {
                if let Some(connection) = self.connections.get_mut(&from) {
                    connection.last_received = Instant::now();
                    if connection.pending.as_ref().is_some_and(|p| p.0 == seq) {
                        connection.pending = None;
                    }
                } else {
                    self.wire.stats.datagrams_dropped += 1;
                }
            }
            KIND_PAYLOAD_UNRELIABLE => {
                if let Some(connection) = self.connections.get_mut(&from) {
                    connection.last_received = Instant::now();
                    let id = connection.id;
                    self.events
                        .push((TransportEvent::Received(id), payload.to_vec()));
                } else {
                    self.wire.stats.datagrams_dropped += 1;
                }
            }
            KIND_PAYLOAD_RELIABLE => {
                if let Some(connection) = self.connections.get_mut(&from) {
                    connection.last_received = Instant::now();
                    let id = connection.id;
                    let expected = connection.expected_incoming;
                    if seq == expected {
                        connection.expected_incoming += 1;
                        self.wire.send(from, KIND_ACK, seq, &[]);
                        self.events
                            .push((TransportEvent::Received(id), payload.to_vec()));
                    } else if seq < expected {
                        self.wire.send(from, KIND_ACK, seq, &[]);
                    } else {
                        self.wire.stats.datagrams_dropped += 1;
                    }
                } else {
                    self.wire.stats.datagrams_dropped += 1;
                }
            }
            _ => {
                self.wire.stats.datagrams_dropped += 1;
            }
        }
    }

    fn handle_hello(&mut self, from: SocketAddr) {
        let now = Instant::now();
        if let Some(connection) = self.connections.get_mut(&from) {
            connection.last_received = now;
            self.wire.send(from, KIND_HELLO_ACK, 0, &[]);
            return;
        }

        let id = ConnectionId(self.next_id);
        self.next_id += 1;
        self.connections.insert(
            from,
            Connection {
                id,
                outgoing: 0,
                reliable_outgoing: 0,
                expected_incoming: 0,
                pending: None,
                outbox: Vec::new(),
                last_received: now,
                last_heartbeat: now,
                established: true,
                closed: false,
            },
        );
        self.by_id.insert(id, from);
        self.wire.send(from, KIND_HELLO_ACK, 0, &[]);
        self.events
            .push((TransportEvent::Connected(id), Vec::new()));
    }
}

enum PumpAction {
    Idle,
    Send(u64, Vec<u8>),
    Resend(u64, Vec<u8>),
}
