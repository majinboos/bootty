use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

pub(super) fn get_local(port: u16, path: &str, timeout: Duration) -> std::io::Result<String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    response_body(&response)
}

fn response_body(response: &[u8]) -> std::io::Result<String> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing headers"))?;
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let body = &response[header_end + 4..];
    let transfer_chunked = headers.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    });
    let bytes = if transfer_chunked {
        decode_chunked_body(body)?
    } else {
        body.to_vec()
    };
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn decode_chunked_body(body: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let Some(line_end) = find_crlf(&body[offset..]) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing chunk size",
            ));
        };
        let size_text = std::str::from_utf8(&body[offset..offset + line_end])
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let size = usize::from_str_radix(size_text.trim(), 16)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        offset += line_end + 2;
        if size == 0 {
            break;
        }
        if offset + size + 2 > body.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "short chunk body",
            ));
        }
        out.extend_from_slice(&body[offset..offset + size]);
        offset += size + 2;
    }
    Ok(out)
}

fn find_crlf(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|window| window == b"\r\n")
}
