use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::wire::{
    FragmentHeader, KIND_RELIABLE_ORDERED_BASE, KIND_RELIABLE_SEQUENCED_BASE,
    KIND_RELIABLE_UNORDERED, KIND_SEQUENCED_BASE, KIND_UNRELIABLE, RELIABLE_WINDOW, SEQ_MASK,
    SEQ_SPACE, UNRELIABLE_WINDOW, relative_seq,
};

pub const DEFAULT_RESEND_DELAY: Duration = Duration::from_millis(235);

pub struct Outgoing {
    pub kind: u8,
    pub data: Rc<Vec<u8>>,
    pub offset: usize,
    pub len: usize,
    pub frag: Option<FragmentHeader>,
}

impl Outgoing {
    pub fn whole(kind: u8, data: Rc<Vec<u8>>) -> Outgoing {
        let len = data.len();
        Outgoing {
            kind,
            data,
            offset: 0,
            len,
            frag: None,
        }
    }

    pub fn chunk(kind: u8, data: Rc<Vec<u8>>, meta: FragmentHeader) -> Outgoing {
        let start = meta.chunk_number as usize * meta.chunk_size;
        let end = (start + meta.chunk_size).min(data.len());
        Outgoing {
            kind,
            data,
            offset: start,
            len: end - start,
            frag: Some(meta),
        }
    }
}

pub fn window_for(kind: u8) -> usize {
    if is_reliable(kind) {
        RELIABLE_WINDOW
    } else {
        UNRELIABLE_WINDOW
    }
}

pub fn is_reliable(kind: u8) -> bool {
    kind >= KIND_RELIABLE_UNORDERED
}

pub fn is_sequenced(kind: u8) -> bool {
    (KIND_SEQUENCED_BASE..KIND_RELIABLE_UNORDERED).contains(&kind)
        || (KIND_RELIABLE_SEQUENCED_BASE..KIND_RELIABLE_ORDERED_BASE).contains(&kind)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SendResult {
    Sent,
    Queued,
    Dropped,
    NotConnected,
}

pub(crate) struct BitWindow {
    bits: Vec<u64>,
}

impl BitWindow {
    fn new(size: usize) -> BitWindow {
        BitWindow {
            bits: vec![0u64; size.div_ceil(64)],
        }
    }

    fn get(&self, index: usize) -> bool {
        (self.bits[index / 64] >> (index % 64)) & 1 == 1
    }

    fn set(&mut self, index: usize, value: bool) {
        if value {
            self.bits[index / 64] |= 1 << (index % 64);
        } else {
            self.bits[index / 64] &= !(1 << (index % 64));
        }
    }
}

pub struct StoredMessage {
    pub message: Outgoing,
    pub seq: u16,
    pub num_sent: u32,
    pub last_sent: Instant,
}

pub enum Sender {
    Unreliable {
        window_start: u16,
        send_start: u16,
    },
    Reliable {
        window_start: u16,
        send_start: u16,
        resend_delay: Duration,
        stored: Vec<Option<StoredMessage>>,
        acked: BitWindow,
    },
}

pub struct SenderChannel {
    pub window: usize,
    pub queued: VecDeque<Outgoing>,
    pub state: Sender,
}

impl SenderChannel {
    pub fn new(kind: u8) -> SenderChannel {
        let window = window_for(kind);
        let state = if is_reliable(kind) {
            Sender::Reliable {
                window_start: 0,
                send_start: 0,
                resend_delay: DEFAULT_RESEND_DELAY,
                stored: (0..window).map(|_| None).collect(),
                acked: BitWindow::new(SEQ_SPACE),
            }
        } else {
            Sender::Unreliable {
                window_start: 0,
                send_start: 0,
            }
        };
        SenderChannel {
            window,
            queued: VecDeque::new(),
            state,
        }
    }

    pub fn set_resend_delay(&mut self, delay: Duration) {
        if let Sender::Reliable { resend_delay, .. } = &mut self.state {
            *resend_delay = delay;
        }
    }

    pub fn allowed_sends(&self) -> usize {
        let (window_start, send_start) = match &self.state {
            Sender::Unreliable {
                window_start,
                send_start,
            } => (window_start, send_start),
            Sender::Reliable {
                window_start,
                send_start,
                ..
            } => (window_start, send_start),
        };
        self.window - ((*send_start as usize + SEQ_SPACE) - *window_start as usize) % SEQ_SPACE
    }

    pub fn enqueue(&mut self, message: Outgoing) -> SendResult {
        match &self.state {
            Sender::Unreliable { .. } => {
                if self.queued.len() + 1 > self.allowed_sends() {
                    return SendResult::Dropped;
                }
                self.queued.push_back(message);
                SendResult::Sent
            }
            Sender::Reliable { .. } => {
                self.queued.push_back(message);
                if self.queued.len() <= self.allowed_sends() {
                    SendResult::Sent
                } else {
                    SendResult::Queued
                }
            }
        }
    }

    pub fn pump(
        &mut self,
        now: Instant,
        retransmits: &mut u64,
        send: &mut impl FnMut(&Outgoing, u16),
    ) {
        match &mut self.state {
            Sender::Unreliable { send_start, .. } => {
                while let Some(message) = self.queued.pop_front() {
                    let seq = *send_start;
                    *send_start = (*send_start + 1) & SEQ_MASK;
                    send(&message, seq);
                }
            }
            Sender::Reliable {
                window_start,
                send_start,
                resend_delay,
                stored,
                ..
            } => {
                for slot in stored.iter_mut() {
                    let Some(entry) = slot else { continue };
                    if now.duration_since(entry.last_sent) >= *resend_delay {
                        *retransmits += 1;
                        entry.last_sent = now;
                        entry.num_sent += 1;
                        send(&entry.message, entry.seq);
                    }
                }

                let mut budget = {
                    let window_start = *window_start as usize;
                    let send_start = *send_start as usize;
                    self.window - ((send_start + SEQ_SPACE) - window_start) % SEQ_SPACE
                };
                while budget > 0 {
                    let Some(message) = self.queued.pop_front() else {
                        break;
                    };
                    let seq = *send_start;
                    *send_start = (*send_start + 1) & SEQ_MASK;
                    send(&message, seq);
                    stored[seq as usize % self.window] = Some(StoredMessage {
                        message,
                        seq,
                        num_sent: 1,
                        last_sent: now,
                    });
                    budget -= 1;
                }
            }
        }
    }

    pub fn receive_ack(
        &mut self,
        now: Instant,
        seq: u16,
        retransmits: &mut u64,
        send: &mut impl FnMut(&Outgoing, u16),
    ) {
        match &mut self.state {
            Sender::Unreliable { window_start, .. } => {
                let relate = relative_seq(seq, *window_start);
                if relate < 0 {
                    return;
                }
                if relate == 0 {
                    *window_start = (*window_start + 1) & SEQ_MASK;
                    return;
                }
                while *window_start != seq {
                    *window_start = (*window_start + 1) & SEQ_MASK;
                }
            }
            Sender::Reliable {
                window_start,
                send_start,
                resend_delay,
                stored,
                acked,
            } => {
                let relate = relative_seq(seq, *window_start);
                if relate < 0 {
                    return;
                }
                if relate == 0 {
                    stored[*window_start as usize % self.window] = None;
                    acked.set(*window_start as usize, false);
                    *window_start = (*window_start + 1) & SEQ_MASK;
                    while acked.get(*window_start as usize) {
                        acked.set(*window_start as usize, false);
                        stored[*window_start as usize % self.window] = None;
                        *window_start = (*window_start + 1) & SEQ_MASK;
                    }
                    return;
                }

                let send_relate = relative_seq(seq, *send_start);
                if send_relate > 0 {
                    return;
                }
                if !acked.get(seq as usize) {
                    acked.set(seq as usize, true);
                }

                let mut rnr = seq;
                loop {
                    rnr = if rnr == 0 { SEQ_MASK } else { rnr - 1 };
                    if !acked.get(rnr as usize) {
                        let entry = &mut stored[rnr as usize % self.window];
                        if let Some(entry) = entry
                            && entry.num_sent == 1
                            && now.duration_since(entry.last_sent) >= resend_delay.mul_f64(0.35)
                        {
                            entry.last_sent = now;
                            entry.num_sent += 1;
                            *retransmits += 1;
                            send(&entry.message, entry.seq);
                        }
                    }
                    if rnr == *window_start {
                        break;
                    }
                }
            }
        }
    }
}

pub struct Incoming {
    pub seq: u16,
    pub payload: Vec<u8>,
}

pub enum Outcome {
    Release(Vec<Vec<u8>>),
    Withheld,
    Dropped,
}

pub enum Receiver {
    UnreliableUnordered,
    UnreliableSequenced {
        last: i32,
    },
    ReliableUnordered {
        window_start: u16,
        early: BitWindow,
    },
    ReliableOrdered {
        window_start: u16,
        early: BitWindow,
        withheld: Vec<Option<Incoming>>,
    },
    ReliableSequenced {
        window_start: u16,
    },
}

pub struct ReceiverChannel {
    pub kind: u8,
    pub window: usize,
    pub acks: bool,
    pub state: Receiver,
}

impl ReceiverChannel {
    pub fn new(kind: u8, unreliable_acks: bool) -> ReceiverChannel {
        let window = window_for(kind);
        let reliable = is_reliable(kind);
        let state = if reliable {
            if kind == KIND_RELIABLE_UNORDERED {
                Receiver::ReliableUnordered {
                    window_start: 0,
                    early: BitWindow::new(window),
                }
            } else if (KIND_RELIABLE_SEQUENCED_BASE..KIND_RELIABLE_ORDERED_BASE).contains(&kind) {
                Receiver::ReliableSequenced { window_start: 0 }
            } else {
                Receiver::ReliableOrdered {
                    window_start: 0,
                    early: BitWindow::new(window),
                    withheld: (0..window).map(|_| None).collect(),
                }
            }
        } else if is_sequenced(kind) {
            Receiver::UnreliableSequenced { last: -1 }
        } else {
            Receiver::UnreliableUnordered
        };
        let acks = reliable || is_sequenced(kind) || (kind == KIND_UNRELIABLE && unreliable_acks);
        ReceiverChannel {
            kind,
            window,
            acks,
            state,
        }
    }

    pub fn receive(&mut self, incoming: Incoming, queue_ack: &mut impl FnMut(u8, u16)) -> Outcome {
        let seq = incoming.seq;
        if self.acks {
            queue_ack(self.kind, seq);
        }
        match &mut self.state {
            Receiver::UnreliableUnordered => Outcome::Release(vec![incoming.payload]),
            Receiver::UnreliableSequenced { last } => {
                let expected = (*last + 1) & SEQ_MASK as i32;
                if relative_seq(seq, expected as u16) < 0 {
                    return Outcome::Dropped;
                }
                *last = seq as i32;
                Outcome::Release(vec![incoming.payload])
            }
            Receiver::ReliableUnordered {
                window_start,
                early,
            } => {
                let relate = relative_seq(seq, *window_start);
                if relate == 0 {
                    early.set(*window_start as usize % self.window, false);
                    *window_start = (*window_start + 1) & SEQ_MASK;
                    return Outcome::Release(vec![incoming.payload]);
                }
                if relate < 0 || relate > self.window as i32 {
                    return Outcome::Dropped;
                }
                if early.get(seq as usize % self.window) {
                    return Outcome::Dropped;
                }
                early.set(seq as usize % self.window, true);
                Outcome::Release(vec![incoming.payload])
            }
            Receiver::ReliableOrdered {
                window_start,
                early,
                withheld,
            } => {
                let relate = relative_seq(seq, *window_start);
                if relate == 0 {
                    early.set(*window_start as usize % self.window, false);
                    *window_start = (*window_start + 1) & SEQ_MASK;
                    let mut released = vec![incoming.payload];
                    let mut next = (seq + 1) & SEQ_MASK;
                    while early.get(next as usize % self.window) {
                        early.set(next as usize % self.window, false);
                        if let Some(held) = withheld[next as usize % self.window].take() {
                            released.push(held.payload);
                        }
                        *window_start = (*window_start + 1) & SEQ_MASK;
                        next = (next + 1) & SEQ_MASK;
                    }
                    Outcome::Release(released)
                } else if relate < 0 || relate > self.window as i32 {
                    Outcome::Dropped
                } else {
                    early.set(seq as usize % self.window, true);
                    withheld[seq as usize % self.window] = Some(incoming);
                    Outcome::Withheld
                }
            }
            Receiver::ReliableSequenced { window_start } => {
                let relate = relative_seq(seq, *window_start);
                if relate == 0 {
                    *window_start = (*window_start + 1) & SEQ_MASK;
                    return Outcome::Release(vec![incoming.payload]);
                }
                if relate < 0 || relate > self.window as i32 {
                    return Outcome::Dropped;
                }
                *window_start = (*window_start + relate as u16) & SEQ_MASK;
                Outcome::Release(vec![incoming.payload])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(kind: u8, data: &[u8]) -> Outgoing {
        Outgoing::whole(kind, Rc::new(data.to_vec()))
    }

    #[test]
    fn reliable_window_opens_and_closes() {
        let mut channel = SenderChannel::new(KIND_RELIABLE_ORDERED_BASE);
        assert_eq!(channel.allowed_sends(), RELIABLE_WINDOW);
        for i in 0..RELIABLE_WINDOW + 8 {
            let result = channel.enqueue(message(KIND_RELIABLE_ORDERED_BASE, &[i as u8]));
            assert_ne!(result, SendResult::Dropped);
        }
        assert_eq!(channel.queued.len(), RELIABLE_WINDOW + 8);
        let mut sent = 0;
        channel.pump(Instant::now(), &mut 0, &mut |_, _| sent += 1);
        assert_eq!(sent, RELIABLE_WINDOW);
        assert_eq!(channel.queued.len(), 8);

        channel.receive_ack(Instant::now(), 0, &mut 0, &mut |_, _| {});
        assert_eq!(channel.allowed_sends(), 1);
        channel.pump(Instant::now(), &mut 0, &mut |_, _| {});
        assert_eq!(channel.allowed_sends(), 0);
        assert_eq!(channel.queued.len(), 7);
        for seq in 1..8 {
            channel.receive_ack(Instant::now(), seq, &mut 0, &mut |_, _| {});
        }
        channel.pump(Instant::now(), &mut 0, &mut |_, _| {});
        assert!(channel.queued.is_empty());
    }

    #[test]
    fn reliable_early_ack_advances_window() {
        let mut channel = SenderChannel::new(KIND_RELIABLE_ORDERED_BASE);
        channel.enqueue(message(KIND_RELIABLE_ORDERED_BASE, b"a"));
        channel.enqueue(message(KIND_RELIABLE_ORDERED_BASE, b"b"));
        channel.pump(Instant::now(), &mut 0, &mut |_, _| {});
        channel.receive_ack(Instant::now(), 1, &mut 0, &mut |_, _| {});
        channel.receive_ack(Instant::now(), 0, &mut 0, &mut |_, _| {});
        assert_eq!(channel.allowed_sends(), RELIABLE_WINDOW);
        match &channel.state {
            Sender::Reliable { stored, .. } => assert!(stored.iter().all(Option::is_none)),
            _ => panic!(),
        }
    }

    #[test]
    fn reliable_ordered_withholds_and_releases_in_order() {
        let mut channel = ReceiverChannel::new(KIND_RELIABLE_ORDERED_BASE, true);
        let mut acks = Vec::new();
        let first = channel.receive(
            Incoming {
                seq: 1,
                payload: vec![2],
            },
            &mut |kind, seq| acks.push((kind, seq)),
        );
        assert!(matches!(first, Outcome::Withheld));
        let second = channel.receive(
            Incoming {
                seq: 0,
                payload: vec![1],
            },
            &mut |_, _| {},
        );
        match second {
            Outcome::Release(batch) => assert_eq!(batch, vec![vec![1], vec![2]]),
            _ => panic!(),
        }
        assert_eq!(acks[0], (KIND_RELIABLE_ORDERED_BASE, 1));
    }

    #[test]
    fn unreliable_sequenced_drops_late() {
        let mut channel = ReceiverChannel::new(KIND_SEQUENCED_BASE, true);
        assert!(matches!(
            channel.receive(
                Incoming {
                    seq: 0,
                    payload: vec![]
                },
                &mut |_, _| {}
            ),
            Outcome::Release(_)
        ));
        assert!(matches!(
            channel.receive(
                Incoming {
                    seq: 4,
                    payload: vec![]
                },
                &mut |_, _| {}
            ),
            Outcome::Release(_)
        ));
        assert!(matches!(
            channel.receive(
                Incoming {
                    seq: 2,
                    payload: vec![]
                },
                &mut |_, _| {}
            ),
            Outcome::Dropped
        ));
    }

    #[test]
    fn unreliable_unordered_acks_only_when_enabled() {
        let mut quiet = ReceiverChannel::new(KIND_UNRELIABLE, false);
        let mut acks = 0;
        quiet.receive(
            Incoming {
                seq: 0,
                payload: vec![],
            },
            &mut |_, _| acks += 1,
        );
        assert_eq!(acks, 0);

        let mut loud = ReceiverChannel::new(KIND_UNRELIABLE, true);
        loud.receive(
            Incoming {
                seq: 0,
                payload: vec![],
            },
            &mut |_, _| acks += 1,
        );
        assert_eq!(acks, 1);
    }

    #[test]
    fn unreliable_acks_advance_window_like_lidgren() {
        let mut channel = SenderChannel::new(KIND_UNRELIABLE);
        channel.enqueue(message(KIND_UNRELIABLE, b"a"));
        channel.enqueue(message(KIND_UNRELIABLE, b"b"));
        channel.enqueue(message(KIND_UNRELIABLE, b"c"));
        channel.pump(Instant::now(), &mut 0, &mut |_, _| {});
        assert_eq!(channel.allowed_sends(), UNRELIABLE_WINDOW - 3);
        channel.receive_ack(Instant::now(), 2, &mut 0, &mut |_, _| {});
        assert_eq!(channel.allowed_sends(), UNRELIABLE_WINDOW - 1);
        channel.receive_ack(Instant::now(), 2, &mut 0, &mut |_, _| {});
        assert_eq!(channel.allowed_sends(), UNRELIABLE_WINDOW);
        channel.receive_ack(Instant::now(), 0, &mut 0, &mut |_, _| {});
        assert_eq!(channel.allowed_sends(), UNRELIABLE_WINDOW);
    }

    #[test]
    fn unreliable_sequenced_receiver_drops_wraparound_late() {
        let mut channel = ReceiverChannel::new(KIND_SEQUENCED_BASE, true);
        assert!(matches!(
            channel.receive(
                Incoming {
                    seq: SEQ_MASK,
                    payload: vec![]
                },
                &mut |_, _| {}
            ),
            Outcome::Dropped
        ));
    }
}
