//! TNEF (`winmail.dat`, `application/ms-tnef`) reader ([MS-OXTNEF]).
//!
//! The stream is a signature, a legacy key, then attributes
//! `{level u8, id u32, len u32, data[len], checksum u16}`. Legacy attributes are mapped to
//! their MAPI properties; `attMsgProps` / `attAttachment` / `attRecipTable` carry encoded
//! property lists and take precedence over the legacy attributes they duplicate.

use mail_model::{Error, Result};
use mapi_props::{MapiAttachment, MapiMessage, NamedId, Prop, PropKey, PropMap, PropType, PropValue};

use crate::value::*;

pub(crate) const TNEF_SIGNATURE: u32 = 0x223E_9F78;

const LVL_MESSAGE: u8 = 1;
const LVL_ATTACHMENT: u8 = 2;

// Attribute ids (type << 16 | id), [MS-OXTNEF] 2.1.3.2.
const ATT_OWNER: u32 = 0x0006_0000;
const ATT_SENT_FOR: u32 = 0x0006_0001;
const ATT_DELEGATE: u32 = 0x0006_0002;
const ATT_DATE_START: u32 = 0x0003_0006;
const ATT_DATE_END: u32 = 0x0003_0007;
const ATT_AID_OWNER: u32 = 0x0005_0008;
const ATT_REQUEST_RES: u32 = 0x0004_0009;
const ATT_ORIGINAL_MESSAGE_CLASS: u32 = 0x0007_0600;
const ATT_FROM: u32 = 0x0000_8000;
const ATT_SUBJECT: u32 = 0x0001_8004;
const ATT_DATE_SENT: u32 = 0x0003_8005;
const ATT_DATE_RECD: u32 = 0x0003_8006;
const ATT_MESSAGE_STATUS: u32 = 0x0006_8007;
const ATT_MESSAGE_CLASS: u32 = 0x0007_8008;
const ATT_MESSAGE_ID: u32 = 0x0001_8009;
const ATT_BODY: u32 = 0x0002_800C;
const ATT_PRIORITY: u32 = 0x0004_800D;
const ATT_ATTACH_DATA: u32 = 0x0006_800F;
const ATT_ATTACH_TITLE: u32 = 0x0001_8010;
const ATT_ATTACH_META_FILE: u32 = 0x0006_8011;
const ATT_ATTACH_CREATE_DATE: u32 = 0x0003_8012;
const ATT_ATTACH_MODIFY_DATE: u32 = 0x0003_8013;
const ATT_DATE_MODIFIED: u32 = 0x0003_8020;
const ATT_ATTACH_TRANSPORT_FILENAME: u32 = 0x0006_9001;
const ATT_ATTACH_REND_DATA: u32 = 0x0006_9002;
const ATT_MSG_PROPS: u32 = 0x0006_9003;
const ATT_RECIP_TABLE: u32 = 0x0006_9004;
const ATT_ATTACHMENT: u32 = 0x0006_9005;
const ATT_TNEF_VERSION: u32 = 0x0008_9006;
const ATT_OEM_CODEPAGE: u32 = 0x0006_9007;

const PID_RCVD_REPRESENTING_ENTRY_ID: u16 = 0x0043;
const PID_RCVD_REPRESENTING_NAME: u16 = 0x0044;
const PID_RCVD_REPRESENTING_ADDRTYPE: u16 = 0x0077;
const PID_RCVD_REPRESENTING_EMAIL: u16 = 0x0078;

/// PidTagAttachTag value for OLE attachments ([MS-OXTNEF] 2.3.3.9).
const OLE_ATTACH_TAG: [u8; 11] = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x14, 0x03, 0x0A, 0x03, 0x02, 0x01];

pub(crate) fn has_signature(data: &[u8]) -> bool {
    u32_at(data, 0) == Some(TNEF_SIGNATURE)
}

fn trunc(what: &str) -> Error {
    Error::Format(format!("tnef: truncated {what}"))
}

#[derive(Default)]
struct AttachBuild {
    legacy: PropMap,
    mapi: PropMap,
}

/// Decode a TNEF stream. Attachment payloads stay in `PidTagAttachDataBinary` (for embedded
/// messages: the inner TNEF stream / compound file without the IID).
///
/// Damage is tolerated so a viewer can show what survives: attributes of unknown level are
/// skipped, a property list keeps the properties before a malformed/unsupported one, and a
/// truncated stream keeps the attributes before the cut (all logged at debug level). It fails
/// only on a bad signature or when no message/recipient/attachment property was decoded.
pub(crate) fn parse(data: &[u8]) -> Result<MapiMessage> {
    if !has_signature(data) {
        return Err(Error::Format("tnef: missing signature 0x223E9F78".into()));
    }
    if data.len() < 6 {
        return Err(trunc("header"));
    }
    let mut pos = 6usize;
    let mut oem_cp: Option<u32> = None;
    let mut legacy = PropMap::new();
    let mut mapi = PropMap::new();
    let mut recipients: Vec<PropMap> = Vec::new();
    let mut atts: Vec<AttachBuild> = Vec::new();
    let mut bad_checksums = 0u32;
    // First damage seen; the error reported if nothing usable was decoded.
    let mut damage: Option<Error> = None;

    while pos < data.len() {
        let rest = &data[pos..];
        // Fewer than 9 bytes: trailing padding after the last attribute.
        let (Some(id), Some(len)) = (u32_at(rest, 1), u32_at(rest, 5)) else { break };
        let level = rest[0];
        let Some(end) = 9usize.checked_add(len as usize).filter(|e| e + 2 <= rest.len()) else {
            tracing::debug!(offset = pos, "tnef: truncated attribute; keeping what was decoded before it");
            damage.get_or_insert_with(|| trunc("attribute data"));
            break;
        };
        let body = &rest[9..end];
        let checksum = u16_at(rest, end).unwrap_or_default();
        pos += end + 2;
        let sum = body.iter().fold(0u16, |a, &b| a.wrapping_add(b as u16));
        // Legacy writers produced bad checksums for message-class attributes ([MS-OXTNEF] 2.3.3.4).
        if sum != checksum && id != ATT_MESSAGE_CLASS && id != ATT_ORIGINAL_MESSAGE_CLASS {
            bad_checksums += 1;
        }
        match level {
            LVL_MESSAGE => match id {
                ATT_TNEF_VERSION => {}
                ATT_OEM_CODEPAGE => oem_cp = u32_at(body, 0).filter(|&c| c != 0),
                ATT_MESSAGE_CLASS => set_raw8(&mut legacy, PID_MESSAGE_CLASS, map_message_class(body)),
                ATT_ORIGINAL_MESSAGE_CLASS => {
                    set_raw8(&mut legacy, PID_ORIGINAL_MESSAGE_CLASS, map_message_class(body))
                }
                ATT_SUBJECT => set_raw8(&mut legacy, PID_SUBJECT, body.to_vec()),
                ATT_BODY => set_raw8(&mut legacy, PID_BODY, body.to_vec()),
                ATT_MESSAGE_ID => {
                    if let Some(key) = unhex(body) {
                        set(&mut legacy, PID_SEARCH_KEY, PropType::Binary, PropValue::Binary(key));
                    }
                }
                ATT_DATE_SENT => set_date(&mut legacy, PID_CLIENT_SUBMIT_TIME, body),
                ATT_DATE_RECD => set_date(&mut legacy, PID_MESSAGE_DELIVERY_TIME, body),
                ATT_DATE_MODIFIED => set_date(&mut legacy, PID_LAST_MODIFICATION_TIME, body),
                ATT_DATE_START => set_date(&mut legacy, PID_START_DATE, body),
                ATT_DATE_END => set_date(&mut legacy, PID_END_DATE, body),
                ATT_MESSAGE_STATUS => {
                    if let Some(&fms) = body.first() {
                        set(&mut legacy, PID_MESSAGE_FLAGS, PropType::Int32, PropValue::Int32(status_to_flags(fms)));
                    }
                }
                ATT_PRIORITY => {
                    if let Some(p) = u16_at(body, 0) {
                        let imp = match p {
                            1 => 2,
                            3 => 0,
                            _ => 1,
                        };
                        set(&mut legacy, PID_IMPORTANCE, PropType::Int32, PropValue::Int32(imp));
                    }
                }
                ATT_FROM => {
                    if let Some((name, addr)) = parse_trp(body) {
                        set_address(&mut legacy, [PID_SENDER_NAME, PID_SENDER_ADDRTYPE, PID_SENDER_EMAIL], name, addr);
                    }
                }
                ATT_SENT_FOR => {
                    if let Some((name, addr)) = parse_counted_address(body) {
                        set_address(
                            &mut legacy,
                            [PID_SENT_REPRESENTING_NAME, PID_SENT_REPRESENTING_ADDRTYPE, PID_SENT_REPRESENTING_EMAIL],
                            name,
                            addr,
                        );
                    }
                }
                ATT_OWNER => {
                    if let Some((name, addr)) = parse_counted_address(body) {
                        let ids = if is_meeting_response(&legacy) {
                            [PID_RCVD_REPRESENTING_NAME, PID_RCVD_REPRESENTING_ADDRTYPE, PID_RCVD_REPRESENTING_EMAIL]
                        } else {
                            [PID_SENT_REPRESENTING_NAME, PID_SENT_REPRESENTING_ADDRTYPE, PID_SENT_REPRESENTING_EMAIL]
                        };
                        set_address(&mut legacy, ids, name, addr);
                    }
                }
                ATT_DELEGATE => {
                    set(&mut legacy, PID_RCVD_REPRESENTING_ENTRY_ID, PropType::Binary, PropValue::Binary(body.to_vec()))
                }
                ATT_AID_OWNER => {
                    if let Some(v) = u32_at(body, 0) {
                        set(&mut legacy, PID_OWNER_APPT_ID, PropType::Int32, PropValue::Int32(v as i32));
                    }
                }
                ATT_REQUEST_RES => {
                    if let Some(v) = u16_at(body, 0) {
                        set(&mut legacy, PID_RESPONSE_REQUESTED, PropType::Boolean, PropValue::Boolean(v & 0xFF != 0));
                    }
                }
                ATT_MSG_PROPS => keep_partial(parse_prop_list(body, &mut 0, &mut mapi), "attMsgProps", &mut damage),
                ATT_RECIP_TABLE => keep_partial(parse_recip_table(body, &mut recipients), "attRecipTable", &mut damage),
                _ => {}
            },
            LVL_ATTACHMENT => {
                if id == ATT_ATTACH_REND_DATA || atts.is_empty() {
                    atts.push(AttachBuild::default());
                }
                let a = atts.last_mut().expect("pushed above");
                match id {
                    ATT_ATTACH_REND_DATA => {
                        let atyp = u16_at(body, 0).unwrap_or(1);
                        if let Some(p) = u32_at(body, 2) {
                            set(&mut a.legacy, PID_RENDERING_POSITION, PropType::Int32, PropValue::Int32(p as i32));
                        }
                        let method = if atyp == 2 {
                            set(
                                &mut a.legacy,
                                PID_ATTACH_TAG,
                                PropType::Binary,
                                PropValue::Binary(OLE_ATTACH_TAG.to_vec()),
                            );
                            ATTACH_OLE
                        } else {
                            ATTACH_BY_VALUE
                        };
                        set(&mut a.legacy, PID_ATTACH_METHOD, PropType::Int32, PropValue::Int32(method));
                    }
                    ATT_ATTACH_TITLE => {
                        set_raw8(&mut a.legacy, PID_ATTACH_FILENAME, body.to_vec());
                        set_raw8(&mut a.legacy, PID_ATTACH_LONG_FILENAME, body.to_vec());
                    }
                    ATT_ATTACH_DATA => {
                        set(&mut a.legacy, PID_ATTACH_DATA, PropType::Binary, PropValue::Binary(body.to_vec()))
                    }
                    ATT_ATTACH_META_FILE => {
                        set(&mut a.legacy, PID_ATTACH_RENDERING, PropType::Binary, PropValue::Binary(body.to_vec()))
                    }
                    ATT_ATTACH_CREATE_DATE => set_date(&mut a.legacy, PID_CREATION_TIME, body),
                    ATT_ATTACH_MODIFY_DATE => set_date(&mut a.legacy, PID_LAST_MODIFICATION_TIME, body),
                    ATT_ATTACH_TRANSPORT_FILENAME => set_raw8(&mut a.legacy, PID_ATTACH_TRANSPORT_NAME, body.to_vec()),
                    ATT_ATTACHMENT => {
                        keep_partial(parse_prop_list(body, &mut 0, &mut a.mapi), "attAttachment", &mut damage)
                    }
                    _ => {}
                }
            }
            other => {
                tracing::debug!(level = other, id, "tnef: skipping attribute of unknown level");
                damage.get_or_insert_with(|| Error::Format(format!("tnef: unknown attribute level {other}")));
            }
        }
    }
    if legacy.is_empty() && mapi.is_empty() && recipients.is_empty() && atts.is_empty() {
        return Err(damage.unwrap_or_else(|| Error::Format("tnef: no message content".into())));
    }
    if bad_checksums > 0 {
        tracing::warn!(bad_checksums, "TNEF attribute checksum mismatch");
    }

    // [MS-OXTNEF] 2.3.3.2: attOemCodepage, then PidTagInternetCodepage.
    let cp = oem_cp.or_else(|| message_codepage(&mapi, None));
    let mut props = merge(mapi, legacy);
    decode_string8_props(&mut props, cp);
    for r in &mut recipients {
        decode_string8_props(r, cp);
    }
    let attachments = atts
        .into_iter()
        .map(|a| {
            let mut props = merge(a.mapi, a.legacy);
            decode_string8_props(&mut props, cp);
            unwrap_attach_object(&mut props);
            MapiAttachment { props, embedded: None }
        })
        .collect();
    Ok(MapiMessage { props, recipients, attachments })
}

/// Encapsulated MAPI properties win over the legacy attributes they duplicate.
fn merge(mut mapi: PropMap, legacy: PropMap) -> PropMap {
    for (k, v) in legacy {
        mapi.entry(k).or_insert(v);
    }
    mapi
}

/// `PidTagAttachDataObject` is `IID + payload`. An `IID_IMessage` payload is a nested TNEF
/// stream; an `IStorage` payload holding `__properties_version1.0` is a `.msg`. Both are
/// embedded messages. The payload replaces the value so `attachment()` returns it directly.
fn unwrap_attach_object(props: &mut PropMap) {
    let Some(p) = props.get_mut(&PropKey::Id(PID_ATTACH_DATA)) else { return };
    if p.ty != PropType::Object {
        return;
    }
    let PropValue::Binary(raw) = &mut p.value else { return };
    if raw.len() < 16 {
        return;
    }
    let iid: [u8; 16] = raw[..16].try_into().expect("16 bytes");
    raw.drain(..16);
    let embedded = (iid == IID_IMESSAGE && has_signature(raw)) || crate::msg_bytes_is_message(raw);
    if embedded {
        set(props, PID_ATTACH_METHOD, PropType::Int32, PropValue::Int32(ATTACH_EMBEDDED_MSG));
    }
}

fn set_raw8(map: &mut PropMap, id: u16, bytes: Vec<u8>) {
    set(map, id, PropType::String8, PropValue::Binary(bytes));
}

fn set_date(map: &mut PropMap, id: u16, b: &[u8]) {
    let f = |i: usize| u16_at(b, i * 2).map(|v| v as u32);
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) = (f(0), f(1), f(2), f(3), f(4), f(5)) else { return };
    if let Some(ms) = civil_to_ms(y as i64, mo, d, h, mi, s) {
        set(map, id, PropType::SysTime, PropValue::SysTime(ms));
    }
}

fn set_address(map: &mut PropMap, ids: [u16; 3], name: &[u8], addr: &[u8]) {
    if !name.is_empty() {
        set_raw8(map, ids[0], name.to_vec());
    }
    match addr.iter().position(|&c| c == b':') {
        Some(i) => {
            set_raw8(map, ids[1], addr[..i].to_vec());
            set_raw8(map, ids[2], addr[i + 1..].to_vec());
        }
        None if !addr.is_empty() => set_raw8(map, ids[2], addr.to_vec()),
        None => {}
    }
}

fn trim_nul(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

/// attFrom TRP structure → (display name, "TYPE:address").
fn parse_trp(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let name_len = u16_at(b, 4)? as usize;
    let addr_len = u16_at(b, 6)? as usize;
    let name = b.get(8..8 + name_len)?;
    let addr = b.get(8 + name_len..8 + name_len + addr_len)?;
    Some((trim_nul(name), trim_nul(addr)))
}

/// attOwner / attSentFor: `u16 len, name, u16 len, "TYPE:address"`.
fn parse_counted_address(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let nl = u16_at(b, 0)? as usize;
    let name = b.get(2..2 + nl)?;
    let al = u16_at(b, 2 + nl)? as usize;
    let addr = b.get(4 + nl..4 + nl + al)?;
    Some((trim_nul(name), trim_nul(addr)))
}

fn is_meeting_response(legacy: &PropMap) -> bool {
    matches!(legacy.get(&PropKey::Id(PID_MESSAGE_CLASS)), Some(Prop { value: PropValue::Binary(c), .. })
        if c.starts_with(b"IPM.Schedule.Meeting.Resp"))
}

/// [MS-OXTNEF] 2.3.3.4 legacy message class names.
fn map_message_class(body: &[u8]) -> Vec<u8> {
    let mut c = trim_nul(body);
    if let Some(rest) = c.strip_prefix(b"Microsoft Mail v3.0 ") {
        c = rest;
    }
    let mapped: &[u8] = match c {
        b"IPM.Microsoft Mail.Note" => b"IPM.Note",
        b"IPM.Microsoft Mail.Read Receipt" => b"Report.IPM.Note.IPNRN",
        b"IPM.Microsoft Mail.Non-Delivery" => b"Report.IPM.Note.NDR",
        b"IPM.Microsoft Schedule.MtgRespP" => b"IPM.Schedule.Meeting.Resp.Pos",
        b"IPM.Microsoft Schedule.MtgRespN" => b"IPM.Schedule.Meeting.Resp.Neg",
        b"IPM.Microsoft Schedule.MtgRespA" => b"IPM.Schedule.Meeting.Resp.Tent",
        b"IPM.Microsoft Schedule.MtgReq" => b"IPM.Schedule.Meeting.Request",
        b"IPM.Microsoft Schedule.MtgCncl" => b"IPM.Schedule.Meeting.Canceled",
        other => other,
    };
    mapped.to_vec()
}

/// attMessageStatus (fms*) → PidTagMessageFlags (MSGFLAG_*), [MS-OXTNEF] 2.1.3.3.8.
fn status_to_flags(fms: u8) -> i32 {
    let mut f = 0;
    if fms & 0x20 != 0 {
        f |= 0x01;
    }
    if fms & 0x01 == 0 {
        f |= 0x02;
    }
    if fms & 0x04 != 0 {
        f |= 0x04;
    }
    if fms & 0x02 != 0 {
        f |= 0x08;
    }
    if fms & 0x80 != 0 {
        f |= 0x10;
    }
    f
}

fn unhex(b: &[u8]) -> Option<Vec<u8>> {
    let s = trim_nul(b);
    if s.is_empty() || !s.len().is_multiple_of(2) {
        return None;
    }
    let nib = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    s.as_chunks::<2>().0.iter().map(|p| Some(nib(p[0])? << 4 | nib(p[1])?)).collect()
}

// ---- encapsulated property lists ([MS-OXTNEF] 2.1.3.4) ----

fn rd_u32(b: &[u8], pos: &mut usize) -> Result<u32> {
    let v = u32_at(b, *pos).ok_or_else(|| trunc("property list"))?;
    *pos += 4;
    Ok(v)
}

fn rd_bytes<'a>(b: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = pos.checked_add(n).and_then(|e| b.get(*pos..e)).ok_or_else(|| trunc("property value"))?;
    *pos += n;
    Ok(s)
}

#[inline]
fn pad4(n: usize) -> usize {
    (4 - n % 4) % 4
}

/// Like `rd_bytes` + 4-byte padding; tolerates a missing final pad.
fn rd_padded<'a>(b: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = rd_bytes(b, pos, n)?;
    *pos = (*pos + pad4(n)).min(b.len());
    Ok(s)
}

/// Logs a damaged property list. Values cannot be resynchronised after a malformed or
/// unsupported property, so the list stops there; properties decoded before it are kept.
fn keep_partial(res: Result<()>, attribute: &str, damage: &mut Option<Error>) {
    if let Err(e) = res {
        tracing::debug!(attribute, error = %e, "tnef: rest of property list skipped");
        damage.get_or_insert(e);
    }
}

/// attRecipTable: row count, then one property list per recipient. On damage, rows (and the
/// partial row) decoded so far are kept in `out`.
fn parse_recip_table(b: &[u8], out: &mut Vec<PropMap>) -> Result<()> {
    let mut pos = 0;
    let rows = rd_u32(b, &mut pos)?;
    for _ in 0..rows {
        let mut row = PropMap::new();
        let res = parse_prop_list(b, &mut pos, &mut row);
        if !row.is_empty() {
            out.push(row);
        }
        res?;
    }
    Ok(())
}

/// Decodes a property list into `out`, which keeps every property decoded before an error.
fn parse_prop_list(b: &[u8], pos: &mut usize, out: &mut PropMap) -> Result<()> {
    let count = rd_u32(b, pos)?;
    for _ in 0..count {
        let tag = rd_u32(b, pos)?;
        let raw_ty = tag as u16;
        let id = (tag >> 16) as u16;
        let key = if id >= 0x8000 {
            let guid: [u8; 16] = rd_bytes(b, pos, 16)?.try_into().expect("16 bytes");
            let kind = rd_u32(b, pos)?;
            let nid = match kind {
                0 => NamedId::Lid(rd_u32(b, pos)?),
                1 => {
                    let len = rd_u32(b, pos)? as usize;
                    NamedId::Name(utf16_to_string(rd_padded(b, pos, len)?))
                }
                k => return Err(Error::Format(format!("tnef: bad named property kind {k}"))),
            };
            PropKey::Named { guid, id: nid }
        } else {
            PropKey::Id(id)
        };
        let ty = PropType::from_u16(raw_ty)
            .ok_or_else(|| Error::Format(format!("tnef: unsupported property type 0x{raw_ty:04X}")))?;
        let value = read_value(b, pos, ty)?;
        out.insert(key, Prop { ty, value });
    }
    Ok(())
}

fn read_variable(b: &[u8], pos: &mut usize) -> Result<Vec<Vec<u8>>> {
    let n = rd_u32(b, pos)? as usize;
    let mut vals = Vec::with_capacity(n.min(b.len().saturating_sub(*pos) / 4));
    for _ in 0..n {
        let len = rd_u32(b, pos)? as usize;
        vals.push(rd_padded(b, pos, len)?.to_vec());
    }
    Ok(vals)
}

fn read_value(b: &[u8], pos: &mut usize, ty: PropType) -> Result<PropValue> {
    use PropType::*;
    Ok(match ty {
        Null => PropValue::Null,
        String8 | Unicode | Binary | Object | ServerId => {
            let mut vals = read_variable(b, pos)?;
            let first = if vals.is_empty() { Vec::new() } else { vals.swap_remove(0) };
            match ty {
                Unicode => PropValue::String(utf16_to_string(&first)),
                _ => PropValue::Binary(first),
            }
        }
        MultiString8 | MultiBinary => PropValue::MultiBinary(read_variable(b, pos)?),
        MultiUnicode => PropValue::MultiString(read_variable(b, pos)?.iter().map(|v| utf16_to_string(v)).collect()),
        Restriction | RuleAction => {
            return Err(Error::Format(format!("tnef: cannot decode property type {ty:?}")));
        }
        _ if mv_elem(ty).is_some() => {
            let elem = mv_elem(ty).expect("checked");
            let size = scalar_size(ty).expect("fixed");
            let n = rd_u32(b, pos)? as usize;
            let mut vals = Vec::with_capacity(n.min(b.len().saturating_sub(*pos) / 4));
            for _ in 0..n {
                let raw = rd_padded(b, pos, size)?;
                vals.push(decode_scalar(elem, raw).ok_or_else(|| trunc("multi-valued property"))?);
            }
            collect_mv(ty, vals).expect("multi-valued type")
        }
        _ => {
            let size = scalar_size(ty).ok_or_else(|| Error::Format(format!("tnef: unsized type {ty:?}")))?;
            // 8-byte scalar reads need 8 bytes; Int16/Boolean occupy 2 bytes + padding.
            let raw = rd_padded(b, pos, size)?;
            decode_scalar(ty, raw).ok_or_else(|| trunc("scalar property"))?
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_mapping() {
        assert_eq!(map_message_class(b"IPM.Microsoft Mail.Note\0"), b"IPM.Note");
        assert_eq!(
            map_message_class(b"Microsoft Mail v3.0 IPM.Microsoft Schedule.MtgReq\0"),
            b"IPM.Schedule.Meeting.Request"
        );
        assert_eq!(map_message_class(b"IPM.Custom\0"), b"IPM.Custom");
    }

    #[test]
    fn status_flags() {
        // read + has-attach, unmodified flag clear → MSGFLAG_UNMODIFIED set
        assert_eq!(status_to_flags(0x20 | 0x80), 0x01 | 0x02 | 0x10);
        assert_eq!(status_to_flags(0x01), 0);
    }

    #[test]
    fn trp_and_counted() {
        let mut trp = vec![4, 0, 0, 0, 5, 0, 12, 0];
        trp.extend_from_slice(b"Name\0SMTP:a@b.c\0");
        trp.extend_from_slice(&[0; 8]);
        let (n, a) = parse_trp(&trp).unwrap();
        assert_eq!((n, a), (&b"Name"[..], &b"SMTP:a@b.c"[..]));
        assert!(parse_trp(&trp[..10]).is_none());

        let mut c = vec![3, 0];
        c.extend_from_slice(b"Al\0");
        c.extend_from_slice(&[7, 0]);
        c.extend_from_slice(b"EX:/o\0\0");
        assert_eq!(parse_counted_address(&c).unwrap(), (&b"Al"[..], &b"EX:/o"[..]));
    }

    #[test]
    fn hex_search_key() {
        assert_eq!(unhex(b"0aFF\0"), Some(vec![0x0A, 0xFF]));
        assert_eq!(unhex(b"0g"), None);
    }

    #[test]
    fn truncated_lists_are_format_errors() {
        // count=1, tag=PT_LONG 0x0E07 but no value.
        let b = [1, 0, 0, 0, 0x03, 0x00, 0x07, 0x0E];
        let mut m = PropMap::new();
        assert!(matches!(parse_prop_list(&b, &mut 0, &mut m), Err(Error::Format(_))));
        // absurd count must not allocate or loop forever
        let b = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(matches!(parse_prop_list(&b, &mut 0, &mut m), Err(Error::Format(_))));
        // Properties before a damaged one are kept: count=2, PT_LONG 0x0E07 = 7, then garbage type.
        let b = [2, 0, 0, 0, 0x03, 0x00, 0x07, 0x0E, 7, 0, 0, 0, 0xFF, 0x0F, 0x01, 0x60];
        let mut m = PropMap::new();
        assert!(parse_prop_list(&b, &mut 0, &mut m).is_err());
        assert_eq!(m.get(&PropKey::Id(0x0E07)).map(|p| &p.value), Some(&PropValue::Int32(7)));
    }
}
