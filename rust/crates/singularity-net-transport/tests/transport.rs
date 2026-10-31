use std::thread::sleep;
use std::time::{Duration, Instant};

use singularity_net_transport::ffi::*;
use singularity_net_transport::{
    ConnectionId, Delivery, MAX_DATAGRAM, TransportEvent, UdpTransport,
};

struct Pair {
    server: UdpTransport,
    client: UdpTransport,
    server_events: Vec<(TransportEvent, Vec<u8>)>,
    client_events: Vec<(TransportEvent, Vec<u8>)>,
    server_conn: Option<ConnectionId>,
    client_conn: Option<ConnectionId>,
    client_established: bool,
}

fn extract_connected(events: &mut Vec<(TransportEvent, Vec<u8>)>) -> Option<ConnectionId> {
    let mut found = None;
    events.retain(|(event, _)| match event {
        TransportEvent::Connected(id) => {
            found = Some(*id);
            false
        }
        _ => true,
    });
    found
}

impl Pair {
    fn new() -> Pair {
        let server = UdpTransport::bind("127.0.0.1:0").unwrap();
        let client = UdpTransport::bind("127.0.0.1:0").unwrap();
        Pair {
            server,
            client,
            server_events: Vec::new(),
            client_events: Vec::new(),
            server_conn: None,
            client_conn: None,
            client_established: false,
        }
    }

    fn server_addr(&self) -> std::net::SocketAddr {
        self.server.local_addr().unwrap()
    }

    fn step(&mut self) {
        self.server.poll(&mut self.server_events);
        self.client.poll(&mut self.client_events);
        if let Some(id) = extract_connected(&mut self.server_events) {
            self.server_conn = Some(id);
        }
        if extract_connected(&mut self.client_events).is_some() {
            self.client_established = true;
        }
    }

    fn connect(&mut self) {
        let addr = self.server_addr();
        let id = self.client.connect(addr);
        self.client_conn = Some(id);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(self.server_conn.is_some() && self.client_established) && Instant::now() < deadline
        {
            self.step();
            sleep(Duration::from_millis(1));
        }
    }

    fn established(&self) -> bool {
        self.server_conn.is_some() && self.client_established
    }
}

#[test]
fn handshake_completes_on_both_sides() {
    let mut pair = Pair::new();
    pair.connect();
    assert!(pair.established(), "handshake did not complete");
    assert_eq!(pair.server.stats().connections, 1);
    assert_eq!(pair.client.stats().connections, 1);
}

#[test]
fn unreliable_echo_roundtrip() {
    let mut pair = Pair::new();
    pair.connect();
    let conn = pair.client_conn.unwrap();
    pair.client.send(conn, Delivery::Unreliable, b"ping");

    let mut received = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while received.is_none() && Instant::now() < deadline {
        pair.step();
        for (event, payload) in pair.server_events.drain(..) {
            if let TransportEvent::Received(id) = event {
                pair.server.send(id, Delivery::Unreliable, &payload);
            }
        }
        for (event, payload) in pair.client_events.drain(..) {
            if let TransportEvent::Received(_) = event {
                received = Some(payload);
            }
        }
    }

    assert_eq!(received.as_deref(), Some(b"ping".as_slice()));
}

#[test]
fn reliable_ordered_survives_packet_loss() {
    let mut pair = Pair::new();
    pair.server.set_timings(
        Duration::from_millis(20),
        Duration::from_millis(2000),
        Duration::from_millis(15),
    );
    pair.client.set_timings(
        Duration::from_millis(20),
        Duration::from_millis(2000),
        Duration::from_millis(15),
    );
    pair.server.set_loss_rate(0.3);
    pair.client.set_loss_rate(0.3);
    pair.connect();
    assert!(pair.established());
    let conn = pair.client_conn.unwrap();

    let count = 40u32;
    for i in 0..count {
        pair.client
            .send(conn, Delivery::ReliableOrdered, &i.to_le_bytes());
    }

    let mut seen: Vec<u32> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while seen.len() < count as usize && Instant::now() < deadline {
        pair.step();
        for (event, payload) in pair.server_events.drain(..) {
            if let TransportEvent::Received(_) = event
                && payload.len() == 4
            {
                seen.push(u32::from_le_bytes(payload.try_into().unwrap()));
            }
        }
    }

    assert_eq!(seen.len(), count as usize, "lost reliable messages");
    let ordered: Vec<u32> = (0..count).collect();
    assert_eq!(seen, ordered, "reliable order broken");
    assert!(pair.client.stats().retransmits > 0, "loss never injected");
}

#[test]
fn close_notifies_peer() {
    let mut pair = Pair::new();
    pair.connect();
    assert!(pair.established());
    let conn = pair.client_conn.unwrap();
    pair.client.close(conn);

    let mut disconnected = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !disconnected && Instant::now() < deadline {
        pair.step();
        disconnected = pair
            .server_events
            .drain(..)
            .any(|(event, _)| matches!(event, TransportEvent::Disconnected(_)));
    }

    assert!(disconnected, "server never saw disconnect");
    assert_eq!(pair.server.stats().connections, 0);
}

#[test]
fn silence_times_out() {
    let mut pair = Pair::new();
    pair.server.set_timings(
        Duration::from_millis(10),
        Duration::from_millis(150),
        Duration::from_millis(15),
    );
    pair.client.set_timings(
        Duration::from_millis(10),
        Duration::from_millis(150),
        Duration::from_millis(15),
    );
    pair.connect();
    assert!(pair.established());

    let mut timed_out = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !timed_out && Instant::now() < deadline {
        pair.server.poll(&mut pair.server_events);
        timed_out = pair
            .server_events
            .drain(..)
            .any(|(event, _)| matches!(event, TransportEvent::Disconnected(_)));
        sleep(Duration::from_millis(5));
    }

    assert!(timed_out, "timeout never fired");
}

#[test]
fn payload_bounds() {
    let mut pair = Pair::new();
    pair.connect();
    let conn = pair.client_conn.unwrap();

    let oversized = vec![7u8; MAX_DATAGRAM + 1];
    assert!(!pair.client.send(conn, Delivery::Unreliable, &oversized));

    let exact = vec![7u8; MAX_DATAGRAM];
    assert!(pair.client.send(conn, Delivery::Unreliable, &exact));

    let mut delivered = 0usize;
    let deadline = Instant::now() + Duration::from_secs(5);
    while delivered == 0 && Instant::now() < deadline {
        pair.step();
        delivered = pair
            .server_events
            .drain(..)
            .filter(|(event, payload)| {
                matches!(event, TransportEvent::Received(_)) && payload.len() == MAX_DATAGRAM
            })
            .count();
    }
    assert_eq!(delivered, 1, "mtu-sized payload missing");
}

#[test]
fn many_clients_all_echo() {
    let clients = 8;
    let mut server = UdpTransport::bind("127.0.0.1:0").unwrap();
    let server_addr = server.local_addr().unwrap();
    let mut client_transports: Vec<UdpTransport> = Vec::new();
    let mut ids: Vec<Option<ConnectionId>> = vec![None; clients];
    for _ in 0..clients {
        let mut client = UdpTransport::bind("127.0.0.1:0").unwrap();
        client.connect(server_addr);
        client_transports.push(client);
    }

    let mut scratch = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while (server.stats().connections < clients as u32 || ids.iter().any(Option::is_none))
        && Instant::now() < deadline
    {
        server.poll(&mut scratch);
        scratch.clear();
        for (i, client) in client_transports.iter_mut().enumerate() {
            client.poll(&mut scratch);
            if ids[i].is_none() {
                ids[i] = extract_connected(&mut scratch);
            }
            scratch.clear();
        }
        sleep(Duration::from_millis(1));
    }
    assert!(ids.iter().all(Option::is_some), "not all clients connected");
    assert_eq!(server.stats().connections, clients as u32);

    for (i, client) in client_transports.iter_mut().enumerate() {
        client.send(
            ids[i].unwrap(),
            Delivery::Unreliable,
            &(i as u32).to_le_bytes(),
        );
    }

    let mut echoed = 0usize;
    let deadline = Instant::now() + Duration::from_secs(5);
    while echoed < clients && Instant::now() < deadline {
        server.poll(&mut scratch);
        for (event, payload) in scratch.drain(..) {
            if let TransportEvent::Received(id) = event {
                server.send(id, Delivery::Unreliable, &payload);
                echoed += 1;
            }
        }
        for client in &mut client_transports {
            client.poll(&mut scratch);
            scratch.clear();
        }
        sleep(Duration::from_millis(1));
    }
    assert_eq!(echoed, clients);
}

#[test]
fn ffi_surface_end_to_end() {
    unsafe {
        assert_eq!(
            sg_transport_abi_version(),
            singularity_net_transport::ABI_VERSION
        );

        let server = sg_transport_bind(c"127.0.0.1:0".as_ptr().cast());
        assert!(!server.is_null());
        let port = sg_transport_local_port(server);
        assert_ne!(port, 0);
        let server_addr = format!("127.0.0.1:{port}\0");

        let client = sg_transport_bind(c"127.0.0.1:0".as_ptr().cast());
        assert!(!client.is_null());
        let conn = sg_transport_connect(client, server_addr.as_ptr().cast());
        assert_ne!(conn, 0);

        let mut events: Vec<SgNetEvent> = (0..16).map(|_| SgNetEvent::default()).collect();
        let mut client_connected = false;
        let mut server_connected = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while (!client_connected || !server_connected) && Instant::now() < deadline {
            let n = sg_transport_poll(client, events.as_mut_ptr(), events.len());
            for event in &events[..n] {
                if event.kind == EVENT_CONNECTED {
                    client_connected = true;
                }
            }
            let n = sg_transport_poll(server, events.as_mut_ptr(), events.len());
            for event in &events[..n] {
                if event.kind == EVENT_CONNECTED {
                    server_connected = true;
                }
            }
            sleep(Duration::from_millis(1));
        }
        assert!(client_connected && server_connected);

        let client_port = sg_transport_local_port(client);
        let mut endpoint = [0i8; 64];
        assert_eq!(
            sg_transport_remote_endpoint(server, conn, endpoint.as_mut_ptr(), endpoint.len()),
            1
        );
        let end = endpoint.iter().position(|&c| c == 0).unwrap();
        let text: String = endpoint[..end].iter().map(|&c| c as u8 as char).collect();
        assert_eq!(text, format!("127.0.0.1:{client_port}"));

        assert_eq!(
            sg_transport_send(client, conn, DELIVERY_UNRELIABLE, b"hello ffi".as_ptr(), 9),
            1
        );

        let mut got = 0usize;
        let deadline = Instant::now() + Duration::from_secs(5);
        while got == 0 && Instant::now() < deadline {
            let n = sg_transport_poll(server, events.as_mut_ptr(), events.len());
            for event in &events[..n] {
                if event.kind == EVENT_RECEIVED && event.length == 9 {
                    assert_eq!(&event.data[..9], b"hello ffi");
                    got += 1;
                }
            }
            sleep(Duration::from_millis(1));
        }
        assert_eq!(got, 1);

        let mut stats = SgNetStats::default();
        assert_eq!(sg_transport_stats(server, &mut stats), 1);
        assert!(stats.datagrams_received > 0);
        assert_eq!(stats.connections, 1);

        sg_transport_close(client, conn);
        sg_transport_destroy(client);
        sg_transport_destroy(server);
    }
}
