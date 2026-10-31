use std::thread::sleep;
use std::time::{Duration, Instant};

use singularity_net_transport::{ConnectionId, Delivery, TransportEvent, UdpTransport};

fn main() {
    let mut server = UdpTransport::bind("127.0.0.1:0").unwrap();
    let server_addr = server.local_addr().unwrap();
    let mut client = UdpTransport::bind("127.0.0.1:0").unwrap();
    let conn = client.connect(server_addr);

    let mut server_events = Vec::new();
    let mut client_events = Vec::new();
    wait_established(
        &mut server,
        &mut client,
        conn,
        &mut server_events,
        &mut client_events,
    );

    for payload_len in [64usize, 512, 1200] {
        bench_unreliable(
            &mut server,
            &mut client,
            conn,
            payload_len,
            &mut server_events,
        );
        bench_reliable(
            &mut server,
            &mut client,
            conn,
            payload_len,
            &mut server_events,
        );
    }

    bench_send_call_overhead(&mut client, conn);
    let stats = client.stats();
    println!(
        "client stats: sent {} recv {} retransmits {}",
        stats.datagrams_sent, stats.datagrams_received, stats.retransmits
    );
}

fn wait_established(
    server: &mut UdpTransport,
    client: &mut UdpTransport,
    conn: ConnectionId,
    server_events: &mut Vec<(TransportEvent, Vec<u8>)>,
    client_events: &mut Vec<(TransportEvent, Vec<u8>)>,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        server.poll(server_events);
        client.poll(client_events);
        let client_ready = client_events
            .iter()
            .any(|(event, _)| matches!(event, TransportEvent::Connected(_)));
        let server_ready = server_events
            .iter()
            .any(|(event, _)| matches!(event, TransportEvent::Connected(_)));
        if (client_ready && server_ready) || Instant::now() > deadline {
            break;
        }
        sleep(Duration::from_millis(1));
    }
    let _ = conn;
}

fn bench_unreliable(
    server: &mut UdpTransport,
    client: &mut UdpTransport,
    conn: ConnectionId,
    payload_len: usize,
    server_events: &mut Vec<(TransportEvent, Vec<u8>)>,
) {
    let batches = 200;
    let batch = 32;
    let payload = vec![0x5au8; payload_len];
    let start = Instant::now();
    let mut sent = 0usize;
    let mut received = 0usize;
    for _ in 0..batches {
        for _ in 0..batch {
            client.send(conn, Delivery::Unreliable, &payload);
            sent += 1;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut got = 0;
        while got < batch && Instant::now() < deadline {
            server.poll(server_events);
            for (event, data) in server_events.drain(..) {
                if let TransportEvent::Received(_) = event
                    && data.len() == payload_len
                {
                    got += 1;
                }
            }
            client.poll(&mut Vec::new());
            sleep(Duration::from_micros(50));
        }
        received += got;
    }
    let elapsed = start.elapsed();
    println!(
        "unreliable {}B: {} msgs, {} delivered, {:.0} msgs/s, {:.1} MB/s",
        payload_len,
        sent,
        received,
        sent as f64 / elapsed.as_secs_f64(),
        (sent * payload_len) as f64 / elapsed.as_secs_f64() / 1e6
    );
}

fn bench_reliable(
    server: &mut UdpTransport,
    client: &mut UdpTransport,
    conn: ConnectionId,
    payload_len: usize,
    server_events: &mut Vec<(TransportEvent, Vec<u8>)>,
) {
    let count = 400;
    let payload = vec![0xa5u8; payload_len];
    let start = Instant::now();
    for _ in 0..count {
        client.send(conn, Delivery::ReliableOrdered, &payload);
    }

    let mut received = 0usize;
    let deadline = Instant::now() + Duration::from_secs(30);
    while received < count && Instant::now() < deadline {
        server.poll(server_events);
        for (event, data) in server_events.drain(..) {
            if let TransportEvent::Received(_) = event
                && data.len() == payload_len
            {
                received += 1;
            }
        }
        client.poll(&mut Vec::new());
        sleep(Duration::from_micros(50));
    }
    let elapsed = start.elapsed();
    println!(
        "reliable   {}B: {} msgs, {} delivered, {:.0} msgs/s (window 64)",
        payload_len,
        count,
        received,
        count as f64 / elapsed.as_secs_f64()
    );
}

fn bench_send_call_overhead(client: &mut UdpTransport, conn: ConnectionId) {
    let payload = [0u8; 32];
    let iterations = 200_000;
    let start = Instant::now();
    let mut accepted = 0usize;
    for _ in 0..iterations {
        if client.send(conn, Delivery::Unreliable, &payload) {
            accepted += 1;
        }
    }
    let elapsed = start.elapsed();
    println!(
        "send() enqueue: {} accepted of {}, {:.0} ns/call",
        accepted,
        iterations,
        elapsed.as_nanos() as f64 / iterations as f64
    );
}
