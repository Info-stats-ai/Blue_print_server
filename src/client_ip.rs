//! Who a request counts as for rate limiting. Behind Railway's proxy every
//! connection comes from the proxy, so limiting by peer address would put all
//! users in one bucket. There the proxy names the client in `X-Real-IP`; that
//! header is only trusted on Railway, since anyone can send it directly.

use std::net::{IpAddr, SocketAddr};

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use tower_governor::GovernorError;
use tower_governor::key_extractor::KeyExtractor;

#[derive(Clone, Copy)]
pub struct ClientIp {
    behind_proxy: bool,
}

impl ClientIp {
    pub fn from_env() -> Self {
        let behind_proxy = ["RAILWAY_ENVIRONMENT", "RAILWAY_ENVIRONMENT_ID", "RAILWAY_PROJECT_ID", "RAILWAY_PUBLIC_DOMAIN"]
            .iter()
            .any(|name| std::env::var_os(name).is_some());
        println!("rate limits: by {}", if behind_proxy { "X-Real-IP" } else { "peer address" });
        Self { behind_proxy }
    }

    fn resolve(self, headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
        let forwarded = self
            .behind_proxy
            .then(|| headers.get("x-real-ip")?.to_str().ok()?.trim().parse().ok())
            .flatten();
        forwarded.or(peer)
    }
}

impl KeyExtractor for ClientIp {
    type Key = IpAddr;

    fn extract<T>(&self, request: &Request<T>) -> Result<Self::Key, GovernorError> {
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(address)| address.ip());
        self.resolve(request.headers(), peer).ok_or(GovernorError::UnableToExtractKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxy_header_only_counts_behind_the_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "203.0.113.7".parse().unwrap());
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let client: IpAddr = "203.0.113.7".parse().unwrap();

        assert_eq!(ClientIp { behind_proxy: true }.resolve(&headers, Some(peer)), Some(client));
        assert_eq!(ClientIp { behind_proxy: false }.resolve(&headers, Some(peer)), Some(peer));
        assert_eq!(ClientIp { behind_proxy: true }.resolve(&HeaderMap::new(), Some(peer)), Some(peer));
    }
}
