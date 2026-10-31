#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, c_char};
use std::net::SocketAddr;

use crate::{ConnectionId, Delivery, MAX_DATAGRAM, TransportEvent, UdpTransport};

pub const EVENT_CONNECTED: u32 = 0;
pub const EVENT_DISCONNECTED: u32 = 1;
pub const EVENT_RECEIVED: u32 = 2;

pub const DELIVERY_UNRELIABLE: u32 = 0;
pub const DELIVERY_RELIABLE_ORDERED: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SgNetEvent {
    pub kind: u32,
    pub connection: u64,
    pub length: u32,
    pub reserved: u32,
    pub data: [u8; MAX_DATAGRAM],
}

impl Default for SgNetEvent {
    fn default() -> Self {
        SgNetEvent {
            kind: u32::MAX,
            connection: 0,
            length: 0,
            reserved: 0,
            data: [0u8; MAX_DATAGRAM],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SgNetStats {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub datagrams_sent: u64,
    pub datagrams_received: u64,
    pub datagrams_dropped: u64,
    pub retransmits: u64,
    pub connections: u32,
    pub reserved: u32,
}

#[unsafe(no_mangle)]
pub extern "C" fn sg_transport_abi_version() -> u32 {
    crate::ABI_VERSION
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_bind(addr: *const c_char) -> *mut UdpTransport {
    if addr.is_null() {
        return std::ptr::null_mut();
    }
    let text = unsafe { CStr::from_ptr(addr) }.to_string_lossy();
    let Ok(parsed) = text.parse::<SocketAddr>() else {
        return std::ptr::null_mut();
    };
    match UdpTransport::bind(parsed) {
        Ok(transport) => Box::into_raw(Box::new(transport)),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_remote_endpoint(
    transport: *const UdpTransport,
    connection: u64,
    out: *mut c_char,
    cap: usize,
) -> u32 {
    if transport.is_null() || out.is_null() || cap == 0 {
        return 0;
    }
    let Some(endpoint) = (unsafe { (*transport).remote_endpoint(ConnectionId(connection)) }) else {
        return 0;
    };
    let text = endpoint.to_string();
    let bytes = text.as_bytes();
    if bytes.len() + 1 > cap {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast(), bytes.len());
        *out.add(bytes.len()) = 0;
    }
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_destroy(transport: *mut UdpTransport) {
    if !transport.is_null() {
        unsafe { drop(Box::from_raw(transport)) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_local_port(transport: *mut UdpTransport) -> u16 {
    if transport.is_null() {
        return 0;
    }
    let transport = unsafe { &*transport };
    transport.local_addr().map(|addr| addr.port()).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_connect(
    transport: *mut UdpTransport,
    addr: *const c_char,
) -> u64 {
    if transport.is_null() || addr.is_null() {
        return 0;
    }
    let text = unsafe { CStr::from_ptr(addr) }.to_string_lossy();
    let Ok(parsed) = text.parse::<SocketAddr>() else {
        return 0;
    };
    unsafe { (*transport).connect(parsed).0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_send(
    transport: *mut UdpTransport,
    connection: u64,
    delivery: u32,
    data: *const u8,
    len: usize,
) -> u32 {
    if transport.is_null() || (data.is_null() && len > 0) {
        return 0;
    }
    let payload = unsafe { std::slice::from_raw_parts(data, len) };
    let method = if delivery == DELIVERY_RELIABLE_ORDERED {
        Delivery::ReliableOrdered
    } else {
        Delivery::Unreliable
    };
    u32::from(unsafe { (*transport).send(ConnectionId(connection), method, payload) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_poll(
    transport: *mut UdpTransport,
    out: *mut SgNetEvent,
    cap: usize,
) -> usize {
    if transport.is_null() || out.is_null() || cap == 0 {
        return 0;
    }
    let transport = unsafe { &mut *transport };
    let mut raw = Vec::new();
    transport.poll(&mut raw);
    let mut written = 0usize;
    for (event, payload) in raw {
        if written >= cap {
            break;
        }
        let (kind, connection) = match event {
            TransportEvent::Connected(id) => (EVENT_CONNECTED, id.0),
            TransportEvent::Disconnected(id) => (EVENT_DISCONNECTED, id.0),
            TransportEvent::Received(id) => (EVENT_RECEIVED, id.0),
        };
        let slot = unsafe { &mut *out.add(written) };
        slot.kind = kind;
        slot.connection = connection;
        let length = payload.len().min(MAX_DATAGRAM);
        slot.length = length as u32;
        slot.reserved = 0;
        slot.data[..length].copy_from_slice(&payload[..length]);
        written += 1;
    }
    written
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_close(transport: *mut UdpTransport, connection: u64) {
    if transport.is_null() {
        return;
    }
    unsafe { (*transport).close(ConnectionId(connection)) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_stats(
    transport: *const UdpTransport,
    out: *mut SgNetStats,
) -> u32 {
    if transport.is_null() || out.is_null() {
        return 0;
    }
    let stats = unsafe { (*transport).stats() };
    unsafe {
        *out = SgNetStats {
            bytes_sent: stats.bytes_sent,
            bytes_received: stats.bytes_received,
            datagrams_sent: stats.datagrams_sent,
            datagrams_received: stats.datagrams_received,
            datagrams_dropped: stats.datagrams_dropped,
            retransmits: stats.retransmits,
            connections: stats.connections,
            reserved: 0,
        };
    }
    1
}
