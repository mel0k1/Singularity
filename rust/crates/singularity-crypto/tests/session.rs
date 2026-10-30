use singularity_crypto::{FRAME_OVERHEAD, Key, Session, Side};

fn key(fill: u8) -> Key {
    [fill; 32]
}

fn payload(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(3))
        .collect()
}

#[test]
fn round_trip_various_sizes() {
    let server = Session::new(&key(1), Side::Server);
    let client = Session::new(&key(1), Side::Client);
    for size in [0usize, 1, 17, 64, 1024] {
        let pt = payload(size);
        let frame = server.encrypt(&pt);
        assert_eq!(frame.len(), pt.len() + FRAME_OVERHEAD);
        assert_eq!(client.decrypt(&frame), Some(pt.clone()));
        let back = client.encrypt(&pt);
        assert_eq!(server.decrypt(&back), Some(pt));
    }
}

#[test]
fn rejects_short_frames() {
    let session = Session::new(&key(2), Side::Server);
    assert_eq!(session.decrypt(&[]), None);
    assert_eq!(session.decrypt(&[0u8; 8]), None);
    assert_eq!(session.decrypt(&[0u8; 23]), None);
    let minimal = session.encrypt(&[]);
    assert_eq!(session.decrypt(&minimal), Some(vec![]));
    assert_eq!(session.decrypt(&minimal[..23]), None);
}

#[test]
fn rejects_tampered_frames() {
    let session = Session::new(&key(3), Side::Server);
    let pt = payload(200);

    let mut frame = session.encrypt(&pt);
    frame[0] ^= 1;
    assert_eq!(session.decrypt(&frame), None);

    let mut frame = session.encrypt(&pt);
    let last = frame.len() - 1;
    frame[last] ^= 1;
    assert_eq!(session.decrypt(&frame), None);

    let mut frame = session.encrypt(&pt);
    frame[12] ^= 1;
    assert_eq!(session.decrypt(&frame), None);
}

#[test]
fn rejects_wrong_key() {
    let alice = Session::new(&key(4), Side::Server);
    let bob = Session::new(&key(5), Side::Server);
    let frame = alice.encrypt(&payload(50));
    assert_eq!(bob.decrypt(&frame), None);
}

#[test]
fn streams_do_not_reuse_nonces() {
    let a = Session::new(&key(6), Side::Server);
    let pt = payload(300);
    let first = a.encrypt(&pt);
    let second = a.encrypt(&pt);
    assert_ne!(first, second);
    let fresh = Session::new(&key(6), Side::Server);
    assert_eq!(fresh.encrypt(&pt), first);
}

#[test]
fn parity_never_collides() {
    let server = Session::new(&key(7), Side::Server);
    let client = Session::new(&key(7), Side::Client);
    let pt = payload(64);
    let mut frames: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        frames.push(server.encrypt(&pt));
        frames.push(client.encrypt(&pt));
    }
    for (i, x) in frames.iter().enumerate() {
        for (j, y) in frames.iter().enumerate() {
            if i != j {
                assert_ne!(x, y);
            }
        }
    }
    for frame in &frames {
        assert_eq!(server.decrypt(frame).as_deref(), Some(&pt[..]));
    }
}
