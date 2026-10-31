#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, c_char};
use std::net::SocketAddr;

use crate::{ConnectionId, MAX_EVENT_INLINE, TransportEvent, UdpTransport};

pub const EVENT_CONNECTED: u32 = 0;
pub const EVENT_DISCONNECTED: u32 = 1;
pub const EVENT_RECEIVED: u32 = 2;

pub const EVENT_INLINE: u32 = 0;
pub const EVENT_OVERFLOW: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SgNetEvent {
    pub kind: u32,
    pub connection: u64,
    pub length: u32,
    pub reserved: u32,
    pub data: [u8; MAX_EVENT_INLINE],
}

impl Default for SgNetEvent {
    fn default() -> Self {
        SgNetEvent {
            kind: u32::MAX,
            connection: 0,
            length: 0,
            reserved: 0,
            data: [0u8; MAX_EVENT_INLINE],
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
pub unsafe extern "C" fn sg_transport_destroy(transport: *mut UdpTransport) {
    if !transport.is_null() {
        unsafe { drop(Box::from_raw(transport)) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_local_port(transport: *const UdpTransport) -> u16 {
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
pub unsafe extern "C" fn sg_transport_remote_identifier(
    transport: *const UdpTransport,
    connection: u64,
) -> i64 {
    if transport.is_null() {
        return 0;
    }
    unsafe {
        (*transport)
            .remote_identifier(ConnectionId(connection))
            .unwrap_or(0)
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
    let kind = delivery as u8;
    if !(1..=98).contains(&kind) {
        return 0;
    }
    let payload = unsafe { std::slice::from_raw_parts(data, len) };
    u32::from(unsafe { (*transport).send_kind(ConnectionId(connection), kind, payload) })
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
        let slot = unsafe { &mut *out.add(written) };
        slot.reserved = 0;
        match event {
            TransportEvent::Connected(id) => {
                slot.kind = EVENT_CONNECTED;
                slot.connection = id.0;
                slot.length = 0;
            }
            TransportEvent::Disconnected(id, reason) => {
                slot.kind = EVENT_DISCONNECTED;
                slot.connection = id.0;
                let bytes = reason.as_bytes();
                let length = bytes.len().min(MAX_EVENT_INLINE);
                slot.length = length as u32;
                slot.data[..length].copy_from_slice(&bytes[..length]);
            }
            TransportEvent::Received(id) => {
                slot.kind = EVENT_RECEIVED;
                slot.connection = id.0;
                slot.length = payload.len() as u32;
                if payload.len() <= MAX_EVENT_INLINE {
                    slot.data[..payload.len()].copy_from_slice(&payload);
                } else {
                    slot.reserved = EVENT_OVERFLOW;
                    slot.data.copy_from_slice(&payload[..MAX_EVENT_INLINE]);
                    transport.queue_overflow(payload);
                }
            }
        }
        written += 1;
    }
    written
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_fetch(
    transport: *mut UdpTransport,
    out: *mut u8,
    cap: usize,
) -> usize {
    if transport.is_null() || (out.is_null() && cap > 0) {
        return 0;
    }
    let transport = unsafe { &mut *transport };
    if cap == 0 {
        return 0;
    }
    let buf = unsafe { std::slice::from_raw_parts_mut(out, cap) };
    transport.fetch_overflow(buf)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_close(transport: *mut UdpTransport, connection: u64) {
    if transport.is_null() {
        return;
    }
    unsafe { (*transport).close(ConnectionId(connection)) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_disconnect(
    transport: *mut UdpTransport,
    connection: u64,
    reason: *const c_char,
    send_bye: u32,
) {
    if transport.is_null() {
        return;
    }
    let text = if reason.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(reason) }
            .to_string_lossy()
            .into_owned()
    };
    let reason = if text.is_empty() {
        "Disconnected".to_owned()
    } else {
        text
    };
    unsafe { (*transport).disconnect_with(ConnectionId(connection), &reason, send_bye != 0) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_disconnect_reason(
    transport: *const UdpTransport,
    connection: u64,
    out: *mut c_char,
    cap: usize,
) -> u32 {
    if transport.is_null() || out.is_null() || cap == 0 {
        return 0;
    }
    let transport = unsafe { &*transport };
    let Some(reason) = transport.disconnect_reason(ConnectionId(connection)) else {
        return 0;
    };
    let bytes = reason.as_bytes();
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
pub unsafe extern "C" fn sg_transport_set_app_identifier(
    transport: *mut UdpTransport,
    app_identifier: *const c_char,
) -> u32 {
    if transport.is_null() || app_identifier.is_null() {
        return 0;
    }
    let text = unsafe { CStr::from_ptr(app_identifier) }
        .to_string_lossy()
        .into_owned();
    unsafe { (*transport).set_app_identifier(&text) };
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_set_mtu(transport: *mut UdpTransport, mtu: u32) -> u32 {
    if transport.is_null() {
        return 0;
    }
    unsafe { (*transport).set_mtu(mtu as usize) };
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_mtu(transport: *const UdpTransport) -> u32 {
    if transport.is_null() {
        return 0;
    }
    unsafe { (*transport).mtu() as u32 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_set_timings(
    transport: *mut UdpTransport,
    ping_interval_ms: u32,
    timeout_ms: u32,
    resend_delay_ms: u32,
) -> u32 {
    if transport.is_null() {
        return 0;
    }
    let ping = std::time::Duration::from_millis(u64::from(ping_interval_ms.max(1)));
    let timeout = std::time::Duration::from_millis(u64::from(timeout_ms.max(1)));
    let resend = std::time::Duration::from_millis(u64::from(resend_delay_ms));
    unsafe { (*transport).set_timings(ping, timeout, resend) };
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_set_key(
    transport: *mut UdpTransport,
    connection: u64,
    key: *const u8,
    len: usize,
) -> u32 {
    if transport.is_null() || key.is_null() || len != 32 {
        return 0;
    }
    let mut material = [0u8; 32];
    unsafe { std::ptr::copy_nonoverlapping(key, material.as_mut_ptr(), 32) };
    u32::from(unsafe { (*transport).set_key(ConnectionId(connection), &material) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_rtt(transport: *const UdpTransport, connection: u64) -> f32 {
    if transport.is_null() {
        return -1.0;
    }
    unsafe { (*transport).rtt(ConnectionId(connection)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_transport_time_offset(
    transport: *const UdpTransport,
    connection: u64,
) -> f32 {
    if transport.is_null() {
        return 0.0;
    }
    unsafe { (*transport).time_offset(ConnectionId(connection)) }
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
