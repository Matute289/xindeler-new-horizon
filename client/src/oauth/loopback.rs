use crate::oauth::{OAuthDeliveryMode, OAuthFailure};
use std::{
    io::{ErrorKind, Read, Write},
    net::TcpListener,
    thread::sleep,
    time::{Duration, Instant},
};

/// Static body served to the browser after the redirect lands. Kept plain and
/// dependency-free -- this is the only page this listener will ever serve.
pub const CALLBACK_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Xindeler</title><p \
                                 style=\"font-family:sans-serif\">Sign-in complete. You can close \
                                 this tab and return to the game.</p>";

/// How long to wait between non-blocking `accept` attempts. Short enough that
/// cancel feels immediate, long enough not to spin a core.
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Cap on the bytes read from the browser before giving up. A real redirect
/// request line is well under 1 KiB; this only bounds a hostile peer.
const MAX_REQUEST_BYTES: usize = 8192;

pub struct LoopbackListener {
    listener: TcpListener,
}

impl LoopbackListener {
    pub fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener })
    }

    pub fn port(&self) -> u16 { self.listener.local_addr().map(|a| a.port()).unwrap_or(0) }

    /// Accepts connections until one of them is the expected redirect, then
    /// answers it and drops the socket. Consumes `self` so the port cannot
    /// stay bound past the one request it exists to serve (spec §3.3).
    ///
    /// Non-blocking + sleep rather than a blocking `accept`: this runs inside
    /// `spawn_blocking`, which tokio cannot abort, so the only way cancel and
    /// the 5-minute deadline can take effect is to check them between polls.
    pub fn wait_for_pickup(
        self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String, OAuthFailure> {
        loop {
            if cancelled() {
                return Err(OAuthFailure::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(OAuthFailure::Timeout);
            }

            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    if stream.set_nonblocking(false).is_err() {
                        continue;
                    }
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

                    let mut buf = [0u8; MAX_REQUEST_BYTES];
                    let read = stream.read(&mut buf).unwrap_or(0);
                    let request_line = String::from_utf8_lossy(&buf[..read])
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();

                    let Some(pickup) = super::loopback::parse_callback_request(&request_line)
                    else {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: \
                              close\r\n\r\n",
                        );
                        continue;
                    };

                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; \
                         charset=utf-8\r\nContent-Length: {}\r\nConnection: \
                         close\r\n\r\n{CALLBACK_PAGE}",
                        CALLBACK_PAGE.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                    return Ok(pickup);
                },
                Err(e) if e.kind() == ErrorKind::WouldBlock => sleep(ACCEPT_POLL_INTERVAL),
                Err(e) => return Err(OAuthFailure::Other(e.to_string())),
            }
        }
    }
}

pub enum Delivery {
    Loopback(LoopbackListener),
    Poll,
}

impl Delivery {
    pub fn mode(&self) -> OAuthDeliveryMode {
        match self {
            Delivery::Loopback(_) => OAuthDeliveryMode::Loopback,
            Delivery::Poll => OAuthDeliveryMode::Poll,
        }
    }
}

/// Bind failure is expected and non-fatal (firewall, sandbox, hardened
/// desktop) -- the whole point of the polling fallback (spec §2.2).
pub fn choose_delivery(bind_result: std::io::Result<LoopbackListener>) -> Delivery {
    match bind_result {
        Ok(listener) => Delivery::Loopback(listener),
        Err(e) => {
            tracing::warn!(
                ?e,
                "loopback bind failed, falling back to OAuth polling mode"
            );
            Delivery::Poll
        },
    }
}

/// A pickup code is server-minted base64url. Anything longer is not something
/// this client's own auth server produced, so it is rejected before it can be
/// echoed anywhere.
pub const MAX_PICKUP_LEN: usize = 128;

/// Parses the single request line the browser sends to the ephemeral loopback
/// listener. Deliberately not a general HTTP parser: exactly one request shape
/// is ever expected here (spec §3.2 step 2), and anything else is a squatter,
/// a port scanner, or a stray browser prefetch -- all of which get `None`.
pub fn parse_callback_request(request_line: &str) -> Option<String> {
    let mut parts = request_line.split(' ');
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = target.split_once('?')?;
    if path != "/callback" {
        return None;
    }

    let pickup = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("pickup="))?;

    if pickup.is_empty()
        || pickup.len() > MAX_PICKUP_LEN
        || !pickup
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }

    Some(pickup.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_browser_redirect_get() {
        assert_eq!(
            parse_callback_request("GET /callback?pickup=abc123 HTTP/1.1"),
            Some("abc123".to_owned())
        );
    }

    #[test]
    fn accepts_http_1_0_and_extra_query_params_in_any_order() {
        assert_eq!(
            parse_callback_request("GET /callback?foo=1&pickup=xY-_9&bar=2 HTTP/1.0"),
            Some("xY-_9".to_owned())
        );
    }

    #[test]
    fn rejects_the_wrong_method() {
        assert_eq!(
            parse_callback_request("POST /callback?pickup=abc HTTP/1.1"),
            None
        );
    }

    #[test]
    fn rejects_the_wrong_path() {
        assert_eq!(parse_callback_request("GET /?pickup=abc HTTP/1.1"), None);
        assert_eq!(
            parse_callback_request("GET /callbackx?pickup=abc HTTP/1.1"),
            None
        );
    }

    #[test]
    fn rejects_a_missing_or_empty_pickup() {
        assert_eq!(parse_callback_request("GET /callback HTTP/1.1"), None);
        assert_eq!(
            parse_callback_request("GET /callback?pickup= HTTP/1.1"),
            None
        );
        assert_eq!(
            parse_callback_request("GET /callback?other=1 HTTP/1.1"),
            None
        );
    }

    #[test]
    fn rejects_a_pickup_that_is_not_base64url() {
        assert_eq!(
            parse_callback_request("GET /callback?pickup=a%20b HTTP/1.1"),
            None
        );
        assert_eq!(
            parse_callback_request("GET /callback?pickup=../../etc/passwd HTTP/1.1"),
            None
        );
    }

    #[test]
    fn rejects_an_absurdly_long_pickup() {
        let long = "a".repeat(MAX_PICKUP_LEN + 1);
        assert_eq!(
            parse_callback_request(&format!("GET /callback?pickup={long} HTTP/1.1")),
            None
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_callback_request(""), None);
        assert_eq!(parse_callback_request("GET"), None);
        assert_eq!(parse_callback_request("\0\0\0"), None);
    }

    use std::{
        io::{Read, Write},
        net::TcpStream,
        time::{Duration, Instant},
    };

    fn never_cancelled() -> impl Fn() -> bool { || false }

    #[test]
    fn bind_failure_falls_back_to_poll_mode() {
        let failed = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        let delivery = choose_delivery(failed);
        assert_eq!(delivery.mode(), OAuthDeliveryMode::Poll);
        assert!(matches!(delivery, Delivery::Poll));
    }

    #[test]
    fn successful_bind_stays_in_loopback_mode() {
        let delivery = choose_delivery(LoopbackListener::bind());
        assert_eq!(delivery.mode(), OAuthDeliveryMode::Loopback);
        let Delivery::Loopback(listener) = delivery else {
            panic!("expected loopback delivery");
        };
        assert_ne!(listener.port(), 0);
    }

    #[test]
    fn listener_receives_a_redirect_and_answers_with_the_static_page() {
        let listener = LoopbackListener::bind().expect("bind 127.0.0.1:0");
        let port = listener.port();

        let browser = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            stream
                .write_all(b"GET /callback?pickup=tok-123 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                .expect("write");
            let mut response = String::new();
            stream.read_to_string(&mut response).expect("read");
            response
        });

        let pickup = listener
            .wait_for_pickup(Instant::now() + Duration::from_secs(5), &never_cancelled())
            .expect("pickup");
        assert_eq!(pickup, "tok-123");

        let response = browser.join().expect("browser thread");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains(CALLBACK_PAGE));
    }

    #[test]
    fn listener_times_out_when_nothing_ever_connects() {
        let listener = LoopbackListener::bind().expect("bind 127.0.0.1:0");
        let err = listener
            .wait_for_pickup(
                Instant::now() + Duration::from_millis(150),
                &never_cancelled(),
            )
            .expect_err("should time out");
        assert!(matches!(err, OAuthFailure::Timeout));
    }

    #[test]
    fn listener_gives_up_promptly_when_cancelled() {
        let listener = LoopbackListener::bind().expect("bind 127.0.0.1:0");
        let started = Instant::now();
        let err = listener
            .wait_for_pickup(Instant::now() + Duration::from_secs(300), &|| true)
            .expect_err("should cancel");
        assert!(matches!(err, OAuthFailure::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn listener_rejects_a_squatter_sending_a_bogus_request() {
        let listener = LoopbackListener::bind().expect("bind 127.0.0.1:0");
        let port = listener.port();

        std::thread::spawn(move || {
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = stream.write_all(b"GET /evil HTTP/1.1\r\n\r\n");
                let mut sink = Vec::new();
                let _ = stream.read_to_end(&mut sink);
            }
        });

        let err = listener
            .wait_for_pickup(
                Instant::now() + Duration::from_millis(400),
                &never_cancelled(),
            )
            .expect_err("bogus request must not yield a pickup");
        assert!(matches!(err, OAuthFailure::Timeout));
    }
}
