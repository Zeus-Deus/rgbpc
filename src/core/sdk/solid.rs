//! Additive, capability-gated solid-color finish after the legacy CLI sequence.
//! Readback confirms the server's cached state, not physical LED acknowledgement.
//! Wire layout: pinned OpenRGB 1.1 RGBController.cpp Get{Device,Mode,Zone}DescriptionData,
//! protocol 3 (including the vendor-internal mode value absent from protocol 6).
use super::{word, Client};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const MAX_METADATA: usize = 1024 * 1024;
const PER_LED: u32 = 1 << 5;
const MODE_SPECIFIC: u32 = 1 << 6;

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(len).ok_or("SDK length overflow")?;
        let part = self
            .bytes
            .get(self.at..end)
            .ok_or("Truncated SDK descriptor")?;
        self.at = end;
        Ok(part)
    }
    fn u16(&mut self) -> Result<usize, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as usize)
    }
    fn u32(&mut self) -> Result<u32, String> {
        word(self.take(4)?)
    }
    fn string(&mut self) -> Result<String, String> {
        let len = self.u16()?;
        let bytes = self.take(len)?;
        let content = bytes.strip_suffix(&[0]).ok_or("Unterminated SDK string")?;
        if content.contains(&0) {
            return Err("Embedded NUL in SDK string".into());
        }
        Ok(String::from_utf8_lossy(content).into_owned())
    }
    fn colors(&mut self) -> Result<Vec<[u8; 4]>, String> {
        let count = self.u16()?;
        Ok(self.take(count * 4)?.as_chunks::<4>().0.to_vec())
    }
}

struct Mode {
    name: String,
    flags: u32,
    selector_at: usize,
    colors_min: u32,
    colors_max: u32,
    colors: Vec<[u8; 4]>,
    raw: Vec<u8>,
}
struct Descriptor {
    name: String,
    active: u32,
    modes: Vec<Mode>,
    colors: Vec<[u8; 4]>,
}
impl Descriptor {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_METADATA {
            return Err("SDK descriptor exceeds size limit".into());
        }
        let mut c = Cursor { bytes, at: 0 };
        if c.u32()? as usize != bytes.len() {
            return Err("Invalid SDK descriptor size".into());
        }
        c.u32()?; // device type
        let name = c.string()?;
        for _ in 0..5 {
            c.string()?;
        } // vendor, description, version, serial, location
        let count = c.u16()?;
        let active = c.u32()?;
        let mut modes = Vec::new();
        for _ in 0..count {
            let start = c.at;
            let name = c.string()?;
            c.u32()?; // vendor value, preserved verbatim
            let flags = c.u32()?;
            c.take(4 * 4)?; // speed and brightness bounds
            let colors_min = c.u32()?;
            let colors_max = c.u32()?;
            c.take(3 * 4)?; // speed, brightness, direction
            let selector_at = c.at - start;
            c.u32()?; // current selector retained in raw
            let colors = c.colors()?;
            modes.push(Mode {
                name,
                flags,
                selector_at,
                colors_min,
                colors_max,
                colors,
                raw: bytes[start..c.at].to_vec(),
            });
        }
        if !modes.is_empty() && active != u32::MAX && active as usize >= modes.len() {
            return Err("Invalid SDK active mode index".into());
        }
        let zones = c.u16()?;
        for _ in 0..zones {
            c.string()?;
            c.take(4 * 4)?; // zone type, min, max, current size (never resize)
            let matrix_size = c.u16()?;
            let matrix = c.take(matrix_size)?;
            if !matrix.is_empty() {
                let mut m = Cursor {
                    bytes: matrix,
                    at: 0,
                };
                let height = m.u32()? as usize;
                let width = m.u32()? as usize;
                let len = height
                    .checked_mul(width)
                    .and_then(|n| n.checked_mul(4))
                    .ok_or("SDK matrix overflow")?;
                m.take(len)?;
                if m.at != matrix.len() {
                    return Err("Invalid SDK matrix size".into());
                }
            }
        }
        let leds = c.u16()?;
        for _ in 0..leds {
            c.string()?;
            c.u32()?;
        }
        let colors = c.colors()?;
        // LED descriptions and RGB slots have separate counts in the SDK.
        // In particular, mode-specific controllers need not name every slot.
        if c.at != bytes.len() {
            return Err("Invalid SDK trailing data".into());
        }
        Ok(Self {
            name,
            active,
            modes,
            colors,
        })
    }
}

fn parse_color(text: &str) -> Result<[u8; 4], String> {
    let text = text.strip_prefix('#').unwrap_or(text);
    if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Solid color must be exactly six hexadecimal digits".into());
    }
    let mut color = [0; 4];
    for i in 0..3 {
        color[i] = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(color)
}
fn sized(mut body: Vec<u8>) -> Result<Vec<u8>, String> {
    if body.len() > MAX_METADATA {
        return Err("SDK payload exceeds size limit".into());
    }
    let size = body.len() as u32;
    body[..4].copy_from_slice(&size.to_le_bytes());
    Ok(body)
}
fn read_descriptor(client: &mut Client, device: u32) -> Result<Descriptor, String> {
    client.send_to(device, 1, &3u32.to_le_bytes())?;
    Descriptor::parse(&client.expected_for(device, 1)?)
}
fn check_identity(d: &Descriptor, expected: &str) -> Result<(), String> {
    if d.name.trim() != expected.trim() {
        return Err("OpenRGB device identity changed before solid-color finish".into());
    }
    Ok(())
}

/// `false` retains CLI-only compatibility for unsupported protocols or modes.
/// Errors never claim a successful postcondition. No persistence, resize or brightness adjustment.
pub(crate) fn finalize_solid(
    address: SocketAddr,
    device_id: u32,
    expected_name: &str,
    color_hex: &str,
    preferred_mode: Option<&str>,
    timeout: Duration,
) -> Result<bool, String> {
    let color = parse_color(color_hex)?; // reject bad input before connecting
    let deadline = Instant::now() + timeout;
    let negotiation = (timeout / 2).min(Duration::from_millis(250));
    if negotiation.is_zero() {
        return Err(super::TIMEOUT.to_string());
    }
    let mut client = Client::connect(address, negotiation)?;
    client.send(40, &3u32.to_le_bytes())?;
    let version = match client.expected(40) {
        Ok(reply) => word(&reply)?,
        Err(error) if error == super::TIMEOUT && client.received_bytes == 0 => {
            // Unversioned servers can ignore this request entirely. Keep CLI
            // compatibility only after confirming the same peer still answers
            // the legacy count request and the selected index still exists.
            // Partial/malformed replies and any post-write errors stay errors.
            client.deadline = deadline;
            client.send(0, &[])?;
            let (controller, packet, reply) = client.receive_frame()?;
            if controller != 0 || packet != 0 {
                return Err("Unexpected OpenRGB SDK reply during legacy negotiation".into());
            }
            if word(&reply)? <= device_id {
                return Err("OpenRGB controller disappeared during SDK negotiation".into());
            }
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    client.deadline = deadline;
    if version < 3 {
        return Ok(false);
    }
    let before = read_descriptor(&mut client, device_id)?;
    check_identity(&before, expected_name)?;
    let preferred = preferred_mode.filter(|p| {
        ["Static", "Custom", "Direct"]
            .iter()
            .any(|n| n.eq_ignore_ascii_case(p))
    });
    let index = preferred
        .and_then(|p| {
            before
                .modes
                .iter()
                .position(|m| m.name.eq_ignore_ascii_case(p))
        })
        .unwrap_or(before.active as usize);
    let Some(mode) = before.modes.get(index) else {
        return Ok(false);
    };
    let current_selector = word(&mode.raw[mode.selector_at..mode.selector_at + 4])?;
    let per_led_available = mode.flags & PER_LED != 0 && !before.colors.is_empty();
    let mode_specific_available = mode.flags & MODE_SPECIFIC != 0
        && mode.colors_min.max(1) <= mode.colors_max.min(u16::MAX as u32);
    // Retain an already valid programmable mechanism instead of imposing a
    // different one merely because both capability bits happen to be set.
    let selector = if current_selector == 2 && mode_specific_available {
        2u32
    } else if per_led_available {
        1u32
    } else if mode_specific_available {
        2u32
    } else {
        return Ok(false);
    };
    let mut raw = mode.raw.clone();
    raw[mode.selector_at..mode.selector_at + 4].copy_from_slice(&selector.to_le_bytes());
    if selector == 2 {
        let min = mode.colors_min.max(1);
        let max = mode.colors_max.min(u16::MAX as u32);
        if min > max {
            return Ok(false);
        }
        let existing = mode.colors.len() as u32;
        let count = if (min..=max).contains(&existing) {
            existing
        } else {
            min
        };
        raw.truncate(mode.selector_at + 4);
        raw.extend_from_slice(&(count as u16).to_le_bytes());
        for _ in 0..count {
            raw.extend_from_slice(&color);
        }
    }
    let update_mode = before.active as usize != index || raw != mode.raw;
    let update_colors = selector == 1 && before.colors.iter().any(|c| c[..3] != color[..3]);
    if !update_mode && !update_colors {
        return Ok(true);
    }
    if update_mode {
        let mut body = vec![0; 4];
        body.extend_from_slice(&(index as u32).to_le_bytes());
        body.extend_from_slice(&raw);
        client.send_to(device_id, 1101, &sized(body)?)?;
    }
    if selector == 1 {
        // A mode change may replace LED colors; follow it with the desired frame.
        let mut body = vec![0; 4];
        body.extend_from_slice(&(before.colors.len() as u16).to_le_bytes());
        for _ in &before.colors {
            body.extend_from_slice(&color);
        }
        client.send_to(device_id, 1050, &sized(body)?)?;
    }
    // Same connection: controller request is a FIFO barrier for queued server writes.
    let after = read_descriptor(&mut client, device_id)?;
    check_identity(&after, expected_name)?;
    if after.active as usize != index
        || after.modes.get(index).is_none_or(|m| m.raw != raw)
        || (selector == 1
            && (after.colors.len() != before.colors.len()
                || after.colors.iter().any(|c| c[..3] != color[..3])))
    {
        return Err("OpenRGB solid-color finish failed server-state postcondition".into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};

    const RGB: [u8; 4] = [0x12, 0x34, 0xab, 0];
    type Request = (u32, u32, Vec<u8>);

    fn u16_to(v: &mut Vec<u8>, n: usize) {
        v.extend_from_slice(&(n as u16).to_le_bytes());
    }
    fn u32_to(v: &mut Vec<u8>, n: u32) {
        v.extend_from_slice(&n.to_le_bytes());
    }
    fn string_to(v: &mut Vec<u8>, s: &str) {
        u16_to(v, s.len() + 1);
        v.extend_from_slice(s.as_bytes());
        v.push(0);
    }
    fn full_size(mut v: Vec<u8>) -> Vec<u8> {
        let size = v.len() as u32;
        v[..4].copy_from_slice(&size.to_le_bytes());
        v
    }
    // Independent protocol-v3 encoder following pinned OpenRGB 1.1 C++ order.
    fn mode(
        name: &str,
        flags: u32,
        selector: u32,
        min: u32,
        max: u32,
        colors: &[[u8; 4]],
    ) -> Vec<u8> {
        let mut v = vec![];
        string_to(&mut v, name);
        for n in [0x76543210, flags, 9, 2, 1, 100, min, max, 5, 8, 1, selector] {
            u32_to(&mut v, n);
        }
        u16_to(&mut v, colors.len());
        for color in colors {
            v.extend_from_slice(color);
        }
        v
    }
    fn descriptor(
        name: &str,
        modes: &[Vec<u8>],
        active: u32,
        colors: &[[u8; 4]],
        matrix: bool,
    ) -> Vec<u8> {
        let mut v = vec![0; 4];
        u32_to(&mut v, 13);
        for s in [
            name,
            "GenericVendor",
            "Generic description",
            "1",
            "serial",
            "location",
        ] {
            string_to(&mut v, s);
        }
        u16_to(&mut v, modes.len());
        u32_to(&mut v, active);
        for m in modes {
            v.extend_from_slice(m);
        }
        u16_to(&mut v, 1);
        string_to(&mut v, "GenericZone");
        for n in [if matrix { 2 } else { 1 }, 0, 4096, colors.len() as u32] {
            u32_to(&mut v, n);
        }
        u16_to(&mut v, if matrix { 8 + colors.len() * 4 } else { 0 });
        if matrix {
            u32_to(&mut v, 1);
            u32_to(&mut v, colors.len() as u32);
            for i in 0..colors.len() {
                u32_to(&mut v, i as u32);
            }
        }
        u16_to(&mut v, colors.len());
        for _ in colors {
            string_to(&mut v, "GenericLED");
            u32_to(&mut v, 42);
        }
        u16_to(&mut v, colors.len());
        for c in colors {
            v.extend_from_slice(c);
        }
        full_size(v)
    }
    fn peer(
        version: u32,
        before: Vec<u8>,
        after: Vec<u8>,
    ) -> (SocketAddr, JoinHandle<Vec<Request>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut requests = vec![];
            let mut reads = 0;
            loop {
                let mut h = [0; 16];
                if socket.read_exact(&mut h).is_err() {
                    break;
                }
                assert_eq!(&h[..4], b"ORGB");
                let device = u32::from_le_bytes(h[4..8].try_into().unwrap());
                let packet = u32::from_le_bytes(h[8..12].try_into().unwrap());
                let size = u32::from_le_bytes(h[12..].try_into().unwrap()) as usize;
                assert!(size <= 1024 * 1024);
                let mut body = vec![0; size];
                socket.read_exact(&mut body).unwrap();
                requests.push((device, packet, body.clone()));
                let reply = match packet {
                    40 => {
                        assert_eq!(device, 0);
                        assert_eq!(body, 3u32.to_le_bytes());
                        Some(version.to_le_bytes().to_vec())
                    }
                    1 => {
                        assert_eq!(device, 7);
                        assert_eq!(body, 3u32.to_le_bytes());
                        reads += 1;
                        Some(if reads == 1 {
                            before.clone()
                        } else {
                            after.clone()
                        })
                    }
                    1101 | 1050 => {
                        assert_eq!(device, 7);
                        None
                    }
                    _ => panic!("unexpected mutation/request {packet}"),
                };
                if let Some(reply) = reply {
                    let mut h = b"ORGB".to_vec();
                    u32_to(&mut h, device);
                    u32_to(&mut h, packet);
                    u32_to(&mut h, reply.len() as u32);
                    if socket
                        .write_all(&h)
                        .and_then(|_| socket.write_all(&reply))
                        .is_err()
                    {
                        break;
                    }
                }
            }
            requests
        });
        (address, task)
    }
    fn run(
        version: u32,
        before: Vec<u8>,
        after: Vec<u8>,
        name: &str,
        preferred: Option<&str>,
    ) -> (Result<bool, String>, Vec<Request>) {
        let (address, task) = peer(version, before, after);
        let result = finalize_solid(
            address,
            7,
            name,
            "1234ab",
            preferred,
            Duration::from_secs(2),
        );
        // Wake accept on a stub that correctly exposes the intended RED.
        if result.is_err() {
            let _ = TcpStream::connect(address);
        }
        (result, task.join().unwrap())
    }

    #[test]
    fn invalid_static_selector_is_repaired_with_preserved_raw_metadata() {
        let before_mode = mode("Static", (1 << 5) | (1 << 4), 3, 0, 0, &[]);
        let desired_mode = mode("Static", (1 << 5) | (1 << 4), 1, 0, 0, &[]);
        let before = descriptor("GenericDevice", &[before_mode], 0, &[[0; 4]; 3], true);
        let after = descriptor(
            "GenericDevice",
            std::slice::from_ref(&desired_mode),
            0,
            &[RGB; 3],
            true,
        );
        let (result, requests) = run(6, before, after, "GenericDevice", Some("Static"));
        assert_eq!(result, Ok(true), "must repair and verify server state");
        assert_eq!(
            requests.iter().map(|r| r.1).collect::<Vec<_>>(),
            vec![40, 1, 1101, 1050, 1]
        );
        let mut expected_mode = vec![0; 4];
        u32_to(&mut expected_mode, 0);
        expected_mode.extend_from_slice(&desired_mode);
        assert_eq!(
            requests[2].2,
            full_size(expected_mode),
            "only selector may change; brightness stays 8"
        );
        let mut expected_colors = vec![0; 4];
        u16_to(&mut expected_colors, 3);
        for _ in 0..3 {
            expected_colors.extend_from_slice(&RGB);
        }
        assert_eq!(
            requests[3].2,
            full_size(expected_colors),
            "payload prefix includes itself"
        );
    }
    #[test]
    fn mode_specific_colors_use_valid_count_without_led_writes() {
        for (min, max, existing, wanted) in [(2, 4, 0, 2), (1, 4, 3, 3), (0, 4, 0, 1), (2, 4, 5, 2)]
        {
            let old = mode("Static", 1 << 6, 3, min, max, &vec![[0; 4]; existing]);
            let desired = mode("Static", 1 << 6, 2, min, max, &vec![RGB; wanted]);
            let before = descriptor("GenericDevice", &[old], 0, &[], false);
            let after = descriptor(
                "GenericDevice",
                std::slice::from_ref(&desired),
                0,
                &[],
                false,
            );
            let (result, requests) = run(6, before, after, "GenericDevice", Some("Static"));
            assert_eq!(
                result,
                Ok(true),
                "mode-specific min={min} max={max} existing={existing}"
            );
            assert_eq!(
                requests.iter().map(|r| r.1).collect::<Vec<_>>(),
                vec![40, 1, 1101, 1]
            );
            let mut body = vec![0; 4];
            u32_to(&mut body, 0);
            body.extend_from_slice(&desired);
            assert_eq!(requests[2].2, full_size(body));
        }
    }

    fn packets(requests: &[Request]) -> Vec<u32> {
        requests.iter().map(|r| r.1).collect()
    }
    fn per_led(selector: u32, colors: &[[u8; 4]]) -> Vec<u8> {
        descriptor(
            "GenericDevice",
            &[mode("Static", 1 << 5, selector, 0, 0, &[])],
            0,
            colors,
            false,
        )
    }
    #[test]
    fn valid_state_is_a_zero_mutation_noop() {
        let d = per_led(1, &[RGB; 7]);
        let (result, requests) = run(3, d.clone(), d, "  GenericDevice  ", Some("Static"));
        assert_eq!(result, Ok(true));
        assert_eq!(packets(&requests), vec![40, 1]);
    }
    #[test]
    fn identity_mismatch_has_zero_mutations() {
        let d = per_led(3, &[[0; 4]; 7]);
        let (result, requests) = run(6, d.clone(), d, "DifferentDevice", Some("Static"));
        assert!(result.unwrap_err().contains("identity"));
        assert_eq!(packets(&requests), vec![40, 1]);
    }
    #[test]
    fn unversioned_server_without_a_version_reply_keeps_cli_compatibility() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 20];
            socket.read_exact(&mut request).unwrap();
            assert_eq!(&request[8..12], &40u32.to_le_bytes());
            let mut next = [0; 16];
            if socket.read_exact(&mut next).is_ok() {
                assert_eq!(&next[8..12], &0u32.to_le_bytes());
                let reply = [
                    b"ORGB".as_slice(),
                    &[0; 8],
                    &4u32.to_le_bytes(),
                    &8u32.to_le_bytes(),
                ]
                .concat();
                socket.write_all(&reply).unwrap();
                true
            } else {
                false
            }
        });
        let result = finalize_solid(
            address,
            7,
            "GenericDevice",
            "1234ab",
            None,
            Duration::from_millis(200),
        );
        let count_was_confirmed = peer.join().unwrap();
        assert_eq!(result, Ok(false));
        assert!(count_was_confirmed);
    }

    #[test]
    fn partial_version_reply_is_not_downgraded_to_legacy_compatibility() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 20];
            socket.read_exact(&mut request).unwrap();
            socket.write_all(b"O").unwrap();
            let mut extra = [0; 1];
            socket.read(&mut extra).unwrap_or(0)
        });
        let result = finalize_solid(
            address,
            0,
            "GenericDevice",
            "1234ab",
            None,
            Duration::from_millis(200),
        );
        assert!(result.is_err());
        assert_eq!(
            peer.join().unwrap(),
            0,
            "partial replies must not trigger a count fallback"
        );
    }

    #[test]
    fn delayed_version_reply_cannot_be_discarded_as_legacy_success() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 20];
            socket.read_exact(&mut request).unwrap();
            thread::sleep(Duration::from_millis(200));
            let mut count_request = [0; 16];
            socket.read_exact(&mut count_request).unwrap();
            assert_eq!(&count_request[8..12], &0u32.to_le_bytes());
            let version = [
                b"ORGB".as_slice(),
                &[0; 4],
                &40u32.to_le_bytes(),
                &4u32.to_le_bytes(),
                &3u32.to_le_bytes(),
            ]
            .concat();
            let count = [
                b"ORGB".as_slice(),
                &[0; 8],
                &4u32.to_le_bytes(),
                &1u32.to_le_bytes(),
            ]
            .concat();
            socket.write_all(&[version, count].concat()).unwrap();
        });
        let result = finalize_solid(
            address,
            0,
            "GenericDevice",
            "1234ab",
            None,
            Duration::from_millis(300),
        );
        peer.join().unwrap();
        assert!(
            result.is_err(),
            "a late versioned reply cannot waive the finish: {result:?}"
        );
    }

    #[test]
    fn pre_v3_server_preserves_cli_only_path() {
        for version in [0, 1, 2] {
            let (result, requests) = run(version, vec![], vec![], "GenericDevice", Some("Static"));
            assert_eq!(result, Ok(false));
            assert_eq!(packets(&requests), vec![40]);
        }
    }
    #[test]
    fn unsupported_modes_and_empty_leds_do_not_mutate() {
        let fixtures = [
            descriptor("GenericDevice", &[], 0, &[RGB], false),
            descriptor(
                "GenericDevice",
                &[mode("Static", 0, 3, 0, 0, &[])],
                0,
                &[RGB],
                false,
            ),
            descriptor(
                "GenericDevice",
                &[mode("Static", 1 << 7, 3, 0, 0, &[])],
                0,
                &[RGB],
                false,
            ),
            per_led(3, &[]),
            descriptor(
                "GenericDevice",
                &[mode("Static", 1 << 6, 3, 0, 0, &[])],
                0,
                &[RGB],
                false,
            ),
            descriptor(
                "GenericDevice",
                &[mode("Static", 1 << 6, 3, 4, 2, &[])],
                0,
                &[RGB],
                false,
            ),
            descriptor(
                "GenericDevice",
                &[mode("Static", 1 << 6, 3, 65536, 70000, &[])],
                0,
                &[RGB],
                false,
            ),
        ];
        for d in fixtures {
            let (result, requests) = run(6, d.clone(), d, "GenericDevice", Some("Static"));
            assert_eq!(result, Ok(false));
            assert_eq!(packets(&requests), vec![40, 1]);
        }
    }
    #[test]
    fn ignored_write_is_a_failed_postcondition_not_success() {
        let d = per_led(3, &[[0; 4]; 4]);
        let (result, requests) = run(6, d.clone(), d, "GenericDevice", Some("Static"));
        assert!(result.unwrap_err().contains("postcondition"));
        assert_eq!(packets(&requests), vec![40, 1, 1101, 1050, 1]);
    }
    #[test]
    fn wrong_led_colors_fail_even_when_mode_was_repaired() {
        let (result, requests) = run(
            6,
            per_led(3, &[[0; 4]; 4]),
            per_led(1, &[[0; 4]; 4]),
            "GenericDevice",
            Some("Static"),
        );
        assert!(result.unwrap_err().contains("postcondition"));
        assert_eq!(packets(&requests).last(), Some(&1));
    }
    #[test]
    fn readback_identity_change_is_an_error() {
        let after = descriptor(
            "DifferentDevice",
            &[mode("Static", 1 << 5, 1, 0, 0, &[])],
            0,
            &[RGB; 2],
            false,
        );
        let (result, _) = run(
            6,
            per_led(3, &[[0; 4]; 2]),
            after,
            "GenericDevice",
            Some("Static"),
        );
        assert!(result.unwrap_err().contains("identity"));
    }
    #[test]
    fn correct_selector_only_needs_led_packet() {
        let (result, requests) = run(
            6,
            per_led(1, &[[0; 4]; 19]),
            per_led(1, &[RGB; 19]),
            "GenericDevice",
            None,
        );
        assert_eq!(result, Ok(true));
        assert_eq!(packets(&requests), vec![40, 1, 1050, 1]);
        assert_eq!(
            u16::from_le_bytes(requests[2].2[4..6].try_into().unwrap()),
            19
        );
        assert_eq!(
            word(&requests[2].2[..4]).unwrap() as usize,
            requests[2].2.len()
        );
    }
    #[test]
    fn preferred_mode_is_resolved_by_actual_advertised_name_not_index() {
        for preferred in ["Static", "Custom", "Direct"] {
            let a = mode("GenericEffect", 1 << 5, 1, 0, 0, &[]);
            let b = mode(preferred, 1 << 5, 1, 0, 0, &[]);
            let before = descriptor(
                "GenericDevice",
                &[a.clone(), b.clone()],
                0,
                &[RGB; 2],
                false,
            );
            let after = descriptor("GenericDevice", &[a, b.clone()], 1, &[RGB; 2], false);
            let (result, requests) = run(6, before, after, "GenericDevice", Some(preferred));
            assert_eq!(result, Ok(true));
            assert_eq!(packets(&requests), vec![40, 1, 1101, 1050, 1]);
            assert_eq!(&requests[2].2[4..8], &1u32.to_le_bytes());
            assert_eq!(&requests[2].2[8..], &b);
        }
    }
    #[test]
    fn absent_preference_retains_original_active_mode() {
        for preferred in [Some("Static"), None, Some("Invented")] {
            let m = mode("GenericEffect", 1 << 5, 1, 0, 0, &[]);
            let d = descriptor("GenericDevice", &[m], 0, &[RGB; 2], false);
            let (result, requests) = run(6, d.clone(), d, "GenericDevice", preferred);
            assert_eq!(result, Ok(true));
            assert_eq!(packets(&requests), vec![40, 1]);
        }
    }
    #[test]
    fn advertised_unsupported_preference_is_not_activated_or_replaced() {
        let d = descriptor(
            "GenericDevice",
            &[
                mode("Direct", 1 << 5, 1, 0, 0, &[]),
                mode("Static", 0, 0, 0, 0, &[]),
            ],
            0,
            &[RGB],
            false,
        );
        let (result, requests) = run(6, d.clone(), d, "GenericDevice", Some("Static"));
        assert_eq!(result, Ok(false));
        assert_eq!(packets(&requests), vec![40, 1]);
    }
    #[test]
    fn mode_specific_valid_state_does_not_touch_leds_or_modes() {
        let d = descriptor(
            "GenericDevice",
            &[mode("Static", 1 << 6, 2, 2, 4, &[RGB; 3])],
            0,
            &[[0; 4]; 2],
            false,
        );
        let (result, requests) = run(6, d.clone(), d, "GenericDevice", Some("Static"));
        assert_eq!(result, Ok(true));
        assert_eq!(packets(&requests), vec![40, 1]);
    }
    #[test]
    fn valid_mode_specific_selection_is_preserved_when_both_mechanisms_exist() {
        let d = descriptor(
            "GenericDevice",
            &[mode("Static", PER_LED | MODE_SPECIFIC, 2, 1, 4, &[RGB; 3])],
            0,
            &[[0; 4]; 2],
            false,
        );
        let (result, requests) = run(6, d.clone(), d, "GenericDevice", Some("Static"));
        assert_eq!(result, Ok(true));
        assert_eq!(packets(&requests), vec![40, 1]);
    }

    #[test]
    fn mode_specific_accepts_independent_led_and_color_counts() {
        // Named LEDs and color slots are independent protocol arrays.
        let mut d = descriptor(
            "GenericDevice",
            &[mode("Static", MODE_SPECIFIC, 2, 1, 1, &[RGB])],
            0,
            &[],
            false,
        );
        d.truncate(d.len() - 2);
        u16_to(&mut d, 2);
        d.extend_from_slice(&[0; 8]);
        let d = full_size(d);
        let (result, requests) = run(6, d.clone(), d, "GenericDevice", Some("Static"));
        assert_eq!(result, Ok(true));
        assert_eq!(packets(&requests), vec![40, 1]);
    }

    #[test]
    fn every_truncation_and_trailing_bytes_are_rejected() {
        let valid = descriptor(
            "GenericDevice",
            &[mode("Static", 1 << 5, 3, 0, 0, &[])],
            0,
            &[[0; 4]; 3],
            true,
        );
        for len in 0..valid.len() {
            let mut d = valid[..len].to_vec();
            if len >= 4 {
                d = full_size(d);
            }
            assert!(
                Descriptor::parse(&d).is_err(),
                "accepted truncated length {len}"
            );
        }
        let mut extra = valid.clone();
        extra.push(0);
        extra = full_size(extra);
        assert!(Descriptor::parse(&extra).is_err());
        let mut wrong_size = valid;
        wrong_size[..4].copy_from_slice(&4u32.to_le_bytes());
        assert!(Descriptor::parse(&wrong_size).is_err());
    }
    #[test]
    fn malformed_network_metadata_cannot_fall_back_or_write() {
        let good = per_led(3, &[[0; 4]; 3]);
        for bad in [
            vec![],
            vec![0; 4],
            good[..good.len() - 1].to_vec(),
            vec![0; MAX_METADATA + 1],
        ] {
            let (result, requests) = run(6, bad, good.clone(), "GenericDevice", Some("Static"));
            assert!(result.is_err());
            assert_eq!(packets(&requests), vec![40, 1]);
        }
    }
    #[test]
    fn invalid_color_is_rejected_before_connecting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        for color in [
            "", "12345", "1234567", "##123456", "12zzab", " 1234ab", "é1234",
        ] {
            let result = finalize_solid(
                listener.local_addr().unwrap(),
                7,
                "GenericDevice",
                color,
                None,
                Duration::from_millis(10),
            );
            assert!(result.unwrap_err().contains("six hexadecimal"));
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
        assert_eq!(parse_color("#1234AB").unwrap(), RGB);
    }
    #[test]
    fn malformed_matrix_string_counts_and_mode_index_are_errors() {
        let good = per_led(3, &[[0; 4]; 2]);
        let mut unterminated = good.clone();
        unterminated[8 + 2 + "GenericDevice".len()] = b'X';
        assert!(Descriptor::parse(&unterminated).is_err());
        let mut embedded = good.clone();
        embedded[10] = 0;
        assert!(Descriptor::parse(&embedded).is_err());
        let index = descriptor(
            "GenericDevice",
            &[mode("Static", 1 << 5, 3, 0, 0, &[])],
            2,
            &[RGB],
            false,
        );
        assert!(Descriptor::parse(&index).is_err());
        let mut bad_colors = good.clone();
        let at = bad_colors.len() - 2 * 4 - 2;
        bad_colors[at..at + 2].copy_from_slice(&1u16.to_le_bytes());
        assert!(Descriptor::parse(&bad_colors).is_err());
        let mut matrix = descriptor("GenericDevice", &[], 0, &[RGB; 2], true);
        let marker = 2u32.to_le_bytes();
        let pos = matrix
            .windows(18)
            .position(|w| {
                w[0..2] == 16u16.to_le_bytes()
                    && w[2..6] == 1u32.to_le_bytes()
                    && w[6..10] == marker
            })
            .unwrap();
        matrix[pos + 6..pos + 10].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Descriptor::parse(&matrix).is_err());
    }

    #[test]
    fn per_led_repair_preserves_even_unused_mode_colors() {
        let old = mode("Static", (1 << 5) | (1 << 6), 3, 1, 4, &[[8, 9, 10, 0]; 3]);
        let desired = mode("Static", (1 << 5) | (1 << 6), 1, 1, 4, &[[8, 9, 10, 0]; 3]);
        let before = descriptor("GenericDevice", &[old], 0, &[[0; 4]; 2], false);
        let after = descriptor(
            "GenericDevice",
            std::slice::from_ref(&desired),
            0,
            &[RGB; 2],
            false,
        );
        let (result, requests) = run(6, before, after, "GenericDevice", Some("Static"));
        assert_eq!(result, Ok(true));
        assert_eq!(&requests[2].2[8..], &desired);
    }
    #[test]
    fn ignored_mode_specific_write_fails_postcondition() {
        let d = descriptor(
            "GenericDevice",
            &[mode("Static", 1 << 6, 2, 1, 3, &[[0; 4]; 2])],
            0,
            &[RGB; 2],
            false,
        );
        let (result, requests) = run(6, d.clone(), d, "GenericDevice", Some("Static"));
        assert!(result.unwrap_err().contains("postcondition"));
        assert_eq!(packets(&requests), vec![40, 1, 1101, 1]);
    }
    #[test]
    fn readback_wrong_active_mode_cannot_succeed() {
        let a = mode("GenericEffect", 1 << 5, 1, 0, 0, &[]);
        let b = mode("Static", 1 << 5, 1, 0, 0, &[]);
        let d = descriptor("GenericDevice", &[a, b], 0, &[RGB; 2], false);
        let (result, requests) = run(6, d.clone(), d, "GenericDevice", Some("Static"));
        assert!(result.unwrap_err().contains("postcondition"));
        assert_eq!(packets(&requests), vec![40, 1, 1101, 1050, 1]);
    }
    #[test]
    fn larger_current_descriptors_do_not_use_the_old_64k_cap() {
        let before = per_led(1, &vec![[0; 4]; 5000]);
        assert!(before.len() > 65536);
        let (result, requests) = run(
            6,
            before,
            per_led(1, &vec![RGB; 5000]),
            "GenericDevice",
            None,
        );
        assert_eq!(result, Ok(true));
        assert_eq!(packets(&requests), vec![40, 1, 1050, 1]);
        assert_eq!(requests[2].2.len(), 4 + 2 + 5000 * 4);
    }
    #[test]
    fn no_active_or_global_mode_retains_cli_without_inventing_a_mode() {
        let d = descriptor(
            "GenericDevice",
            &[mode("Static", 1 << 5, 1, 0, 0, &[])],
            u32::MAX,
            &[RGB],
            false,
        );
        let (result, requests) = run(6, d.clone(), d, "GenericDevice", None);
        assert_eq!(result, Ok(false));
        assert_eq!(packets(&requests), vec![40, 1]);
    }
    #[test]
    fn unavailable_transport_is_an_error_not_unsupported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        assert!(finalize_solid(
            address,
            7,
            "GenericDevice",
            "1234ab",
            None,
            Duration::from_millis(100)
        )
        .is_err());
    }
    #[test]
    fn stalled_server_is_a_bounded_transport_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 20];
            socket.read_exact(&mut request).unwrap();
            assert_eq!(&request[8..12], &40u32.to_le_bytes());
            let mut eof = [0; 1];
            let _ = socket.read(&mut eof);
        });
        let result = finalize_solid(
            address,
            7,
            "GenericDevice",
            "1234ab",
            None,
            Duration::from_millis(30),
        );
        task.join().unwrap();
        assert!(result.is_err());
    }
}
