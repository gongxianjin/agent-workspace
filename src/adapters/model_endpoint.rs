//! Trusted model endpoint for the Linux worker. The manager never talks to
//! the model itself; the call happens inside the Linux worker process
//! (user-confirmed design). TLS uses a root store containing exactly the one
//! pinned certificate: no proxy, no redirects, no TOFU, no weakened TLS.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;

/// Operator-staged endpoint identity. Credentials are deliberately absent:
/// the bearer token reaches the worker process only through in-memory
/// stdin frames and the child environment, never via this configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEndpoint {
    pub schema: String,
    pub base_url: String,
    pub model: String,
    pub certificate_pem: String,
    pub timeout_secs: u64,
    /// Optional explicit connect override ("ip:port"). The pinned leaf may
    /// carry a DNS name (boapi.uno) while public DNS resolves that name to an
    /// address that does not serve the pinned certificate (linux221 run
    /// 1152214d). TLS still connects to this staged address but SNI and name
    /// verification keep running against the URL hostname — a connect
    /// override, never a name-verification exemption. Absent on existing
    /// staged configs; never carries a credential.
    #[serde(default)]
    pub connect_addr: Option<String>,
}

impl ModelEndpoint {
    pub const MIN_TIMEOUT_SECS: u64 = 30;
    /// Upper bound for the total model-call timeout. Raised 120 -> 240 for
    /// linux221 run c0277b6f: a real generation observed at ~117 s left 3 s
    /// of headroom and the next run exceeded the cap ("operation timed
    /// out"). 240 s stays inside the 270 s gateway exchange window
    /// (240 s + ~10 s owner overhead < 270 s), so a slow call still yields
    /// a terminal verdict rather than a transport-unknown run.
    pub const MAX_TIMEOUT_SECS: u64 = 240;

    pub fn load(path: &std::path::Path) -> Result<Self> {
        use std::io::Read;
        let mut raw = Vec::new();
        std::fs::File::open(path)?.take(65537).read_to_end(&mut raw)?;
        ensure!(raw.len() <= 65536, "model endpoint config exceeds 64KiB");
        let config: ModelEndpoint = serde_json::from_slice(&raw)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == "model-endpoint/1", "unsupported model endpoint schema");
        let url = reqwest::Url::parse(&self.base_url).context("invalid model endpoint URL")?;
        ensure!(url.scheme() == "https", "model endpoint must be explicit https");
        ensure!(url.host_str().is_some(), "model endpoint host required");
        ensure!(
            url.username().is_empty() && url.password().is_none() && url.query().is_none()
                && url.fragment().is_none() && matches!(url.path(), "" | "/"),
            "model endpoint must be a bare origin without userinfo, path, query or fragment"
        );
        ensure!(!self.model.is_empty() && self.model.len() <= 128, "invalid model name");
        ensure!(
            (Self::MIN_TIMEOUT_SECS..=Self::MAX_TIMEOUT_SECS).contains(&self.timeout_secs),
            "model endpoint timeout must be 30..240 seconds"
        );
        ensure!(
            self.certificate_pem.starts_with("-----BEGIN CERTIFICATE-----")
                && self.certificate_pem.contains("-----END CERTIFICATE-----")
                && self.certificate_pem.len() <= 32768,
            "pinned certificate PEM required"
        );
        if let Some(connect) = &self.connect_addr {
            let addr: std::net::SocketAddr = connect
                .parse()
                .context("connect override must be an ip:port literal")?;
            ensure!(addr.is_ipv4(), "connect override must be an IPv4 literal");
        }
        Ok(())
    }

    /// Blocking client pinned to the single staged certificate. The staged
    /// PEM is a **leaf** certificate (the live endpoint presents a Let's
    /// Encrypt leaf; CA=false). A leaf cannot act as a trust anchor in a
    /// RootCertStore — chain building fails closed with UnknownIssuer
    /// (linux221 run 72eeb08f) — so pinning is enforced by a custom
    /// verifier comparing the presented end-entity certificate
    /// byte-for-byte with the staged pin. Handshake signature verification
    /// and server-name checks remain fully enforced.
    /// Linux-only: the model call never happens on the manager host.
    #[cfg(target_os = "linux")]
    pub fn pinned_client(&self) -> Result<reqwest::blocking::Client> {
        use std::sync::Arc;
        use std::time::Duration;
        self.validate()?;
        let mut pinned = Vec::new();
        for certificate in rustls::pki_types::pem::PemObject::pem_slice_iter(
            self.certificate_pem.as_bytes(),
        ) {
            pinned.push(certificate.context("invalid pinned certificate PEM section")?);
        }
        ensure!(!pinned.is_empty(), "pinned certificate contains no certificates");
        let verifier = Arc::new(LeafPinnedVerifier::new(pinned));
        let _ = rustls::crypto::ring::default_provider().install_default();
        // with_custom_certificate_verifier alone yields a ConfigBuilder in
        // WantsClientCert state; use_preconfigured_tls takes impl Any and
        // only accepts a finished ClientConfig — an unfinished builder
        // downcast-fails at runtime with UnknownPreconfigured. Terminate
        // the builder explicitly.
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        let client = reqwest::blocking::Client::builder()
            .use_preconfigured_tls(config)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(self.timeout_secs))
            .connect_timeout(Duration::from_secs(10));
        // Connect-by-IP, validate-by-name (linux221 run 1152214d): when the
        // pinned leaf's DNS name does not resolve to the address serving it,
        // override resolution for exactly that host. reqwest applies the
        // override only to connection establishment; SNI and certificate
        // name verification still run against the URL hostname, and the
        // byte-for-byte pin still gates the handshake.
        let client = match &self.connect_addr {
            Some(connect) => {
                let url = reqwest::Url::parse(&self.base_url)
                    .context("invalid model endpoint URL")?;
                let host = url
                    .host_str()
                    .context("connect override requires a URL host")?;
                let addr: std::net::SocketAddr = connect
                    .parse()
                    .context("connect override must be an ip:port literal")?;
                client.resolve(host, addr)
            }
            None => client,
        };
        let client = client.build()?;
        Ok(client)
    }
}

/// Server certificate verifier that accepts exactly the pinned leaf
/// certificate(s), byte-for-byte, and refuses everything else. TLS handshake
/// signatures are still verified against the standard webpki algorithm set
/// (rustls aborts the connection otherwise); `dangerous()` here only
/// replaces the trust-anchor decision, which is precisely the pin.
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LeafPinnedVerifier {
    pinned: Vec<rustls::pki_types::CertificateDer<'static>>,
    supported: rustls::crypto::WebPkiSupportedAlgorithms,
}

#[cfg(target_os = "linux")]
impl LeafPinnedVerifier {
    fn new(pinned: Vec<rustls::pki_types::CertificateDer<'static>>) -> Self {
        Self {
            pinned,
            supported: rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        }
    }
}

#[cfg(target_os = "linux")]
impl rustls::client::danger::ServerCertVerifier for LeafPinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // Byte-for-byte pin comparison on the end-entity certificate.
        if !self.pinned.iter().any(|pinned| pinned.as_ref() == end_entity.as_ref()) {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ));
        }
        // The pin does not suspend name verification: the certificate must
        // still be valid for the URL host (SAN/CN match, IP literals
        // included). Expiry is the staged pin's own lifecycle concern — the
        // manager re-verifies the live certificate fingerprint at every
        // stage — not this handshake's.
        let parsed = rustls::server::ParsedCertificate::try_from(end_entity)
            .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        rustls::client::verify_server_name(&parsed, server_name)
            .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName))?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.supported,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.supported)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.supported.supported_schemes()
    }
}
