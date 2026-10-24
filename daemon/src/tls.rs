//! TLS to a Cast receiver's control port. Receivers present self-signed
//! certificates, so nothing is checked against a trust store; the handshake
//! signature still is, against the key in the certificate.

use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls13_signature_with_raw_key};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, PeerMisbehaved,
    SignatureScheme, StreamOwned,
};

pub type Stream = StreamOwned<ClientConnection, TcpStream>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

const SEQUENCE: u8 = 0x30;
const BIT_STRING: u8 = 0x03;
const EXPLICIT_VERSION: u8 = 0xa0;

const BAD_ENCODING: rustls::Error =
    rustls::Error::InvalidCertificate(CertificateError::BadEncoding);

pub fn connect(addr: IpAddr, port: u16, read_timeout: Option<Duration>) -> Result<Stream> {
    let socket_addr = SocketAddr::new(addr, port);
    let tcp = TcpStream::connect_timeout(&socket_addr, CONNECT_TIMEOUT)
        .with_context(|| format!("connecting to {socket_addr}"))?;
    tcp.set_read_timeout(read_timeout)?;
    tcp.set_nodelay(true)?;

    let builder = ClientConfig::builder();
    let verifier = AnyCertificate(builder.crypto_provider().signature_verification_algorithms);
    let config = builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let connection = ClientConnection::new(Arc::new(config), ServerName::from(addr))
        .context("creating TLS connection")?;
    Ok(StreamOwned::new(connection, tcp))
}

#[derive(Debug)]
struct AnyCertificate(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let spki = subject_public_key_info(cert).ok_or(BAD_ENCODING)?;
        verify_tls12_signature(&self.0, message, spki, dss.scheme, dss.signature())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let spki = subject_public_key_info(cert).ok_or(BAD_ENCODING)?;
        verify_tls13_signature_with_raw_key(message, &spki.into(), dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// rustls has no raw-key variant for TLS 1.2, so this mirrors its own check.
fn verify_tls12_signature(
    algorithms: &WebPkiSupportedAlgorithms,
    message: &[u8],
    spki: &[u8],
    scheme: SignatureScheme,
    signature: &[u8],
) -> Result<HandshakeSignatureValid, rustls::Error> {
    let (key_algorithm, key) = public_key(spki).ok_or(BAD_ENCODING)?;
    let candidates = algorithms
        .mapping
        .iter()
        .find_map(|(s, algs)| (*s == scheme).then_some(*algs))
        .ok_or(PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme)?;
    candidates
        .iter()
        .filter(|alg| *alg.public_key_alg_id() == *key_algorithm)
        .any(|alg| alg.verify_signature(key, message, signature).is_ok())
        .then(HandshakeSignatureValid::assertion)
        .ok_or(rustls::Error::InvalidCertificate(
            CertificateError::BadSignature,
        ))
}

/// rustls' own checks parse the whole certificate with webpki, which rejects
/// X.509 v1 - what LG webOS TVs present - though only the key is needed.
fn subject_public_key_info(cert: &[u8]) -> Option<&[u8]> {
    let (cert, _) = element(cert, SEQUENCE)?;
    let (mut tbs, _) = element(cert, SEQUENCE)?;
    if tbs.first() == Some(&EXPLICIT_VERSION) {
        tbs = split(tbs)?.2;
    }
    // serialNumber, signature, issuer, validity, subject
    for _ in 0..5 {
        tbs = split(tbs)?.2;
    }
    let (tag, _, rest) = split(tbs)?;
    let spki = tbs.get(..tbs.len().checked_sub(rest.len())?)?;
    (tag == SEQUENCE).then_some(spki)
}

/// The `AlgorithmIdentifier` contents and key bits of a `SubjectPublicKeyInfo`.
fn public_key(spki: &[u8]) -> Option<(&[u8], &[u8])> {
    let (spki, _) = element(spki, SEQUENCE)?;
    let (algorithm, rest) = element(spki, SEQUENCE)?;
    let (bits, _) = element(rest, BIT_STRING)?;
    match bits.split_first()? {
        (0, key) => Some((algorithm, key)),
        _ => None,
    }
}

fn element(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (found, contents, rest) = split(input)?;
    (found == tag).then_some((contents, rest))
}

/// Splits the first DER element off `input` into tag, contents and the rest.
fn split(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let (bytes, rest) = rest.split_at_checked(usize::from(first & 0x7f))?;
        if bytes.is_empty() {
            return None;
        }
        let mut len = 0_usize;
        for &byte in bytes {
            len = len.checked_mul(256)?.checked_add(usize::from(byte))?;
        }
        (len, rest)
    };
    let (contents, rest) = rest.split_at_checked(len)?;
    Some((tag, contents, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    // One P-256 key: a v1 certificate like LG's, a v3 one, and its signature
    // over MESSAGE.
    const CERT_V1: &[u8] = include_bytes!("../testdata/cert-v1.der");
    const CERT_V3: &[u8] = include_bytes!("../testdata/cert-v3.der");
    const SIGNATURE: &[u8] = include_bytes!("../testdata/signature.der");
    const MESSAGE: &[u8] = b"gnome-shell-cast";

    fn algorithms() -> WebPkiSupportedAlgorithms {
        ClientConfig::builder()
            .crypto_provider()
            .signature_verification_algorithms
    }

    fn verify(
        cert: &[u8],
        scheme: SignatureScheme,
        message: &[u8],
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let spki = subject_public_key_info(cert).unwrap();
        verify_tls12_signature(&algorithms(), message, spki, scheme, SIGNATURE)
    }

    #[test]
    fn finds_the_same_key_in_v1_and_v3_certificates() {
        assert_eq!(
            subject_public_key_info(CERT_V1),
            subject_public_key_info(CERT_V3)
        );
    }

    #[test]
    fn verifies_the_signature_of_a_v1_certificate() {
        assert!(verify(CERT_V1, SignatureScheme::ECDSA_NISTP256_SHA256, MESSAGE).is_ok());
    }

    #[test]
    fn verifies_the_signature_of_a_v3_certificate() {
        assert!(verify(CERT_V3, SignatureScheme::ECDSA_NISTP256_SHA256, MESSAGE).is_ok());
    }

    #[test]
    fn rejects_a_signature_over_another_message() {
        assert!(verify(CERT_V1, SignatureScheme::ECDSA_NISTP256_SHA256, b"other").is_err());
    }

    #[test]
    fn rejects_a_scheme_that_does_not_match_the_key() {
        assert!(verify(CERT_V1, SignatureScheme::RSA_PSS_SHA256, MESSAGE).is_err());
    }

    #[test]
    fn rejects_a_truncated_certificate() {
        assert!(subject_public_key_info(&CERT_V1[..100]).is_none());
    }
}
