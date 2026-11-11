use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A real implementation of Minecraft's Server List Ping protocol
/// (handshake -> status request -> status response, then a timed ping/pong
/// for latency) — see https://minecraft.wiki/w/Java_Edition_protocol/Server_list_ping.
/// This is what every third-party launcher/server-list tool uses to show
/// live player counts and latency; there is no simpler "official API" for it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(4);

/// A fake protocol version is fine here: servers only validate it for the
/// login state, not for status pings, so any value works.
const PROTOCOL_VERSION: i32 = 767;

#[derive(Debug, Clone, Serialize)]
pub struct ServerPingInfo {
    pub online: bool,
    pub players_online: Option<u32>,
    pub players_max: Option<u32>,
    pub version_name: Option<String>,
    pub motd: Option<String>,
    pub latency_ms: Option<u32>,
    /// The server's icon, exactly as it sends it in the status response: a
    /// complete `data:image/png;base64,...` URI, usable directly as an
    /// `<img src>` with no further decoding.
    pub favicon: Option<String>,
    pub error: Option<String>,
}

impl ServerPingInfo {
    fn offline(error: impl Into<String>) -> Self {
        Self {
            online: false,
            players_online: None,
            players_max: None,
            version_name: None,
            motd: None,
            latency_ms: None,
            favicon: None,
            error: Some(error.into()),
        }
    }
}

fn split_host_port(address: &str) -> (String, u16) {
    match address.rsplit_once(':') {
        Some((host, port_str)) if !host.is_empty() => match port_str.parse::<u16>() {
            Ok(port) => (host.to_string(), port),
            Err(_) => (address.to_string(), 25565),
        },
        _ => (address.to_string(), 25565),
    }
}

pub async fn ping_server(address: &str) -> ServerPingInfo {
    match tokio::time::timeout(EXCHANGE_TIMEOUT, do_ping(address)).await {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => ServerPingInfo::offline(e),
        Err(_) => ServerPingInfo::offline("Timed out"),
    }
}

async fn do_ping(address: &str) -> Result<ServerPingInfo, String> {
    let (host, port) = split_host_port(address);

    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| "Connection timed out".to_string())?
        .map_err(|e| e.to_string())?;
    stream.set_nodelay(true).ok();

    // Handshake (0x00): protocol version, server address, port, next_state=1 (status)
    let mut handshake_body = Vec::new();
    write_varint(&mut handshake_body, PROTOCOL_VERSION);
    write_string(&mut handshake_body, &host);
    handshake_body.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut handshake_body, 1);
    write_packet(&mut stream, 0x00, &handshake_body).await.map_err(|e| e.to_string())?;

    // Status request (0x00, empty body)
    write_packet(&mut stream, 0x00, &[]).await.map_err(|e| e.to_string())?;

    let (_id, body) = read_packet(&mut stream).await.map_err(|e| e.to_string())?;
    let json_str = read_string_from_slice(&body).ok_or("Malformed status response")?;
    let json: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| e.to_string())?;

    let players_online = json.pointer("/players/online").and_then(|v| v.as_u64()).map(|v| v as u32);
    let players_max = json.pointer("/players/max").and_then(|v| v.as_u64()).map(|v| v as u32);
    let version_name = json.pointer("/version/name").and_then(|v| v.as_str()).map(|s| s.to_string());
    let motd = extract_motd(json.get("description"));
    let favicon = json.get("favicon").and_then(|v| v.as_str()).map(|s| s.to_string());

    // Ping/pong (0x01) for a real round-trip latency measurement.
    let token: i64 = 42;
    let mut ping_body = Vec::new();
    ping_body.extend_from_slice(&token.to_be_bytes());
    let start = Instant::now();
    write_packet(&mut stream, 0x01, &ping_body).await.map_err(|e| e.to_string())?;
    let (_id, pong_body) = read_packet(&mut stream).await.map_err(|e| e.to_string())?;
    let latency_ms = if pong_body.len() >= 8 {
        Some(start.elapsed().as_millis() as u32)
    } else {
        None
    };

    Ok(ServerPingInfo {
        online: true,
        players_online,
        players_max,
        version_name,
        motd,
        latency_ms,
        favicon,
        error: None,
    })
}

fn extract_motd(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?;
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    let mut text = String::new();
    if let Some(t) = value.get("text").and_then(|v| v.as_str()) {
        text.push_str(t);
    }
    if let Some(extra) = value.get("extra").and_then(|v| v.as_array()) {
        for part in extra {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                text.push_str(t);
            } else if let Some(t) = part.as_str() {
                text.push_str(t);
            }
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    write_varint(buf, s.len() as i32);
    buf.extend_from_slice(s.as_bytes());
}

async fn write_packet(stream: &mut TcpStream, packet_id: i32, body: &[u8]) -> std::io::Result<()> {
    let mut payload = Vec::new();
    write_varint(&mut payload, packet_id);
    payload.extend_from_slice(body);

    let mut framed = Vec::new();
    write_varint(&mut framed, payload.len() as i32);
    framed.extend_from_slice(&payload);

    stream.write_all(&framed).await
}

async fn read_varint_async(stream: &mut TcpStream) -> std::io::Result<i32> {
    let mut result: i32 = 0;
    let mut shift = 0u32;
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await?;
        result |= ((byte[0] & 0x7F) as i32) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 35 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "VarInt too long"));
        }
    }
    Ok(result)
}

async fn read_packet(stream: &mut TcpStream) -> std::io::Result<(i32, Vec<u8>)> {
    let length = read_varint_async(stream).await?;
    if !(0..=5_000_000).contains(&length) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid packet length"));
    }
    let mut buf = vec![0u8; length as usize];
    stream.read_exact(&mut buf).await?;

    let mut cursor = std::io::Cursor::new(&buf);
    let id = read_varint_sync(&mut cursor)?;
    let start = cursor.position() as usize;
    Ok((id, buf[start..].to_vec()))
}

fn read_varint_sync<R: std::io::Read>(cursor: &mut R) -> std::io::Result<i32> {
    let mut result: i32 = 0;
    let mut shift = 0u32;
    loop {
        let mut byte = [0u8; 1];
        cursor.read_exact(&mut byte)?;
        result |= ((byte[0] & 0x7F) as i32) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 35 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "VarInt too long"));
        }
    }
    Ok(result)
}

fn read_string_from_slice(data: &[u8]) -> Option<String> {
    let mut cursor = std::io::Cursor::new(data.to_vec());
    let len = read_varint_sync(&mut cursor).ok()?;
    let start = cursor.position() as usize;
    let end = start.checked_add(len as usize)?;
    data.get(start..end).map(|b| String::from_utf8_lossy(b).to_string())
}
