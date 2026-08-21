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
}
