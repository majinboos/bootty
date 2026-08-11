use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_RESPONSE_BODY_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_HEADER_BYTES: usize = 8 * 1024;

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
    stream
        .take((MAX_RESPONSE_BODY_BYTES + MAX_RESPONSE_HEADER_BYTES + 1) as u64)
        .read_to_end(&mut response)?;
    response_body(&response)
}
/// Local HTTP request that observes extension shutdown while connecting and reading. The ordinary
/// helper intentionally waits for a complete response; refresh workers need a pollable socket so
/// host teardown cannot leave a thread blocked for the provider timeout.
pub(super) fn get_local_cancellable(
    port: u16,
    path: &str,
    timeout: Duration,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(20);
    'request: loop {
        if shutdown.load(Ordering::Acquire) {
            return Err(std::io::Error::other("extension host stopped"));
        }
        let mut stream = loop {
            if shutdown.load(Ordering::Acquire) {
                return Err(std::io::Error::other("extension host stopped"));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return if shutdown.load(Ordering::Acquire) {
                    Err(std::io::Error::other("extension host stopped"))
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "local HTTP request timed out",
                    ))
                };
            }
            match TcpStream::connect_timeout(&address, remaining.min(poll)) {
                Ok(stream) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(error) => {
                    return if shutdown.load(Ordering::Acquire) {
                        Err(std::io::Error::other("extension host stopped"))
                    } else {
                        Err(error)
                    };
                }
            }
        };
        if let Err(error) = stream.set_read_timeout(Some(poll)) {
            return if shutdown.load(Ordering::Acquire) {
                Err(std::io::Error::other("extension host stopped"))
            } else {
                Err(error)
            };
        }
        if let Err(error) = stream.set_write_timeout(Some(poll)) {
            return if shutdown.load(Ordering::Acquire) {
                Err(std::io::Error::other("extension host stopped"))
            } else {
                Err(error)
            };
        }
        if let Err(error) = write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        ) {
            if is_retryable_connection_error(&error)
                && !shutdown.load(Ordering::Acquire)
                && Instant::now() < deadline
            {
                std::thread::sleep(poll);
                continue 'request;
            }
            return if shutdown.load(Ordering::Acquire) {
                Err(std::io::Error::other("extension host stopped"))
            } else {
                Err(error)
            };
        }
        let mut response = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            if shutdown.load(Ordering::Acquire) {
                return Err(std::io::Error::other("extension host stopped"));
            }
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(size) => {
                    if shutdown.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("extension host stopped"));
                    }
                    if response.len().saturating_add(size)
                        > MAX_RESPONSE_BODY_BYTES + MAX_RESPONSE_HEADER_BYTES
                    {
                        return Err(std::io::Error::other("HTTP response body too large"));
                    }
                    response.extend_from_slice(&chunk[..size]);
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::TimedOut
                        || error.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    if Instant::now() >= deadline {
                        return if shutdown.load(Ordering::Acquire) {
                            Err(std::io::Error::other("extension host stopped"))
                        } else {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "local HTTP request timed out",
                            ))
                        };
                    }
                }
                Err(error) if is_retryable_connection_error(&error) => {
                    if shutdown.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("extension host stopped"));
                    }
                    if Instant::now() < deadline {
                        std::thread::sleep(poll);
                        continue 'request;
                    }
                    return Err(error);
                }
                Err(error) => {
                    return if shutdown.load(Ordering::Acquire) {
                        Err(std::io::Error::other("extension host stopped"))
                    } else {
                        Err(error)
                    };
                }
            }
        }
        if shutdown.load(Ordering::Acquire) {
            return Err(std::io::Error::other("extension host stopped"));
        }
        return response_body_inner(&response);
    }
}
fn is_retryable_connection_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
    )
}

#[cfg(test)]
pub(super) fn response_body(response: &[u8]) -> std::io::Result<String> {
    response_body_inner(response)
}

#[cfg(not(test))]
fn response_body(response: &[u8]) -> std::io::Result<String> {
    response_body_inner(response)
}

fn response_body_inner(response: &[u8]) -> std::io::Result<String> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing headers"))?;
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let transfer_chunked = headers.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    });
    let body = &response[header_end + 4..];
    let bytes = if transfer_chunked {
        decode_chunked_body(body)?
    } else {
        if body.len() > MAX_RESPONSE_BODY_BYTES {
            return Err(response_too_large());
        }
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
        if out.len().saturating_add(size) > MAX_RESPONSE_BODY_BYTES {
            return Err(response_too_large());
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

fn response_too_large() -> std::io::Error {
    std::io::Error::other("HTTP response body too large")
}

#[cfg(test)]
mod tests {
    use std::{
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    use super::{MAX_RESPONSE_BODY_BYTES, get_local_cancellable, response_body};
    fn response(body: &[u8]) -> Vec<u8> {
        let mut response = b"HTTP/1.1 200 OK\r\nContent-Length: ".to_vec();
        response.extend_from_slice(body.len().to_string().as_bytes());
        response.extend_from_slice(b"\r\n\r\n");
        response.extend_from_slice(body);
        response
    }

    #[test]
    fn response_body_accepts_exact_cap() {
        let body = vec![b'x'; MAX_RESPONSE_BODY_BYTES];
        assert_eq!(
            response_body(&response(&body)).unwrap().len(),
            MAX_RESPONSE_BODY_BYTES
        );
    }

    #[test]
    fn response_body_rejects_cap_plus_one() {
        let body = vec![b'x'; MAX_RESPONSE_BODY_BYTES + 1];
        let error = response_body(&response(&body)).unwrap_err();
        assert_eq!(error.to_string(), "HTTP response body too large");
    }
    #[test]
    fn cancellable_local_request_observes_shutdown() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let port = listener.local_addr().expect("listener address").port();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let _ = listener.accept();
            accepted_tx.send(()).expect("report accepted request");
            thread::sleep(Duration::from_millis(200));
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let request_shutdown = Arc::clone(&shutdown);
        let started = Instant::now();
        let request = thread::spawn(move || {
            get_local_cancellable(port, "/stalled", Duration::from_secs(35), &request_shutdown)
        });
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("request accepted");
        thread::sleep(Duration::from_millis(50));
        shutdown.store(true, Ordering::Release);
        let error = request
            .join()
            .expect("request")
            .expect_err("request cancelled");
        assert_eq!(error.to_string(), "extension host stopped");
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().expect("server");
    }
}
