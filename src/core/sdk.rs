//! Bounded SDK validation, rescan, and capability-checked solid-color finishing.
pub(super) mod solid;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

const TIMEOUT: &str = "OpenRGB SDK operation timed out";

struct Client {
    stream: TcpStream,
    deadline: Instant,
    received_bytes: usize,
}

impl Client {
    fn connect(address: SocketAddr, timeout: Duration) -> Result<Self, String> {
        let deadline = Instant::now() + timeout;
        let stream = TcpStream::connect_timeout(&address, timeout).map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stream,
            deadline,
            received_bytes: 0,
        })
    }

    fn send(&mut self, id: u32, body: &[u8]) -> Result<(), String> {
        self.send_to(0, id, body)
    }

    fn send_to(&mut self, device: u32, id: u32, body: &[u8]) -> Result<(), String> {
        if body.len() > 1_048_576 {
            return Err("OpenRGB SDK request exceeds size limit".to_string());
        }
        let mut packet = Vec::from(*b"ORGB");
        packet.extend_from_slice(&device.to_le_bytes());
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&(body.len() as u32).to_le_bytes());
        packet.extend_from_slice(body);
        let mut offset = 0;
        while offset < packet.len() {
            self.stream
                .set_write_timeout(Some(self.remaining()?))
                .map_err(|e| e.to_string())?;
            match self.stream.write(&packet[offset..]) {
                Ok(0) => return Err("OpenRGB SDK socket closed during write".to_string()),
                Ok(size) => offset += size,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Self::io_error(error)),
            }
        }
        Ok(())
    }

    fn expected_for(&mut self, device: u32, expected: u32) -> Result<Vec<u8>, String> {
        for _ in 0..128 {
            let (reply_device, id, body) = self.receive_frame()?;
            if id == expected {
                if reply_device != device {
                    return Err("OpenRGB SDK replied for a different controller".to_string());
                }
                return Ok(body);
            }
        }
        Err("Too many unrelated OpenRGB SDK replies".to_string())
    }

    fn remaining(&self) -> Result<Duration, String> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| TIMEOUT.to_string())
    }

    fn io_error(error: std::io::Error) -> String {
        match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => TIMEOUT.to_string(),
            _ => error.to_string(),
        }
    }

    fn read_exact_bounded(&mut self, mut bytes: &mut [u8]) -> Result<(), String> {
        while !bytes.is_empty() {
            self.stream
                .set_read_timeout(Some(self.remaining()?))
                .map_err(|e| e.to_string())?;
            match self.stream.read(bytes) {
                Ok(0) => return Err("OpenRGB SDK socket closed during read".to_string()),
                Ok(size) => {
                    self.received_bytes += size;
                    bytes = &mut bytes[size..];
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Self::io_error(error)),
            }
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<(u32, Vec<u8>), String> {
        let (device, id, body) = self.receive_frame()?;
        if device != 0 {
            return Err("Invalid OpenRGB SDK reply".to_string());
        }
        Ok((id, body))
    }

    fn receive_frame(&mut self) -> Result<(u32, u32, Vec<u8>), String> {
        let mut header = [0; 16];
        self.read_exact_bounded(&mut header)?;
        if &header[..4] != b"ORGB" {
            return Err("Invalid OpenRGB SDK reply".to_string());
        }
        let device = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let id = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let size = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        if size > 1_048_576 {
            return Err("OpenRGB SDK reply exceeds size limit".to_string());
        }
        let mut body = vec![0; size];
        self.read_exact_bounded(&mut body)?;
        Ok((device, id, body))
    }

    fn expected(&mut self, expected: u32) -> Result<Vec<u8>, String> {
        for _ in 0..128 {
            let (id, body) = self.receive()?;
            if id == expected {
                return Ok(body);
            }
        }
        Err("Too many unrelated OpenRGB SDK replies".to_string())
    }
}

fn word(body: &[u8]) -> Result<u32, String> {
    let bytes: [u8; 4] = body.try_into().map_err(|_| "Invalid OpenRGB SDK value")?;
    Ok(u32::from_le_bytes(bytes))
}

pub fn probe(address: SocketAddr, timeout: Duration) -> Result<(), String> {
    let mut client = Client::connect(address, timeout)?;
    client.send(0, &[])?;
    word(&client.expected(0)?)?;
    Ok(())
}

pub fn rescan(address: SocketAddr, timeout: Duration) -> Result<(), String> {
    let mut client = Client::connect(address, timeout)?;
    client.send(40, &6u32.to_le_bytes())?;
    if word(&client.expected(40)?)? < 6 {
        return Err(
            "This external OpenRGB server does not advertise safe rescanning; rescan in OpenRGB"
                .to_string(),
        );
    }
    client.send(52, &1u32.to_le_bytes())?;
    if word(&client.expected(53)?)? & (1 << 5) == 0 {
        return Err(
            "This external OpenRGB server does not support hardware rescanning".to_string(),
        );
    }
    client.send(140, &[])?;
    let mut started = false;
    for _ in 0..4096 {
        let (id, body) = client.receive()?;
        match id {
            101 => started = true,
            103 if started => return Ok(()),
            10 if body.len() == 8
                && body[..4] == 140u32.to_le_bytes()
                && word(&body[4..])? != 0 =>
            {
                return Err("OpenRGB rejected the hardware rescan".to_string());
            }
            _ => {}
        }
    }
    Err("OpenRGB did not confirm rescan completion".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn sdk_deadline_is_absolute_across_partial_payload_reads() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let header = [b"ORGB".as_slice(), &[0; 8], &4u32.to_le_bytes()].concat();
            socket.write_all(&header).unwrap();
            for byte in 1u32.to_le_bytes() {
                std::thread::sleep(Duration::from_millis(100));
                if socket.write_all(&[byte]).is_err() {
                    break;
                }
            }
        });
        let result = (|| {
            let mut client = Client::connect(address, Duration::from_millis(300))?;
            client.expected(0)
        })();
        peer.join().unwrap();
        assert!(
            result.is_err(),
            "partial reads must not restart the deadline: {result:?}"
        );
    }

    #[test]
    fn device_packet_roundtrip_uses_the_requested_controller() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 20];
            if socket.read_exact(&mut request).is_err() {
                return None;
            }
            let response = [
                b"ORGB".as_slice(),
                &7u32.to_le_bytes(),
                &1u32.to_le_bytes(),
                &4u32.to_le_bytes(),
                &42u32.to_le_bytes(),
            ]
            .concat();
            socket.write_all(&response).unwrap();
            Some(request)
        });
        let result = (|| {
            let mut client = Client::connect(address, Duration::from_secs(1))?;
            client.send_to(7, 1, &3u32.to_le_bytes())?;
            client.expected_for(7, 1)
        })();
        let request = peer.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(result.unwrap(), 42u32.to_le_bytes());
        let request = request.unwrap();
        assert_eq!(&request[..4], b"ORGB");
        assert_eq!(&request[4..8], &7u32.to_le_bytes());
        assert_eq!(&request[8..12], &1u32.to_le_bytes());
        assert_eq!(&request[12..16], &4u32.to_le_bytes());
        assert_eq!(&request[16..], &3u32.to_le_bytes());
    }

    fn rescan_peer(flags: u32) -> (SocketAddr, std::thread::JoinHandle<Vec<u32>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut requests = Vec::new();
            loop {
                let mut header = [0; 16];
                if socket.read_exact(&mut header).is_err() {
                    break;
                }
                assert_eq!(&header[..4], b"ORGB");
                let packet = u32::from_le_bytes(header[8..12].try_into().unwrap());
                let size = u32::from_le_bytes(header[12..].try_into().unwrap()) as usize;
                let mut body = vec![0; size];
                socket.read_exact(&mut body).unwrap();
                requests.push(packet);
                let replies = match packet {
                    40 => vec![(40u32, 6u32.to_le_bytes().to_vec())],
                    52 => vec![(53, flags.to_le_bytes().to_vec())],
                    140 => vec![
                        (10, [140u32.to_le_bytes(), 0u32.to_le_bytes()].concat()),
                        (101, vec![]),
                        (103, vec![]),
                    ],
                    _ => panic!("Unexpected request {packet}"),
                };
                for (id, body) in replies {
                    let mut response = Vec::from(*b"ORGB");
                    response.extend_from_slice(&0u32.to_le_bytes());
                    response.extend_from_slice(&id.to_le_bytes());
                    response.extend_from_slice(&(body.len() as u32).to_le_bytes());
                    response.extend_from_slice(&body);
                    socket.write_all(&response).unwrap();
                }
                if packet == 140 {
                    break;
                }
            }
            requests
        });
        (address, handle)
    }

    #[test]
    fn rescan_negotiates_capabilities_and_waits_for_detection_completion() {
        let (address, peer) = rescan_peer(1 << 5);
        let result = rescan(address, Duration::from_secs(1));
        if result.is_err() {
            let _ = TcpStream::connect(address);
        }
        let requests = peer.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(requests, vec![40, 52, 140]);
    }

    #[test]
    fn rescan_does_not_mutate_servers_without_the_detection_capability() {
        let (address, peer) = rescan_peer(0);
        let result = rescan(address, Duration::from_secs(1));
        if result.is_err() {
            let _ = TcpStream::connect(address);
        }
        let requests = peer.join().unwrap();
        assert!(result.is_err());
        assert_eq!(requests, vec![40, 52]);
    }

    #[test]
    fn probe_requires_an_openrgb_count_reply_not_just_an_open_port() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 16];
            if socket.read_exact(&mut request).is_ok() {
                assert_eq!(&request[..4], b"ORGB");
                assert_eq!(&request[4..], &[0; 12]);
                socket
                    .write_all(b"ORGB\0\0\0\0\0\0\0\0\x04\0\0\0\x02\0\0\0")
                    .unwrap();
            }
        });
        let result = probe(address, Duration::from_millis(200));
        // Connect even on RED so the inert peer cannot remain stuck in accept.
        if result.is_err() {
            let _ = std::net::TcpStream::connect(address);
        }
        peer.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
    }
}
