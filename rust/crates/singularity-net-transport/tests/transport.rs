use std::thread::sleep;
use std::time::{Duration, Instant};

use singularity_net_transport::ffi::*;
use singularity_net_transport::{
    ConnectionId, Delivery, MAX_GROUP_BYTES, MAX_UNRELIABLE_BYTES, TransportEvent, UdpTransport,
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
fn ipv6_wildcard_binds_v6_only() {
    let v6 = UdpTransport::bind("[::1]:0").unwrap();
    assert!(v6.local_addr().unwrap().is_ipv6());

    let wildcard = UdpTransport::bind("[::]:0").unwrap();
    assert!(wildcard.local_addr().unwrap().is_ipv6());

    let v4 = UdpTransport::bind("0.0.0.0:0").unwrap();
    assert!(v4.local_addr().unwrap().is_ipv4());

    let mut pair = Pair::new();
    pair.connect();
    assert!(pair.established());
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
fn fragmented_reliable_roundtrip() {
    let mut pair = Pair::new();
    pair.connect();
    assert!(pair.established());
    let conn = pair.client_conn.unwrap();

    let mut payload = vec![0u8; 16 * 1024];
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    assert!(pair.client.send(conn, Delivery::ReliableOrdered, &payload));

    let mut received: Option<Vec<u8>> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while received.is_none() && Instant::now() < deadline {
        pair.step();
        for (event, data) in pair.server_events.drain(..) {
            if let TransportEvent::Received(_) = event
                && data.len() == payload.len()
            {
                received = Some(data);
            }
        }
    }

    assert_eq!(
        received.as_ref(),
        Some(&payload),
        "fragmented payload broken"
    );
    assert!(
        pair.client.stats().datagrams_sent > 20,
        "expected many fragment datagrams"
    );
}

#[test]
fn close_notifies_peer_and_carries_reason() {
    let mut pair = Pair::new();
    pair.connect();
    assert!(pair.established());
    let conn = pair.client_conn.unwrap();
    pair.client.disconnect(conn, "local bye");

    let mut disconnected = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while disconnected.is_none() && Instant::now() < deadline {
        pair.step();
        for (event, _) in pair.server_events.drain(..) {
            if let TransportEvent::Disconnected(id, _) = event {
                disconnected = Some(id);
            }
        }
    }

    let id = disconnected.expect("server never saw disconnect");
    assert_eq!(
        pair.server.disconnect_reason(id),
        Some("local bye"),
        "reason did not cross the wire"
    );
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

    let mut timed_out = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while timed_out.is_none() && Instant::now() < deadline {
        pair.server.poll(&mut pair.server_events);
        for (event, _) in pair.server_events.drain(..) {
            if let TransportEvent::Disconnected(id, _) = event {
                timed_out = Some(id);
            }
        }
        sleep(Duration::from_millis(5));
    }

    let id = timed_out.expect("timeout never fired");
    assert_eq!(
        pair.server.disconnect_reason(id),
        Some("Connection timed out")
    );
}

#[test]
fn payload_bounds() {
    let mut pair = Pair::new();
    pair.connect();
    let conn = pair.client_conn.unwrap();

    let oversized = vec![7u8; MAX_UNRELIABLE_BYTES + 1];
    assert!(!pair.client.send(conn, Delivery::Unreliable, &oversized));

    let exact = vec![7u8; MAX_UNRELIABLE_BYTES];
    assert!(pair.client.send(conn, Delivery::Unreliable, &exact));

    let beyond_group = vec![7u8; MAX_GROUP_BYTES + 1];
    assert!(
        !pair
            .client
            .send(conn, Delivery::ReliableOrdered, &beyond_group)
    );

    let fragmented = vec![9u8; 1200];
    assert!(
        pair.client
            .send(conn, Delivery::ReliableOrdered, &fragmented)
    );

    let mut delivered = 0usize;
    let deadline = Instant::now() + Duration::from_secs(10);
    while delivered < 2 && Instant::now() < deadline {
        pair.step();
        delivered += pair
            .server_events
            .drain(..)
            .filter(|(event, payload)| {
                matches!(event, TransportEvent::Received(_))
                    && (payload.len() == MAX_UNRELIABLE_BYTES || payload.len() == 1200)
            })
            .count();
    }
    assert_eq!(delivered, 2, "boundary payloads missing");
}

#[test]
fn rtt_measured_after_pings() {
    let mut pair = Pair::new();
    pair.server.set_timings(
        Duration::from_millis(20),
        Duration::from_millis(2000),
        Duration::from_millis(0),
    );
    pair.client.set_timings(
        Duration::from_millis(20),
        Duration::from_millis(2000),
        Duration::from_millis(0),
    );
    pair.connect();
    assert!(pair.established());
    let conn = pair.client_conn.unwrap();

    let server_conn = pair.server_conn.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while (pair.client.rtt(conn) < 0.0 || pair.server.rtt(server_conn) < 0.0)
        && Instant::now() < deadline
    {
        pair.step();
    }
    assert!(pair.client.rtt(conn) >= 0.0, "no pong came back");
    assert!(
        pair.server.rtt(server_conn) >= 0.0,
        "server rtt stayed unknown"
    );
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
        assert_eq!(sg_transport_set_timings(client, 20, 2000, 30), 1);
        assert_eq!(sg_transport_set_timings(server, 20, 2000, 30), 1);
        let conn = sg_transport_connect(client, server_addr.as_ptr().cast());
        assert_ne!(conn, 0);

        let mut events: Vec<SgNetEvent> = (0..16).map(|_| SgNetEvent::default()).collect();
        let mut client_connected = false;
        let mut server_connected = 0u64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while (server_connected == 0 || !client_connected) && Instant::now() < deadline {
            let n = sg_transport_poll(client, events.as_mut_ptr(), events.len());
            for event in &events[..n] {
                if event.kind == EVENT_CONNECTED {
                    client_connected = true;
                }
            }
            let n = sg_transport_poll(server, events.as_mut_ptr(), events.len());
            for event in &events[..n] {
                if event.kind == EVENT_CONNECTED {
                    server_connected = event.connection;
                }
            }
            sleep(Duration::from_millis(1));
        }
        assert!(client_connected && server_connected != 0);

        let client_port = sg_transport_local_port(client);
        let mut endpoint = [0i8; 64];
        assert_eq!(
            sg_transport_remote_endpoint(server, conn, endpoint.as_mut_ptr(), endpoint.len()),
            1
        );
        let end = endpoint.iter().position(|&c| c == 0).unwrap();
        let text: String = endpoint[..end].iter().map(|&c| c as u8 as char).collect();
        assert_eq!(text, format!("127.0.0.1:{client_port}"));

        let key: [u8; 32] = core::array::from_fn(|i| (i * 7 + 1) as u8);
        assert_eq!(
            sg_transport_set_key(client, conn, key.as_ptr(), key.len()),
            1
        );
        assert_eq!(
            sg_transport_set_key(server, server_connected, key.as_ptr(), key.len()),
            1
        );

        assert_eq!(
            sg_transport_send(client, conn, 67, b"hello ffi".as_ptr(), 9),
            1
        );

        let mut got = 0usize;
        let deadline = Instant::now() + Duration::from_secs(5);
        while got == 0 && Instant::now() < deadline {
            sg_transport_poll(client, events.as_mut_ptr(), events.len());
            let n = sg_transport_poll(server, events.as_mut_ptr(), events.len());
            for event in &events[..n] {
                if event.kind == EVENT_RECEIVED && event.length == 9 {
                    assert_eq!(&event.data[..9], b"hello ffi");
                    got += 1;
                }
            }
            sleep(Duration::from_millis(1));
        }
        assert_eq!(got, 1, "encrypted ffi payload never arrived");

        let big: Vec<u8> = (0..4096u32).map(|i| i as u8).collect();
        assert_eq!(
            sg_transport_send(server, server_connected, 67, big.as_ptr(), big.len()),
            1
        );

        let mut overflow = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while overflow.is_none() && Instant::now() < deadline {
            sg_transport_poll(server, events.as_mut_ptr(), events.len());
            let n = sg_transport_poll(client, events.as_mut_ptr(), events.len());
            for event in &events[..n] {
                if event.kind == EVENT_RECEIVED && event.length as usize == big.len() {
                    assert_eq!(event.reserved, EVENT_OVERFLOW);
                    overflow = Some(event.length as usize);
                }
            }
            sleep(Duration::from_millis(1));
        }
        let needed = overflow.expect("fragmented overflow event missing");
        let mut small = [0u8; 16];
        assert_eq!(
            sg_transport_fetch(client, small.as_mut_ptr(), small.len()),
            needed
        );
        let mut fetched = vec![0u8; needed];
        assert_eq!(
            sg_transport_fetch(client, fetched.as_mut_ptr(), fetched.len()),
            needed
        );
        assert_eq!(fetched, big, "overflow fetch payload mismatch");

        let mut rtt_seen = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !rtt_seen && Instant::now() < deadline {
            sg_transport_poll(client, events.as_mut_ptr(), events.len());
            rtt_seen = sg_transport_rtt(client, conn) >= 0.0;
            sleep(Duration::from_millis(2));
        }
        assert!(rtt_seen, "no rtt through the ffi");
        assert!(sg_transport_time_offset(client, conn).is_finite());

        let mut stats = SgNetStats::default();
        assert_eq!(sg_transport_stats(server, &mut stats), 1);
        assert!(stats.datagrams_received > 0);
        assert_eq!(stats.connections, 1);

        let reason = c"ffi bye";
        sg_transport_disconnect(client, conn, reason.as_ptr().cast(), 1);

        let mut saw_disconnect = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !saw_disconnect && Instant::now() < deadline {
            let n = sg_transport_poll(server, events.as_mut_ptr(), events.len());
            for event in &events[..n] {
                if event.kind == EVENT_DISCONNECTED {
                    saw_disconnect = true;
                }
            }
            sleep(Duration::from_millis(1));
        }
        assert!(saw_disconnect, "server never saw the ffi disconnect");

        let mut reason_buf = [0i8; 64];
        assert_eq!(
            sg_transport_disconnect_reason(
                server,
                server_connected,
                reason_buf.as_mut_ptr(),
                reason_buf.len()
            ),
            1
        );
        let end = reason_buf.iter().position(|&c| c == 0).unwrap();
        let text: String = reason_buf[..end].iter().map(|&c| c as u8 as char).collect();
        assert_eq!(text, "ffi bye");

        sg_transport_destroy(client);
        sg_transport_destroy(server);
    }
}

#[test]
fn reliable_burst_far_past_window_stays_alive() {
    let mut pair = Pair::new();
    pair.connect();
    assert!(pair.established());
    let conn = pair.client_conn.unwrap();

    let count = 300u32;
    let payload = vec![0x77u8; 500];
    for i in 0..count {
        let mut message = payload.clone();
        message[0..4].copy_from_slice(&i.to_le_bytes());
        assert!(
            pair.client.send(conn, Delivery::ReliableOrdered, &message),
            "send {} rejected",
            i
        );
    }

    let mut seen: Vec<u32> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while seen.len() < count as usize && Instant::now() < deadline {
        pair.step();
        for (event, data) in pair.server_events.drain(..) {
            match event {
                TransportEvent::Received(_) if data.len() == 500 => {
                    seen.push(u32::from_le_bytes(data[0..4].try_into().unwrap()));
                }
                TransportEvent::Disconnected(..) => {
                    panic!("connection died mid-burst after {} messages", seen.len());
                }
                _ => {}
            }
        }
        for (event, _) in pair.client_events.drain(..) {
            if let TransportEvent::Disconnected(..) = event {
                panic!(
                    "client connection died mid-burst after {} messages",
                    seen.len()
                );
            }
        }
    }

    assert_eq!(seen.len(), count as usize, "burst lost messages");
    let ordered: Vec<u32> = (0..count).collect();
    assert_eq!(seen, ordered, "burst order broken");
    assert!(pair.server.stats().connections == 1);
}
