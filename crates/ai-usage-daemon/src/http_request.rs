//! Read one bounded HTTP request; every response closes its connection.
use std::io::{self, BufRead, BufReader, Read};

const MAX_HEADER_BYTES: u64 = 16 * 1024;
// Run-usage records are the largest bodies accepted by any route.
const MAX_BODY_BYTES: u64 = usagestat_core::run_usage::MAX_EVENT_BYTES as u64;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let mut values = self
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name));
        let (_, value) = values.next()?;
        // Ambiguous credentials must never authenticate.
        if values.next().is_some() {
            return None;
        }
        Some(value)
    }
}

pub fn read_request(reader: impl Read) -> io::Result<Request> {
    let mut reader = BufReader::new(reader.take(MAX_HEADER_BYTES + MAX_BODY_BYTES));
    let mut lines = Vec::new();
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        header_bytes += line.len() as u64;
        if !line.ends_with("\r\n") || header_bytes > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete HTTP headers",
            ));
        }
        line.truncate(line.len() - 2);
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }

    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP headers");
    let mut lines = lines.into_iter();
    let first = lines.next().ok_or_else(invalid)?;
    let parts: Vec<_> = first.split_whitespace().collect();
    if parts.len() != 3
        || !parts[1].starts_with('/')
        || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1")
    {
        return Err(invalid());
    }
    let path = parts[1].split('?').next().unwrap().trim_end_matches('/');
    let mut headers = Vec::new();
    for line in lines {
        let (key, value) = line.split_once(':').ok_or_else(invalid)?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.bytes().any(|b| (b < 32 && b != b'\t') || b == 127)
        {
            return Err(invalid());
        }
        headers.push((key.to_ascii_lowercase(), value.trim().to_string()));
    }
    // This server closes each connection; reject ambiguous framing and do not
    // implement chunked uploads for its small JSON management contract.
    let lengths: Vec<_> = headers
        .iter()
        .filter(|(key, _)| key == "content-length")
        .collect();
    if lengths.len() > 1
        || headers.iter().any(|(key, _)| {
            matches!(
                key.as_str(),
                "transfer-encoding" | "expect" | "content-encoding"
            )
        })
    {
        return Err(invalid());
    }
    let length = match lengths.first() {
        Some((_, value)) if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
            value.parse::<u64>().map_err(|_| invalid())?
        }
        Some(_) => return Err(invalid()),
        None => 0,
    };
    if length > MAX_BODY_BYTES {
        return Err(invalid());
    }
    let mut body = vec![0; length as usize];
    reader.read_exact(&mut body)?;
    Ok(Request {
        method: parts[0].to_string(),
        path: if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        },
        headers,
        body,
        query: parts[1]
            .split_once('?')
            .map_or("", |(_, query)| query)
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fragmented<'a>(&'a [u8]);
    impl Read for Fragmented<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let count = out.len().min(3);
            self.0.read(&mut out[..count])
        }
    }

    #[test]
    fn reads_fragmented_headers_and_normalizes_paths() {
        let request = read_request(Fragmented(b"GET /v0/management/quota-scheduler/status/?x=1 HTTP/1.1\r\naUtHoRiZaTiOn: Bearer test-key\r\n\r\n")).unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/v0/management/quota-scheduler/status");
        assert_eq!(request.query, "x=1");
        assert_eq!(request.header("authorization"), Some("Bearer test-key"));
    }

    #[test]
    fn rejects_incomplete_oversized_or_malformed_headers() {
        for input in [
            "GET / HTTP/1.1\r\nAuthorization: Bearer test-key\r\n".to_string(),
            format!(
                "GET / HTTP/1.1\r\nX-Large: {}\r\n\r\n",
                "x".repeat(MAX_HEADER_BYTES as usize)
            ),
            "GET / HTTP/1.1\r\nAuthorization Bearer test-key\r\n\r\n".to_string(),
            "GET / HTTP/1.1\r\nAuthorization: Bearer test-key\0\r\n\r\n".to_string(),
            "\r\n".to_string(),
        ] {
            assert!(read_request(input.as_bytes()).is_err());
        }
    }

    #[test]
    fn duplicate_credentials_are_ambiguous() {
        let request = read_request(&b"GET / HTTP/1.1\r\nAuthorization: Bearer one\r\nauthorization: Bearer two\r\n\r\n"[..]).unwrap();
        assert_eq!(request.header("authorization"), None);
    }

    #[test]
    fn reads_bounded_fragmented_bodies_and_rejects_ambiguous_framing() {
        let input = b"POST /v0/management/api-call HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(read_request(Fragmented(input)).unwrap().body, b"{}");
        for headers in [
            "Content-Length: 2\r\nContent-Length: 2\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Content-Length: 65537\r\n",
            "Content-Length: +2\r\n",
            "Content-Length: 3\r\n",
        ] {
            assert!(
                read_request(format!("POST / HTTP/1.1\r\n{headers}\r\n{{}}").as_bytes()).is_err()
            );
        }
    }

    #[test]
    fn retains_body_read_ahead_and_rejects_ambiguous_framing() {
        let bytes = b"POST /v1/run-usage HTTP/1.1\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        assert_eq!(read_request(&bytes[..]).unwrap().body, b"{\"a\":1}");
        assert_eq!(read_request(Fragmented(bytes)).unwrap().body, b"{\"a\":1}");
        for headers in [
            "Content-Length: 7\r\nContent-Length: 7",
            "Content-Length: +7",
            "Transfer-Encoding: chunked",
            "Content-Length: 65537",
            "Expect: 100-continue",
            "Content-Length: 9",
        ] {
            let input = format!("POST /v1/run-usage HTTP/1.1\r\n{headers}\r\n\r\n{{\"a\":1}}");
            assert!(read_request(input.as_bytes()).is_err(), "{headers}");
        }
    }
}
