use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use singularity_crypto::{FRAME_OVERHEAD, Key, Session, Side};

use crate::channels::{
    Incoming, Outcome, Outgoing, ReceiverChannel, SendResult, SenderChannel, is_reliable,
};
use crate::wire::{
    Cursor, FragmentHeader, HEADER_BYTES, KIND_ACKNOWLEDGE, KIND_CONNECT, KIND_CONNECT_RESPONSE,
    KIND_CONNECTION_ESTABLISHED, KIND_DISCONNECT, KIND_EXPAND_MTU_REQUEST, KIND_EXPAND_MTU_SUCCESS,
    KIND_PING, KIND_PONG, KIND_UNCONNECTED, MAX_CHUNK_BYTES, MAX_CONCURRENT_GROUPS,
    MAX_GROUP_BYTES, MAX_MTU, MessageRef, best_chunk_size, bytes_to_hold_bits, disconnect_payload,
    established_payload, fragment_header_size, handshake_payload, is_library_type, is_user_type,
    parse_ack_payload, parse_disconnect, parse_handshake, parse_pong, ping_payload, pong_payload,
    read_fragment_header, write_fragment_header, write_header,
};

pub const NUM_CHANNELS: usize = 99;
pub const MAX_FRAGMENT_GROUPS: u32 = 65534;
pub const MIN_MTU: usize = 64;
pub const MAX_PROBE_MTU: usize = 8190;
pub const DEFAULT_APP_IDENTIFIER: &str = "RobustToolbox";
pub const DEFAULT_MTU: usize = 700;
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(1);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(25);
pub const DEFAULT_HANDSHAKE_INTERVAL: Duration = Duration::from_secs(3);
pub const DEFAULT_MAX_HANDSHAKE_ATTEMPTS: u32 = 5;
pub const DEFAULT_FRAGMENT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_REASSEMBLY_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_EXPAND_MTU_FREQUENCY: Duration = Duration::from_secs(2);
pub const DEFAULT_EXPAND_MTU_FAIL_ATTEMPTS: u32 = 5;

const UNUSED_KIND_FIRST: u8 = 99;
const UNUSED_KIND_LAST: u8 = 127;
const MAX_UNRELIABLE_BYTES: usize = 8191;

pub struct TransportConfig {
    pub app_identifier: String,
    pub mtu: usize,
    pub ping_interval: Duration,
    pub timeout: Duration,
    pub resend_delay: Option<Duration>,
    pub handshake_interval: Duration,
    pub max_handshake_attempts: u32,
    pub fragment_timeout: Duration,
    pub reassembly_bytes: usize,
    pub expand_mtu: bool,
    pub expand_mtu_frequency: Duration,
    pub expand_mtu_fail_attempts: u32,
}

impl Default for TransportConfig {
    fn default() -> TransportConfig {
        TransportConfig {
            app_identifier: DEFAULT_APP_IDENTIFIER.to_owned(),
            mtu: DEFAULT_MTU,
            ping_interval: DEFAULT_PING_INTERVAL,
            timeout: DEFAULT_TIMEOUT,
            resend_delay: None,
            handshake_interval: DEFAULT_HANDSHAKE_INTERVAL,
            max_handshake_attempts: DEFAULT_MAX_HANDSHAKE_ATTEMPTS,
            fragment_timeout: DEFAULT_FRAGMENT_TIMEOUT,
            reassembly_bytes: DEFAULT_REASSEMBLY_BYTES,
            expand_mtu: false,
            expand_mtu_frequency: DEFAULT_EXPAND_MTU_FREQUENCY,
            expand_mtu_fail_attempts: DEFAULT_EXPAND_MTU_FAIL_ATTEMPTS,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Connecting,
    AwaitingConnect,
    Responded,
    Connected,
    Dead,
}

pub struct FrameOut {
    mtu: usize,
    current: Vec<u8>,
    ready: Vec<Vec<u8>>,
}

impl FrameOut {
    pub fn new(mtu: usize) -> FrameOut {
        FrameOut {
            mtu,
            current: Vec::with_capacity(mtu),
            ready: Vec::new(),
        }
    }

    pub fn push_outgoing(&mut self, message: &Outgoing, seq: u16) {
        let payload = &message.data[message.offset..message.offset + message.len];
        self.push_message(message.kind, seq, message.frag.as_ref(), payload);
    }

    pub fn push_message(
        &mut self,
        kind: u8,
        seq: u16,
        frag: Option<&FragmentHeader>,
        payload: &[u8],
    ) {
        let frag_bytes = frag
            .map(|meta| {
                let total_bytes = bytes_to_hold_bits(meta.total_bits);
                let num_chunks = total_bytes.div_ceil(meta.chunk_size.max(1));
                fragment_header_size(meta.group, total_bytes, meta.chunk_size, num_chunks)
            })
            .unwrap_or(0);
        let size = HEADER_BYTES + frag_bytes + payload.len();
        if !self.current.is_empty() && self.current.len() + size > self.mtu {
            self.flush();
        }
        let start = self.current.len();
        self.current.resize(start + HEADER_BYTES, 0);
        let bit_len = ((frag_bytes + payload.len()) * 8) as u16;
        write_header(
            &mut self.current[start..],
            kind,
            seq,
            frag.is_some(),
            bit_len,
        );
        if let Some(meta) = frag {
            let start = self.current.len();
            self.current.resize(start + 24, 0);
            let written = write_fragment_header(&mut self.current[start..], meta);
            self.current.truncate(start + written);
        }
        self.current.extend_from_slice(payload);
        if self.current.len() > self.mtu {
            self.flush();
        }
    }

    pub fn push_standalone(&mut self, kind: u8, payload: &[u8]) {
        self.flush();
        self.push_message(kind, 0, None, payload);
        self.flush();
    }

    pub fn push_acks(&mut self, acks: &mut VecDeque<(u8, u16)>) {
        while !acks.is_empty() {
            let room = self.mtu.saturating_sub(self.current.len() + HEADER_BYTES) / 3;
            if room == 0 {
                self.flush();
                continue;
            }
            let count = room.min(acks.len());
            let bit_len = (count * 3 * 8) as u16;
            let start = self.current.len();
            self.current.resize(start + HEADER_BYTES, 0);
            write_header(
                &mut self.current[start..],
                KIND_ACKNOWLEDGE,
                0,
                false,
                bit_len,
            );
            for _ in 0..count {
                let (kind, seq) = acks.pop_front().expect("ack queue not empty");
                self.current.push(kind);
                self.current.extend_from_slice(&seq.to_le_bytes());
            }
            if !acks.is_empty() {
                self.flush();
            }
        }
    }

    pub fn flush(&mut self) {
        if !self.current.is_empty() {
            self.ready.push(std::mem::take(&mut self.current));
        }
    }

    pub fn take(&mut self) -> Vec<Vec<u8>> {
        self.flush();
        std::mem::take(&mut self.ready)
    }
}

struct Reassembly {
    data: Vec<u8>,
    bits: Vec<u64>,
    total_bits: u32,
    chunk_size: usize,
    total_bytes: usize,
    num_chunks: usize,
    count: usize,
    last_received: Instant,
}

impl Reassembly {
    fn new(
        total_bytes: usize,
        total_bits: u32,
        chunk_size: usize,
        num_chunks: usize,
    ) -> Reassembly {
        Reassembly {
            data: vec![0u8; total_bytes],
            bits: vec![0u64; num_chunks.div_ceil(64)],
            total_bits,
            chunk_size,
            total_bytes,
            num_chunks,
            count: 0,
            last_received: Instant::now(),
        }
    }

    fn mark(&mut self, chunk: usize) -> bool {
        if (self.bits[chunk / 64] >> (chunk % 64)) & 1 == 1 {
            return false;
        }
        self.bits[chunk / 64] |= 1 << (chunk % 64);
        self.count += 1;
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MtuStatus {
    Idle,
    Probing,
    Finished,
}

struct MtuState {
    status: MtuStatus,
    current: usize,
    largest_successful: usize,
    smallest_failed: Option<usize>,
    last_sent_size: usize,
    last_sent_at: Option<Instant>,
    fails: u32,
    discovered: bool,
}

impl MtuState {
    fn fresh() -> MtuState {
        MtuState {
            status: MtuStatus::Idle,
            current: 0,
            largest_successful: 0,
            smallest_failed: None,
            last_sent_size: 0,
            last_sent_at: None,
            fails: 0,
            discovered: false,
        }
    }

    fn start(&mut self, floor: usize) {
        self.status = MtuStatus::Idle;
        self.current = floor;
        self.largest_successful = floor;
        self.smallest_failed = None;
        self.last_sent_size = 0;
        self.last_sent_at = None;
        self.fails = 0;
        self.discovered = false;
    }
}

pub struct Protocol {
    pub initiator: bool,
    pub status: Status,
    pub remote_unique_id: u64,
    pub avg_rtt: f64,
    pub remote_time_offset: f64,
    pub retransmits: u64,
    send_channels: Vec<Option<SenderChannel>>,
    recv_channels: Vec<Option<ReceiverChannel>>,
    queued_acks: VecDeque<(u8, u16)>,
    incoming_acks: VecDeque<(u8, u16)>,
    fragment_groups: HashMap<u32, Reassembly>,
    group_counter: u32,
    ping_number: u8,
    ping_count: u32,
    ping_sent_secs: f64,
    last_ping: Instant,
    last_activity: Instant,
    timeout_grace: bool,
    handshake_sent: Instant,
    handshake_attempts: u32,
    crypto: Option<Session>,
    clock_start: Instant,
    released: Vec<Vec<u8>>,
    connected_pending: bool,
    dead_reason: Option<String>,
    mtu: MtuState,
}

impl Protocol {
    pub fn new(initiator: bool, now: Instant) -> Protocol {
        Protocol {
            initiator,
            status: if initiator {
                Status::Connecting
            } else {
                Status::AwaitingConnect
            },
            remote_unique_id: 0,
            avg_rtt: -1.0,
            remote_time_offset: 0.0,
            retransmits: 0,
            send_channels: (0..NUM_CHANNELS).map(|_| None).collect(),
            recv_channels: (0..NUM_CHANNELS).map(|_| None).collect(),
            queued_acks: VecDeque::new(),
            incoming_acks: VecDeque::new(),
            fragment_groups: HashMap::new(),
            group_counter: 0,
            ping_number: 0,
            ping_count: 0,
            ping_sent_secs: 0.0,
            last_ping: now,
            last_activity: now,
            timeout_grace: false,
            handshake_sent: now,
            handshake_attempts: 0,
            crypto: None,
            clock_start: now,
            released: Vec::new(),
            connected_pending: false,
            dead_reason: None,
            mtu: MtuState::fresh(),
        }
    }

    pub fn set_key(&mut self, key: &Key) {
        let side = if self.initiator {
            Side::Client
        } else {
            Side::Server
        };
        self.crypto = Some(Session::new(key, side));
    }

    pub fn set_resend_delay(&mut self, delay: Duration) {
        for slot in 0..NUM_CHANNELS {
            if let Some(channel) = self.send_channels[slot].as_mut() {
                channel.set_resend_delay(delay);
            }
        }
    }

    pub fn current_mtu(&self, cfg: &TransportConfig) -> usize {
        if self.mtu.current == 0 {
            cfg.mtu.clamp(MIN_MTU, MAX_MTU)
        } else {
            self.mtu.current
        }
    }

    pub fn reset_mtu_floor(&mut self, floor: usize) {
        let floor = floor.clamp(MIN_MTU, MAX_MTU);
        if self.mtu.status != MtuStatus::Probing && !self.mtu.discovered {
            self.mtu.current = floor;
            self.mtu.largest_successful = floor;
        }
    }

    pub fn take_released(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.released)
    }

    pub fn take_connected(&mut self) -> bool {
        std::mem::take(&mut self.connected_pending)
    }

    pub fn is_dead(&self) -> bool {
        self.status == Status::Dead
    }

    pub fn reason(&self) -> Option<&str> {
        self.dead_reason.as_deref()
    }

    pub fn rtt(&self) -> f32 {
        if self.avg_rtt < 0.0 {
            -1.0
        } else {
            self.avg_rtt as f32
        }
    }

    pub fn time_offset(&self) -> f32 {
        self.remote_time_offset as f32
    }

    pub fn disconnect(&mut self, reason: &str, send_bye: bool, out: &mut FrameOut) {
        if self.status == Status::Dead {
            return;
        }
        if send_bye {
            out.push_standalone(KIND_DISCONNECT, &disconnect_payload(reason));
        }
        self.status = Status::Dead;
        self.dead_reason = Some(reason.to_owned());
    }

    pub fn send_user(&mut self, cfg: &TransportConfig, kind: u8, payload: &[u8]) -> SendResult {
        if self.status != Status::Connected {
            return SendResult::NotConnected;
        }
        if payload.is_empty() || !is_user_type(kind) {
            return SendResult::Dropped;
        }
        let reliable = is_reliable(kind);
        let overhead = if self.crypto.is_some() {
            FRAME_OVERHEAD
        } else {
            0
        };
        if reliable && payload.len() + overhead > MAX_GROUP_BYTES {
            return SendResult::Dropped;
        }
        if !reliable && payload.len() + overhead > MAX_UNRELIABLE_BYTES {
            return SendResult::Dropped;
        }
        let wire_payload = match &self.crypto {
            Some(session) => session.encrypt(payload),
            None => payload.to_vec(),
        };
        let mtu = self.current_mtu(cfg);
        let slot = (kind - 1) as usize;
        if reliable && HEADER_BYTES + wire_payload.len() > mtu {
            self.group_counter = if self.group_counter >= MAX_FRAGMENT_GROUPS {
                1
            } else {
                self.group_counter + 1
            };
            let group = self.group_counter;
            let chunk_size = best_chunk_size(group, wire_payload.len(), mtu);
            let total_bits = (wire_payload.len() * 8) as u32;
            let num_chunks = wire_payload.len().div_ceil(chunk_size);
            let data = Rc::new(wire_payload);
            let mut result = SendResult::Sent;
            for chunk_number in 0..num_chunks as u32 {
                let meta = FragmentHeader {
                    group,
                    total_bits,
                    chunk_size,
                    chunk_number,
                };
                let channel =
                    self.send_channels[slot].get_or_insert_with(|| SenderChannel::new(kind));
                let enqueued = channel.enqueue(Outgoing::chunk(kind, data.clone(), meta));
                if enqueued == SendResult::Dropped {
                    result = SendResult::Dropped;
                }
            }
            result
        } else {
            let channel = self.send_channels[slot].get_or_insert_with(|| SenderChannel::new(kind));
            channel.enqueue(Outgoing::whole(kind, Rc::new(wire_payload)))
        }
    }

    pub fn tick(
        &mut self,
        now: Instant,
        cfg: &TransportConfig,
        unique_id: u64,
        out: &mut FrameOut,
    ) {
        if self.status == Status::Dead {
            return;
        }
        match self.status {
            Status::Connecting => {
                if self.handshake_attempts == 0
                    || now.duration_since(self.handshake_sent) >= cfg.handshake_interval
                {
                    if self.handshake_attempts > 0
                        && self.handshake_attempts >= cfg.max_handshake_attempts
                    {
                        self.disconnect(
                            "Failed to establish connection - no response from remote host",
                            false,
                            out,
                        );
                        return;
                    }
                    self.handshake_attempts += 1;
                    self.handshake_sent = now;
                    let payload =
                        handshake_payload(&cfg.app_identifier, unique_id, self.local_time(), &[]);
                    out.push_standalone(KIND_CONNECT, &payload);
                }
            }
            Status::Responded => {
                if now.duration_since(self.handshake_sent) >= cfg.handshake_interval {
                    if self.handshake_attempts >= cfg.max_handshake_attempts {
                        self.disconnect(
                            "Failed to establish connection - no response from remote host",
                            false,
                            out,
                        );
                        return;
                    }
                    self.send_connect_response(now, cfg, unique_id, out);
                }
            }
            Status::Connected => {
                let limit = if self.timeout_grace {
                    cfg.timeout + cfg.timeout
                } else {
                    cfg.timeout
                };
                if now.duration_since(self.last_activity) > limit {
                    self.disconnect("Connection timed out", false, out);
                    return;
                }
                if now.duration_since(self.last_ping) >= cfg.ping_interval {
                    self.send_ping(now, out);
                }
                self.mtu_heartbeat(now, cfg, out);
            }
            Status::AwaitingConnect | Status::Dead => {}
        }

        out.push_acks(&mut self.queued_acks);

        while let Some((kind, seq)) = self.incoming_acks.pop_front() {
            if !is_user_type(kind) {
                continue;
            }
            if let Some(channel) = self.send_channels[(kind - 1) as usize].as_mut() {
                channel.receive_ack(now, seq, &mut self.retransmits, &mut |message, seq| {
                    out.push_outgoing(message, seq)
                });
            }
        }

        for slot in (0..NUM_CHANNELS).rev() {
            if let Some(channel) = self.send_channels[slot].as_mut() {
                channel.pump(now, &mut self.retransmits, &mut |message, seq| {
                    out.push_outgoing(message, seq)
                });
            }
        }
        out.flush();
    }

    pub fn handle_datagram(
        &mut self,
        now: Instant,
        cfg: &TransportConfig,
        unique_id: u64,
        datagram: &[u8],
        out: &mut FrameOut,
    ) {
        if self.status == Status::Dead {
            return;
        }
        if self.status == Status::Connected {
            self.last_activity = now;
            self.timeout_grace = false;
        }
        let mut cursor = Cursor::new(datagram);
        while let Some(message) = cursor.next() {
            if message.kind == KIND_UNCONNECTED {
                continue;
            }
            if (UNUSED_KIND_FIRST..=UNUSED_KIND_LAST).contains(&message.kind) {
                return;
            }
            if is_library_type(message.kind) {
                self.handle_library_message(now, cfg, unique_id, message, out);
            } else {
                self.handle_user_message(now, cfg, message, out);
            }
            if self.status == Status::Dead {
                return;
            }
        }
    }

    fn handle_user_message(
        &mut self,
        now: Instant,
        cfg: &TransportConfig,
        message: MessageRef,
        out: &mut FrameOut,
    ) {
        if self.status != Status::Connected {
            return;
        }
        let kind = message.kind;
        let frag = message.frag;
        let slot = (kind - 1) as usize;
        let channel = self
            .recv_channels
            .get_mut(slot)
            .expect("receiver slot in range")
            .get_or_insert_with(|| ReceiverChannel::new(kind, true));
        let incoming = Incoming {
            seq: message.seq,
            payload: message.payload.to_vec(),
        };
        let outcome = channel.receive(incoming, &mut |kind, seq| {
            self.queued_acks.push_back((kind, seq))
        });
        if let Outcome::Release(batch) = outcome {
            for payload in batch {
                self.release_payload(now, cfg, frag, payload, out);
                if self.status == Status::Dead {
                    return;
                }
            }
        }
    }

    fn handle_library_message(
        &mut self,
        now: Instant,
        cfg: &TransportConfig,
        unique_id: u64,
        message: MessageRef,
        out: &mut FrameOut,
    ) {
        match message.kind {
            KIND_CONNECT => {
                if matches!(self.status, Status::AwaitingConnect | Status::Responded) {
                    if !self.validate_handshake(cfg, message.payload, out) {
                        return;
                    }
                    self.send_connect_response(now, cfg, unique_id, out);
                }
            }
            KIND_CONNECT_RESPONSE => match self.status {
                Status::Connecting => {
                    if !self.validate_handshake(cfg, message.payload, out) {
                        return;
                    }
                    let payload = established_payload(self.local_time());
                    out.push_standalone(KIND_CONNECTION_ESTABLISHED, &payload);
                    self.become_connected(now, cfg, out);
                }
                Status::Connected => {
                    let payload = established_payload(self.local_time());
                    out.push_standalone(KIND_CONNECTION_ESTABLISHED, &payload);
                }
                _ => {}
            },
            KIND_CONNECTION_ESTABLISHED => {
                if self.status == Status::Responded && message.payload.len() >= 4 {
                    let remote_time = f32::from_le_bytes(
                        message.payload[..4].try_into().expect("four byte payload"),
                    );
                    self.init_remote_time_offset(remote_time);
                    self.become_connected(now, cfg, out);
                }
            }
            KIND_DISCONNECT => {
                let reason = parse_disconnect(message.payload);
                self.disconnect(&reason, false, out);
            }
            KIND_PING => {
                if self.status == Status::Connected
                    && let Some(&number) = message.payload.first()
                {
                    let payload = pong_payload(number, self.local_time());
                    out.push_standalone(KIND_PONG, &payload);
                }
            }
            KIND_PONG => {
                if self.status == Status::Connected
                    && let Some((number, remote_time)) = parse_pong(message.payload)
                {
                    self.receive_pong(now, cfg, number, remote_time);
                }
            }
            KIND_ACKNOWLEDGE => {
                if self.status == Status::Connected
                    && !message.payload.is_empty()
                    && message.payload.len().is_multiple_of(3)
                {
                    for (kind, seq) in parse_ack_payload(message.payload) {
                        if is_user_type(kind) {
                            self.incoming_acks.push_back((kind, seq));
                        } else {
                            break;
                        }
                    }
                }
            }
            KIND_EXPAND_MTU_REQUEST if self.status == Status::Connected => {
                let size = message.payload.len();
                if size > 0 && size <= MAX_PROBE_MTU {
                    let payload = (size as u32).to_le_bytes();
                    out.push_standalone(KIND_EXPAND_MTU_SUCCESS, &payload);
                }
            }
            KIND_EXPAND_MTU_SUCCESS
                if self.status == Status::Connected && message.payload.len() == 4 =>
            {
                let size =
                    u32::from_le_bytes(message.payload.try_into().expect("four byte payload"))
                        as usize;
                self.mtu_success(now, cfg, size, out);
            }
            _ => {}
        }
    }

    fn release_payload(
        &mut self,
        now: Instant,
        cfg: &TransportConfig,
        frag: bool,
        payload: Vec<u8>,
        out: &mut FrameOut,
    ) {
        let assembled = if frag {
            match self.accept_fragment(now, cfg, payload) {
                Some(data) => data,
                None => return,
            }
        } else {
            payload
        };
        let decrypted = self
            .crypto
            .as_ref()
            .and_then(|session| session.decrypt(&assembled));
        let plain = match (self.crypto.is_some(), decrypted) {
            (true, Some(plain)) => plain,
            (true, None) => {
                self.disconnect("Decryption failed", false, out);
                return;
            }
            (false, _) => assembled,
        };
        self.released.push(plain);
    }

    fn accept_fragment(
        &mut self,
        now: Instant,
        cfg: &TransportConfig,
        payload: Vec<u8>,
    ) -> Option<Vec<u8>> {
        let (header, header_len) = read_fragment_header(&payload)?;
        let total_bytes = bytes_to_hold_bits(header.total_bits);
        let payload_len = payload.len() - header_len;
        if header.group == 0
            || header.total_bits == 0
            || header.chunk_size == 0
            || header.chunk_size > MAX_CHUNK_BYTES
            || total_bytes == 0
            || total_bytes > MAX_GROUP_BYTES
            || payload_len == 0
            || payload_len > header.chunk_size
        {
            return None;
        }
        let num_chunks = total_bytes.div_ceil(header.chunk_size);
        let chunk_number = header.chunk_number as usize;
        if chunk_number >= num_chunks
            || chunk_number * header.chunk_size + payload_len > total_bytes
        {
            return None;
        }
        self.expire_fragment_groups(now, cfg);
        match self.fragment_groups.get(&header.group) {
            Some(group) => {
                if group.total_bytes != total_bytes
                    || group.total_bits != header.total_bits
                    || group.chunk_size != header.chunk_size
                    || group.num_chunks != num_chunks
                {
                    return None;
                }
            }
            None => {
                if self.fragment_groups.len() >= MAX_CONCURRENT_GROUPS {
                    return None;
                }
                let queued: usize = self.fragment_groups.values().map(|g| g.total_bytes).sum();
                if queued + total_bytes > cfg.reassembly_bytes {
                    return None;
                }
            }
        }
        let group = self.fragment_groups.entry(header.group).or_insert_with(|| {
            Reassembly::new(
                total_bytes,
                header.total_bits,
                header.chunk_size,
                num_chunks,
            )
        });
        if !group.mark(chunk_number) {
            return None;
        }
        group.last_received = now;
        let offset = chunk_number * header.chunk_size;
        group.data[offset..offset + payload_len].copy_from_slice(&payload[header_len..]);
        if group.count == group.num_chunks {
            let done = self.fragment_groups.remove(&header.group)?;
            Some(done.data)
        } else {
            None
        }
    }

    fn expire_fragment_groups(&mut self, now: Instant, cfg: &TransportConfig) {
        if self.fragment_groups.is_empty() {
            return;
        }
        let stale: Vec<u32> = self
            .fragment_groups
            .iter()
            .filter(|(_, group)| now.duration_since(group.last_received) > cfg.fragment_timeout)
            .map(|(key, _)| *key)
            .collect();
        for key in stale {
            self.fragment_groups.remove(&key);
        }
    }

    fn validate_handshake(
        &mut self,
        cfg: &TransportConfig,
        payload: &[u8],
        out: &mut FrameOut,
    ) -> bool {
        match parse_handshake(payload) {
            Some((app_identifier, unique_id, local_time, _)) => {
                if app_identifier != cfg.app_identifier {
                    let reason = "Wrong application identifier!";
                    self.disconnect(reason, true, out);
                    return false;
                }
                self.remote_unique_id = unique_id;
                self.init_remote_time_offset(local_time);
                true
            }
            None => {
                let reason = "Handshake data validation failed";
                self.disconnect(reason, true, out);
                false
            }
        }
    }

    fn send_connect_response(
        &mut self,
        now: Instant,
        cfg: &TransportConfig,
        unique_id: u64,
        out: &mut FrameOut,
    ) {
        self.handshake_attempts += 1;
        self.handshake_sent = now;
        self.status = Status::Responded;
        let payload = handshake_payload(&cfg.app_identifier, unique_id, self.local_time(), &[]);
        out.push_standalone(KIND_CONNECT_RESPONSE, &payload);
    }

    fn become_connected(&mut self, now: Instant, cfg: &TransportConfig, out: &mut FrameOut) {
        if self.status == Status::Connected {
            return;
        }
        self.status = Status::Connected;
        self.connected_pending = true;
        self.last_activity = now;
        self.timeout_grace = true;
        self.mtu.start(cfg.mtu.clamp(MIN_MTU, MAX_MTU));
        self.send_ping(now, out);
    }

    fn mtu_heartbeat(&mut self, now: Instant, cfg: &TransportConfig, out: &mut FrameOut) {
        if self.mtu.status == MtuStatus::Finished {
            return;
        }
        if self.mtu.status == MtuStatus::Idle {
            if !cfg.expand_mtu {
                self.mtu_finalize(self.mtu.current);
                return;
            }
            self.mtu.status = MtuStatus::Probing;
            self.mtu_expand(now, cfg, out);
            return;
        }
        let Some(last_sent_at) = self.mtu.last_sent_at else {
            return;
        };
        if now > last_sent_at + cfg.expand_mtu_frequency {
            self.mtu.fails += 1;
            if self.mtu.fails >= cfg.expand_mtu_fail_attempts {
                self.mtu_finalize(self.mtu.current);
                return;
            }
            self.mtu.smallest_failed = Some(self.mtu.last_sent_size);
            self.mtu_expand(now, cfg, out);
        }
    }

    fn mtu_expand(&mut self, now: Instant, _cfg: &TransportConfig, out: &mut FrameOut) {
        let try_mtu = match self.mtu.smallest_failed {
            None => (self.mtu.current as f32 * 1.25) as usize,
            Some(failed) => (failed + self.mtu.largest_successful) / 2,
        }
        .min(MAX_PROBE_MTU);
        if try_mtu == self.mtu.largest_successful {
            self.mtu_finalize(self.mtu.largest_successful);
            return;
        }
        let probe = vec![0u8; try_mtu];
        out.push_standalone(KIND_EXPAND_MTU_REQUEST, &probe);
        self.mtu.last_sent_size = try_mtu;
        self.mtu.last_sent_at = Some(now);
    }

    fn mtu_finalize(&mut self, size: usize) {
        if self.mtu.status == MtuStatus::Finished {
            return;
        }
        self.mtu.status = MtuStatus::Finished;
        self.mtu.current = size;
    }

    fn mtu_success(
        &mut self,
        now: Instant,
        cfg: &TransportConfig,
        size: usize,
        out: &mut FrameOut,
    ) {
        if self.mtu.status != MtuStatus::Probing {
            return;
        }
        if size == 0 || size > MAX_PROBE_MTU {
            return;
        }
        if size != self.mtu.last_sent_size {
            return;
        }
        if size > self.mtu.largest_successful {
            self.mtu.largest_successful = size;
            self.mtu.discovered = true;
        }
        if size < self.mtu.current {
            return;
        }
        self.mtu.current = size;
        self.mtu_expand(now, cfg, out);
    }

    fn send_ping(&mut self, now: Instant, out: &mut FrameOut) {
        self.ping_number = self.ping_number.wrapping_add(1);
        self.ping_count += 1;
        self.ping_sent_secs = self.now_secs();
        self.last_ping = now;
        let payload = ping_payload(self.ping_number);
        out.push_standalone(KIND_PING, &payload);
    }

    fn receive_pong(&mut self, now: Instant, cfg: &TransportConfig, number: u8, remote_time: f32) {
        if number != self.ping_number {
            return;
        }
        self.last_activity = now;
        self.timeout_grace = false;
        let now_secs = self.now_secs();
        let rtt = (now_secs - self.ping_sent_secs).max(0.0);
        let diff = remote_time as f64 + rtt / 2.0 - now_secs;
        if self.avg_rtt < 0.0 {
            self.avg_rtt = rtt;
            self.remote_time_offset = diff;
        } else {
            self.avg_rtt = self.avg_rtt * 0.7 + rtt * 0.3;
            let weight = self.ping_count.max(1) as f64;
            self.remote_time_offset = (self.remote_time_offset * (weight - 1.0) + diff) / weight;
        }
        if cfg.resend_delay.is_none() {
            let delay = Duration::from_secs_f64(0.025 + self.avg_rtt.max(0.0) * 2.1);
            for slot in 0..NUM_CHANNELS {
                if let Some(channel) = self.send_channels[slot].as_mut() {
                    channel.set_resend_delay(delay);
                }
            }
        }
    }

    fn init_remote_time_offset(&mut self, remote_time: f32) {
        let now_secs = self.now_secs();
        self.remote_time_offset = remote_time as f64 + self.avg_rtt / 2.0 - now_secs;
    }

    fn now_secs(&self) -> f64 {
        self.clock_start.elapsed().as_secs_f64()
    }

    fn local_time(&self) -> f32 {
        self.now_secs() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{
        Cursor, KIND_ACKNOWLEDGE, KIND_EXPAND_MTU_REQUEST, KIND_EXPAND_MTU_SUCCESS,
        KIND_RELIABLE_ORDERED_BASE, parse_ack_payload, write_header,
    };

    fn library_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; HEADER_BYTES];
        write_header(&mut frame, kind, 0, false, (payload.len() * 8) as u16);
        frame.extend_from_slice(payload);
        frame
    }

    fn expand_ready_protocol(now: Instant) -> (Protocol, TransportConfig) {
        let cfg = TransportConfig {
            expand_mtu: true,
            ..TransportConfig::default()
        };
        let mut protocol = Protocol::new(true, now);
        let mut out = FrameOut::new(700);
        protocol.become_connected(now, &cfg, &mut out);
        out.take();
        (protocol, cfg)
    }

    fn probe_size(datagrams: &[Vec<u8>]) -> Option<usize> {
        datagrams
            .iter()
            .find(|datagram| datagram[0] == KIND_EXPAND_MTU_REQUEST)
            .map(|probe| probe.len() - HEADER_BYTES)
    }

    #[test]
    fn acks_pack_into_lidgren_acknowledge_messages() {
        let mut acks: VecDeque<(u8, u16)> = (0..5).map(|i| (67u8, i as u16)).collect();
        let mut out = FrameOut::new(700);
        out.push_acks(&mut acks);
        let datagrams = out.take();
        assert_eq!(datagrams.len(), 1);
        let datagram = &datagrams[0];
        assert_eq!(datagram[0], KIND_ACKNOWLEDGE);
        let bit_len = u16::from_le_bytes([datagram[3], datagram[4]]);
        assert_eq!(bit_len, 5 * 3 * 8);
        let parsed = parse_ack_payload(&datagram[HEADER_BYTES..]);
        assert_eq!(parsed, (0..5).map(|i| (67u8, i as u16)).collect::<Vec<_>>());
        assert!(acks.is_empty());
    }

    #[test]
    fn acks_split_when_more_than_one_datagram() {
        let mut acks: VecDeque<(u8, u16)> = (0..400).map(|i| (67u8, i as u16)).collect();
        let mut out = FrameOut::new(700);
        out.push_acks(&mut acks);
        let datagrams = out.take();
        assert!(datagrams.len() >= 2);
        for datagram in &datagrams {
            assert!(datagram.len() <= 700);
            assert_eq!(datagram[0], KIND_ACKNOWLEDGE);
        }
        let total: usize = datagrams
            .iter()
            .map(|datagram| parse_ack_payload(&datagram[HEADER_BYTES..]).len())
            .sum();
        assert_eq!(total, 400);
        assert!(acks.is_empty());
    }

    #[test]
    fn messages_coalesce_into_one_datagram_up_to_mtu() {
        let mut out = FrameOut::new(64);
        for i in 0..8 {
            let message = Outgoing::whole(KIND_RELIABLE_ORDERED_BASE, Rc::new(vec![i; 10]));
            out.push_outgoing(&message, i as u16);
        }
        let datagrams = out.take();
        assert_eq!(datagrams.len(), 2);
        for datagram in &datagrams {
            assert!(datagram.len() <= 64);
        }
        let mut cursor = Cursor::new(&datagrams[0]);
        let mut seen = 0;
        while let Some(message) = cursor.next() {
            seen += 1;
            assert_eq!(message.payload.len(), 10);
        }
        assert_eq!(seen, 4);
    }

    #[test]
    fn mtu_probes_grow_by_a_quarter_and_honor_successes() {
        let now = Instant::now();
        let (mut protocol, cfg) = expand_ready_protocol(now);
        let mut out = FrameOut::new(700);
        protocol.tick(now, &cfg, 1, &mut out);
        assert_eq!(probe_size(&out.take()), Some(875));

        let mut out = FrameOut::new(700);
        protocol.handle_datagram(
            now,
            &cfg,
            1,
            &library_frame(KIND_EXPAND_MTU_SUCCESS, &875u32.to_le_bytes()),
            &mut out,
        );
        assert_eq!(protocol.current_mtu(&cfg), 875);
        assert_eq!(probe_size(&out.take()), Some(1093));

        let mut out = FrameOut::new(700);
        protocol.handle_datagram(
            now,
            &cfg,
            1,
            &library_frame(KIND_EXPAND_MTU_SUCCESS, &1093u32.to_le_bytes()),
            &mut out,
        );
        assert_eq!(protocol.current_mtu(&cfg), 1093);
        assert_eq!(probe_size(&out.take()), Some(1366));
    }

    #[test]
    fn mtu_success_with_unexpected_size_is_ignored() {
        let now = Instant::now();
        let (mut protocol, cfg) = expand_ready_protocol(now);
        let mut out = FrameOut::new(700);
        protocol.tick(now, &cfg, 1, &mut out);
        out.take();

        let mut out = FrameOut::new(700);
        protocol.handle_datagram(
            now,
            &cfg,
            1,
            &library_frame(KIND_EXPAND_MTU_SUCCESS, &1093u32.to_le_bytes()),
            &mut out,
        );
        assert_eq!(protocol.current_mtu(&cfg), 700);
        assert_eq!(probe_size(&out.take()), None);
    }

    #[test]
    fn mtu_probe_timeout_switches_to_binary_search() {
        let start = Instant::now();
        let (mut protocol, cfg) = expand_ready_protocol(start);
        let mut out = FrameOut::new(700);
        protocol.tick(start, &cfg, 1, &mut out);
        assert_eq!(probe_size(&out.take()), Some(875));

        let later = start + Duration::from_secs(3);
        let mut out = FrameOut::new(700);
        protocol.tick(later, &cfg, 1, &mut out);
        assert_eq!(probe_size(&out.take()), Some(787));
        assert_eq!(protocol.current_mtu(&cfg), 700);
    }

    #[test]
    fn mtu_expansion_finalizes_after_fail_attempts() {
        let start = Instant::now();
        let (mut protocol, cfg) = expand_ready_protocol(start);
        let mut out = FrameOut::new(700);
        protocol.tick(start, &cfg, 1, &mut out);
        out.take();

        let mut now = start;
        for _ in 0..cfg.expand_mtu_fail_attempts {
            now += cfg.expand_mtu_frequency + Duration::from_millis(1);
            let mut out = FrameOut::new(700);
            protocol.tick(now, &cfg, 1, &mut out);
            out.take();
        }
        assert_eq!(protocol.current_mtu(&cfg), 700);

        now += cfg.expand_mtu_frequency + Duration::from_millis(1);
        let mut out = FrameOut::new(700);
        protocol.tick(now, &cfg, 1, &mut out);
        assert_eq!(probe_size(&out.take()), None);
    }

    #[test]
    fn mtu_expansion_climbs_to_protocol_max() {
        let start = Instant::now();
        let (mut protocol, cfg) = expand_ready_protocol(start);
        let mut now = start;
        let mut out = FrameOut::new(700);
        protocol.tick(now, &cfg, 1, &mut out);
        let Some(mut size) = probe_size(&out.take()) else {
            panic!("first probe missing");
        };
        for _ in 0..64 {
            let mut out = FrameOut::new(700);
            protocol.handle_datagram(
                now,
                &cfg,
                1,
                &library_frame(KIND_EXPAND_MTU_SUCCESS, &(size as u32).to_le_bytes()),
                &mut out,
            );
            let Some(next) = probe_size(&out.take()) else {
                break;
            };
            size = next;
            now += Duration::from_millis(10);
        }
        assert_eq!(protocol.current_mtu(&cfg), MAX_PROBE_MTU);
    }

    #[test]
    fn mtu_stays_at_floor_when_expansion_is_disabled() {
        let now = Instant::now();
        let cfg = TransportConfig::default();
        let mut protocol = Protocol::new(true, now);
        let mut out = FrameOut::new(700);
        protocol.become_connected(now, &cfg, &mut out);
        out.take();

        let mut out = FrameOut::new(700);
        protocol.tick(now, &cfg, 1, &mut out);
        assert_eq!(probe_size(&out.take()), None);
        assert_eq!(protocol.current_mtu(&cfg), 700);

        protocol.reset_mtu_floor(1200);
        assert_eq!(protocol.current_mtu(&cfg), 1200);
    }

    #[test]
    fn mtu_request_is_answered_with_success() {
        let now = Instant::now();
        let (mut protocol, cfg) = expand_ready_protocol(now);
        let mut out = FrameOut::new(700);
        protocol.handle_datagram(
            now,
            &cfg,
            1,
            &library_frame(KIND_EXPAND_MTU_REQUEST, &[0u8; 1200]),
            &mut out,
        );
        let datagrams = out.take();
        assert_eq!(datagrams.len(), 1);
        assert_eq!(datagrams[0][0], KIND_EXPAND_MTU_SUCCESS);
        assert_eq!(&datagrams[0][HEADER_BYTES..], &1200u32.to_le_bytes());
    }

    #[test]
    fn oversized_unreliable_message_flies_as_single_datagram() {
        let mut out = FrameOut::new(700);
        let message = Outgoing::whole(KIND_RELIABLE_ORDERED_BASE, Rc::new(vec![0u8; 5000]));
        out.push_outgoing(&message, 3);
        let datagrams = out.take();
        assert_eq!(datagrams.len(), 1);
        assert_eq!(datagrams[0].len(), HEADER_BYTES + 5000);
    }
}
