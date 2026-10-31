pub const SEQ_SPACE: usize = 1024;
pub const SEQ_MASK: u16 = (SEQ_SPACE - 1) as u16;
pub const HEADER_BYTES: usize = 5;

pub const MAX_USER_TYPE: u8 = 98;

pub const UNRELIABLE_WINDOW: usize = 128;
pub const RELIABLE_WINDOW: usize = 64;

pub const MAX_CHUNK_BYTES: usize = 8191;
pub const MAX_GROUP_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CONCURRENT_GROUPS: usize = 64;
pub const MAX_MTU: usize = 8191;

pub const KIND_UNCONNECTED: u8 = 0;

pub const KIND_UNRELIABLE: u8 = 1;
pub const KIND_SEQUENCED_BASE: u8 = 2;
pub const KIND_RELIABLE_UNORDERED: u8 = 34;
pub const KIND_RELIABLE_SEQUENCED_BASE: u8 = 35;
pub const KIND_RELIABLE_ORDERED_BASE: u8 = 67;

pub const KIND_LIBRARY_ERROR: u8 = 128;
pub const KIND_PING: u8 = 129;
pub const KIND_PONG: u8 = 130;
pub const KIND_CONNECT: u8 = 131;
pub const KIND_CONNECT_RESPONSE: u8 = 132;
pub const KIND_CONNECTION_ESTABLISHED: u8 = 133;
pub const KIND_ACKNOWLEDGE: u8 = 134;
pub const KIND_DISCONNECT: u8 = 135;
pub const KIND_EXPAND_MTU_REQUEST: u8 = 140;
pub const KIND_EXPAND_MTU_SUCCESS: u8 = 141;

pub fn is_user_type(kind: u8) -> bool {
    (KIND_UNRELIABLE..=MAX_USER_TYPE).contains(&kind)
}

pub fn is_library_type(kind: u8) -> bool {
    kind >= KIND_LIBRARY_ERROR
}

pub fn relative_seq(nr: u16, expected: u16) -> i32 {
    let nr = nr as i32 & SEQ_MASK as i32;
    let expected = expected as i32 & SEQ_MASK as i32;
    (nr - expected + SEQ_SPACE as i32 + (SEQ_SPACE as i32 / 2)) % SEQ_SPACE as i32
        - (SEQ_SPACE as i32 / 2)
}

pub fn bytes_to_hold_bits(bits: u32) -> usize {
    (bits as usize).div_ceil(8)
}

pub fn write_header(dst: &mut [u8], kind: u8, seq: u16, frag: bool, bit_len: u16) {
    let packed = ((seq & SEQ_MASK) << 1) | (frag as u16);
    dst[0] = kind;
    dst[1] = packed as u8;
    dst[2] = (packed >> 8) as u8;
    dst[3] = bit_len as u8;
    dst[4] = (bit_len >> 8) as u8;
}

pub struct MessageRef<'a> {
    pub kind: u8,
    pub seq: u16,
    pub frag: bool,
    pub payload: &'a [u8],
}

pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Cursor<'a> {
        Cursor { buf, pos: 0 }
    }

    pub fn next(&mut self) -> Option<MessageRef<'a>> {
        if self.buf.len().saturating_sub(self.pos) < HEADER_BYTES {
            return None;
        }
        let kind = self.buf[self.pos];
        let low = self.buf[self.pos + 1];
        let high = self.buf[self.pos + 2];
        let frag = (low & 1) == 1;
        let seq = (((low >> 1) as u16) | ((high as u16) << 7)) & SEQ_MASK;
        let bit_len = u16::from_le_bytes([self.buf[self.pos + 3], self.buf[self.pos + 4]]);
        self.pos += HEADER_BYTES;
        let byte_len = bytes_to_hold_bits(u32::from(bit_len));
        if self.buf.len() - self.pos < byte_len {
            self.pos = self.buf.len();
            return None;
        }
        let payload = &self.buf[self.pos..self.pos + byte_len];
        self.pos += byte_len;
        Some(MessageRef {
            kind,
            seq,
            frag,
            payload,
        })
    }
}

pub fn varuint_len(mut value: u32) -> usize {
    let mut len = 1;
    while value >= 0x80 {
        len += 1;
        value >>= 7;
    }
    len
}

pub fn write_varuint(dst: &mut [u8], mut value: u32) -> usize {
    let mut ptr = 0;
    while value >= 0x80 {
        dst[ptr] = value as u8 | 0x80;
        ptr += 1;
        value >>= 7;
    }
    dst[ptr] = value as u8;
    ptr + 1
}

pub fn read_varuint(src: &[u8], pos: &mut usize) -> Option<u32> {
    let mut result: u32 = 0;
    let mut shift = 0;
    while shift <= 28 {
        if *pos >= src.len() {
            return None;
        }
        let next = src[*pos];
        *pos += 1;
        result |= ((next & 0x7f) as u32) << shift;
        if next & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
    }
    None
}

pub struct FragmentHeader {
    pub group: u32,
    pub total_bits: u32,
    pub chunk_size: usize,
    pub chunk_number: u32,
}

pub fn fragment_header_size(
    group: u32,
    total_bytes: usize,
    chunk_size: usize,
    num_chunks: usize,
) -> usize {
    varuint_len(group)
        + varuint_len((total_bytes * 8) as u32)
        + varuint_len(chunk_size as u32)
        + varuint_len(num_chunks as u32)
}

pub fn write_fragment_header(dst: &mut [u8], header: &FragmentHeader) -> usize {
    let mut ptr = write_varuint(dst, header.group);
    ptr += write_varuint(&mut dst[ptr..], header.total_bits);
    ptr += write_varuint(&mut dst[ptr..], header.chunk_size as u32);
    ptr += write_varuint(&mut dst[ptr..], header.chunk_number);
    ptr
}

pub fn read_fragment_header(src: &[u8]) -> Option<(FragmentHeader, usize)> {
    let mut pos = 0;
    let group = read_varuint(src, &mut pos)?;
    let total_bits = read_varuint(src, &mut pos)?;
    let chunk_size = read_varuint(src, &mut pos)?;
    let chunk_number = read_varuint(src, &mut pos)?;
    Some((
        FragmentHeader {
            group,
            total_bits,
            chunk_size: chunk_size as usize,
            chunk_number,
        },
        pos,
    ))
}

pub fn best_chunk_size(group: u32, total_bytes: usize, mtu: usize) -> usize {
    let approx = (mtu.saturating_sub(HEADER_BYTES + 4)).max(1);
    let est = fragment_header_size(group, total_bytes, approx, (total_bytes / approx).max(1));
    let mut chunk = (mtu.saturating_sub(HEADER_BYTES + est)).max(1);
    loop {
        let mut num_chunks = total_bytes / chunk;
        if num_chunks * chunk < total_bytes {
            num_chunks += 1;
        }
        let header_size = fragment_header_size(group, total_bytes, chunk, num_chunks);
        if chunk + header_size + HEADER_BYTES + 1 < mtu || chunk <= 1 {
            return chunk;
        }
        chunk -= 1;
    }
}

pub fn write_string(dst: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    let mut scratch = [0u8; 5];
    let len = write_varuint(&mut scratch, bytes.len() as u32);
    dst.extend_from_slice(&scratch[..len]);
    dst.extend_from_slice(bytes);
}

pub fn read_string(src: &[u8], pos: &mut usize) -> Option<String> {
    let byte_count = read_varuint(src, pos)? as usize;
    if src.len() < *pos + byte_count {
        return None;
    }
    let value = std::str::from_utf8(&src[*pos..*pos + byte_count])
        .ok()?
        .to_owned();
    *pos += byte_count;
    Some(value)
}

pub fn handshake_payload(
    app_identifier: &str,
    unique_id: u64,
    local_time: f32,
    hail: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(13 + app_identifier.len() + hail.len());
    write_string(&mut payload, app_identifier);
    payload.extend_from_slice(&unique_id.to_le_bytes());
    payload.extend_from_slice(&local_time.to_le_bytes());
    payload.extend_from_slice(hail);
    payload
}

pub fn parse_handshake(payload: &[u8]) -> Option<(String, u64, f32, &[u8])> {
    let mut pos = 0;
    let app_identifier = read_string(payload, &mut pos)?;
    if payload.len() < pos + 12 {
        return None;
    }
    let unique_id = u64::from_le_bytes(payload[pos..pos + 8].try_into().unwrap());
    let local_time = f32::from_le_bytes(payload[pos + 8..pos + 12].try_into().unwrap());
    pos += 12;
    Some((app_identifier, unique_id, local_time, &payload[pos..]))
}

pub fn parse_ack_payload(payload: &[u8]) -> Vec<(u8, u16)> {
    let mut acks = Vec::with_capacity(payload.len() / 3);
    let mut pos = 0;
    while pos + 3 <= payload.len() {
        acks.push((
            payload[pos],
            u16::from_le_bytes([payload[pos + 1], payload[pos + 2]]),
        ));
        pos += 3;
    }
    acks
}

pub fn ping_payload(number: u8) -> Vec<u8> {
    vec![number]
}

pub fn pong_payload(number: u8, remote_time: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    out.push(number);
    out.extend_from_slice(&remote_time.to_le_bytes());
    out
}

pub fn parse_pong(payload: &[u8]) -> Option<(u8, f32)> {
    if payload.len() < 5 {
        return None;
    }
    Some((
        payload[0],
        f32::from_le_bytes(payload[1..5].try_into().unwrap()),
    ))
}

pub fn disconnect_payload(reason: &str) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(&mut out, reason);
    out
}

pub fn parse_disconnect(payload: &[u8]) -> String {
    let mut pos = 0;
    read_string(payload, &mut pos).unwrap_or_default()
}

pub fn established_payload(local_time: f32) -> Vec<u8> {
    local_time.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip_matches_lidgren_layout() {
        let mut buf = [0u8; 5];
        write_header(&mut buf, KIND_RELIABLE_ORDERED_BASE, 155, false, 32);
        assert_eq!(buf, [67, (155 << 1) as u8, 155 >> 7, 32, 0]);
        let mut cursor = Cursor::new(&buf);
        assert!(cursor.next().is_none());
    }

    #[test]
    fn header_datagram_roundtrip() {
        let payload = b"abcd";
        let mut buf = vec![0; HEADER_BYTES + 4];
        write_header(&mut buf, KIND_UNRELIABLE, 3, false, 32);
        buf[5..].copy_from_slice(payload);

        let mut cursor = Cursor::new(&buf);
        let message = cursor.next().unwrap();
        assert_eq!(message.kind, KIND_UNRELIABLE);
        assert_eq!(message.seq, 3);
        assert!(!message.frag);
        assert_eq!(message.payload, payload);
        assert!(cursor.next().is_none());
    }

    #[test]
    fn sequence_wraps_at_space() {
        assert_eq!(relative_seq(0, 1023), 1);
        assert_eq!(relative_seq(1023, 0), -1);
        assert_eq!(relative_seq(5, 5), 0);
        assert_eq!(relative_seq(600, 5), -429);
    }

    #[test]
    fn varuint_roundtrip() {
        let values = [0u32, 1, 127, 128, 255, 8191, 65535, 1 << 20, u32::MAX];
        for value in values {
            let mut buf = [0u8; 5];
            let len = write_varuint(&mut buf, value);
            assert_eq!(len, varuint_len(value));
            let mut pos = 0;
            assert_eq!(read_varuint(&buf[..len], &mut pos), Some(value));
            assert_eq!(pos, len);
        }
    }

    #[test]
    fn fragment_header_roundtrip() {
        let header = FragmentHeader {
            group: 7,
            total_bits: 80000,
            chunk_size: 651,
            chunk_number: 12,
        };
        let mut buf = [0u8; 24];
        let len = write_fragment_header(&mut buf, &header);
        let (parsed, header_len) = read_fragment_header(&buf[..len]).unwrap();
        assert_eq!(header_len, len);
        assert_eq!(parsed.group, 7);
        assert_eq!(parsed.total_bits, 80000);
        assert_eq!(parsed.chunk_size, 651);
        assert_eq!(parsed.chunk_number, 12);
        assert_eq!(len, fragment_header_size(7, 10000, 651, 16));
    }

    #[test]
    fn handshake_roundtrip() {
        let payload = handshake_payload("RobustToolbox", 0x0102030405060708, 1.5, b"hail!");
        let (app, id, time, hail) = parse_handshake(&payload).unwrap();
        assert_eq!(app, "RobustToolbox");
        assert_eq!(id, 0x0102030405060708);
        assert_eq!(time, 1.5);
        assert_eq!(hail, b"hail!");
    }

    #[test]
    fn best_chunk_size_fits_mtu() {
        let group = 3;
        let total = 8000;
        let mtu = 700;
        let chunk = best_chunk_size(group, total, mtu);
        let num = total.div_ceil(chunk);
        assert!(chunk + fragment_header_size(group, total, chunk, num) + HEADER_BYTES + 1 < mtu);
        assert!(chunk > 600);
        assert!(best_chunk_size(group, 1, 700) > 600);
    }

    #[test]
    fn pong_roundtrip() {
        let bytes = pong_payload(7, 2.25);
        assert_eq!(parse_pong(&bytes), Some((7, 2.25)));
    }
}
