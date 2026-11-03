//! Minimal BER/DER walker to pull the encapsulated content out of an opaque
//! S/MIME `signed-data` blob (RFC 5652 `ContentInfo { signedData, SignedData }`).

/// One TLV: tag byte, content range, and whether it used indefinite length.
struct Tlv {
    tag: u8,
    start: usize,
    end: usize,
    /// Offset just past the element (incl. end-of-contents for indefinite length).
    next: usize,
}

const MAX_DEPTH: u32 = 32;

fn tlv(b: &[u8], pos: usize, depth: u32) -> Option<Tlv> {
    if depth > MAX_DEPTH {
        return None;
    }
    let tag = *b.get(pos)?;
    if tag & 0x1f == 0x1f {
        return None; // high-tag-number form never appears in the structures we read
    }
    let l0 = *b.get(pos + 1)?;
    let mut p = pos + 2;
    if l0 == 0x80 {
        if tag & 0x20 == 0 {
            return None; // indefinite length only for constructed
        }
        let start = p;
        loop {
            if b.get(p..p + 2)? == [0, 0] {
                return Some(Tlv { tag, start, end: p, next: p + 2 });
            }
            p = tlv(b, p, depth + 1)?.next;
        }
    }
    let len = if l0 & 0x80 == 0 {
        l0 as usize
    } else {
        let n = (l0 & 0x7f) as usize;
        if n == 0 || n > 8 {
            return None;
        }
        let mut v = 0usize;
        for _ in 0..n {
            v = v.checked_mul(256)?.checked_add(*b.get(p)? as usize)?;
            p += 1;
        }
        v
    };
    let end = p.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some(Tlv { tag, start: p, end, next: end })
}

fn children(b: &[u8], t: &Tlv) -> Option<Vec<Tlv>> {
    let mut out = Vec::new();
    let mut p = t.start;
    while p < t.end {
        let c = tlv(b, p, 1)?;
        p = c.next;
        out.push(c);
    }
    Some(out)
}

/// OCTET STRING contents, flattening constructed (chunked BER) strings.
fn octets(b: &[u8], t: &Tlv, out: &mut Vec<u8>, depth: u32) -> Option<()> {
    if depth > MAX_DEPTH {
        return None;
    }
    match t.tag {
        0x04 => out.extend_from_slice(&b[t.start..t.end]),
        0x24 => {
            for c in children(b, t)? {
                octets(b, &c, out, depth + 1)?;
            }
        }
        _ => return None,
    }
    Some(())
}

const OID_SIGNED_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];

/// The signed MIME entity inside an opaque `signed-data` ContentInfo, if present.
pub(crate) fn signed_content(der: &[u8]) -> Option<Vec<u8>> {
    let ci = tlv(der, 0, 0)?;
    if ci.tag != 0x30 {
        return None;
    }
    let ci_c = children(der, &ci)?;
    let (oid, explicit) = (ci_c.first()?, ci_c.get(1)?);
    if oid.tag != 0x06 || &der[oid.start..oid.end] != OID_SIGNED_DATA || explicit.tag != 0xa0 {
        return None;
    }
    let sd = children(der, explicit)?.into_iter().next()?;
    if sd.tag != 0x30 {
        return None;
    }
    // SignedData: version, digestAlgorithms, encapContentInfo, ...
    let encap = children(der, &sd)?.into_iter().nth(2)?;
    if encap.tag != 0x30 {
        return None;
    }
    let econtent = children(der, &encap)?.into_iter().nth(1)?;
    if econtent.tag != 0xa0 {
        return None;
    }
    let os = children(der, &econtent)?.into_iter().next()?;
    let mut out = Vec::new();
    octets(der, &os, &mut out, 0)?;
    Some(out)
}

/// Test/fixture helper: DER-encodes an opaque signed-data ContentInfo around `content`
/// (no signers; enough for structure tests).
#[doc(hidden)]
pub fn encode_signed_data(content: &[u8]) -> Vec<u8> {
    fn enc(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        let n = body.len();
        if n < 0x80 {
            v.push(n as u8);
        } else {
            let bytes = n.to_be_bytes();
            let skip = bytes.iter().take_while(|b| **b == 0).count();
            v.push(0x80 | (8 - skip) as u8);
            v.extend_from_slice(&bytes[skip..]);
        }
        v.extend_from_slice(body);
        v
    }
    let data_oid = enc(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x01]);
    let econtent = enc(0xa0, &enc(0x04, content));
    let encap = enc(0x30, &[data_oid, econtent].concat());
    let version = enc(0x02, &[1]);
    let digests = enc(0x31, &[]);
    let signer_infos = enc(0x31, &[]);
    let sd = enc(0x30, &[version, digests, encap, signer_infos].concat());
    enc(0x30, &[enc(0x06, OID_SIGNED_DATA), enc(0xa0, &sd)].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_garbage() {
        let content = b"Content-Type: text/plain\r\n\r\nsigned hello\r\n".repeat(10);
        let der = encode_signed_data(&content);
        assert_eq!(signed_content(&der).as_deref(), Some(&content[..]));
        for cut in 0..der.len() {
            assert!(signed_content(&der[..cut]).is_none() || cut == der.len());
        }
        assert!(signed_content(b"\x30\x80\x30\x80").is_none());
        assert!(signed_content(&[]).is_none());
    }

    #[test]
    fn indefinite_length_ber() {
        // ContentInfo with indefinite lengths and a chunked OCTET STRING.
        let mut der = vec![0x30, 0x80, 0x06, 0x09];
        der.extend_from_slice(OID_SIGNED_DATA);
        der.extend_from_slice(&[0xa0, 0x80, 0x30, 0x80, 0x02, 0x01, 0x01, 0x31, 0x00]);
        der.extend_from_slice(&[0x30, 0x80, 0x06, 0x01, 0x00, 0xa0, 0x80, 0x24, 0x80]);
        der.extend_from_slice(&[0x04, 0x02, b'h', b'i', 0x04, 0x01, b'!', 0, 0]);
        der.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(signed_content(&der).as_deref(), Some(&b"hi!"[..]));
    }
}
