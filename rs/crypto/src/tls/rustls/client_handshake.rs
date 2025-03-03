use crate::tls::rustls::cert_resolver::StaticCertResolver;
use crate::tls::rustls::csp_server_signing_key::CspServerEd25519SigningKey;
use crate::tls::rustls::node_cert_verifier::NodeServerCertVerifier;
use crate::tls::rustls::{certified_key, RustlsTlsStream};
use crate::tls::{tls_cert_from_registry, TlsCertFromRegistryError};
use ic_crypto_internal_csp::api::CspTlsHandshakeSignerProvider;
use ic_crypto_internal_csp::key_id::KeyId;
use ic_crypto_tls_interfaces::{SomeOrAllNodes, TlsClientHandshakeError, TlsStream};
use ic_interfaces_registry::RegistryClient;
use ic_types::{NodeId, RegistryVersion};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::rustls::ciphersuite::{TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384};
use tokio_rustls::rustls::sign::CertifiedKey;
use tokio_rustls::rustls::{ClientConfig, ProtocolVersion, ResolvesClientCert, SignatureScheme};
use tokio_rustls::webpki::DNSNameRef;
use tokio_rustls::TlsConnector;

pub async fn perform_tls_client_handshake<P: CspTlsHandshakeSignerProvider>(
    signer_provider: &P,
    self_node_id: NodeId,
    registry_client: Arc<dyn RegistryClient>,
    tcp_stream: TcpStream,
    server: NodeId,
    registry_version: RegistryVersion,
) -> Result<Box<dyn TlsStream>, TlsClientHandshakeError> {
    let self_tls_cert =
        tls_cert_from_registry(registry_client.as_ref(), self_node_id, registry_version)?;
    let self_tls_cert_key_id = KeyId::try_from(&self_tls_cert).map_err(|error| {
        TlsClientHandshakeError::MalformedSelfCertificate {
            internal_error: format!("Cannot instantiate KeyId: {:?}", error),
        }
    })?;
    let mut config = ClientConfig::new();
    config.versions = vec![ProtocolVersion::TLSv1_3];
    config.ciphersuites = vec![&TLS13_AES_256_GCM_SHA384, &TLS13_AES_128_GCM_SHA256];
    let ed25519_signing_key =
        CspServerEd25519SigningKey::new(self_tls_cert_key_id, signer_provider.handshake_signer());
    config.client_auth_cert_resolver = static_cert_resolver(
        certified_key(self_tls_cert, ed25519_signing_key),
        SignatureScheme::ED25519,
    );
    let server_cert_verifier = NodeServerCertVerifier::new(
        SomeOrAllNodes::new_with_single_node(server),
        registry_client,
        registry_version,
    );
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(server_cert_verifier));

    connect(tcp_stream, config).await
}

fn static_cert_resolver(key: CertifiedKey, scheme: SignatureScheme) -> Arc<dyn ResolvesClientCert> {
    Arc::new(StaticCertResolver::new(key, scheme).expect(
        "Failed to create the static cert resolver because the signing key referenced \
        in the certified key is incompatible with the signature scheme. This is an implementation error.",
    ))
}

async fn connect(
    tcp_stream: TcpStream,
    config: ClientConfig,
) -> Result<Box<dyn TlsStream>, TlsClientHandshakeError> {
    let irrelevant_domain =
        DNSNameRef::try_from_ascii_str("domain.is-irrelevant-as-hostname-verification-is.disabled")
            .expect("failed to create domain");
    let tls_stream = TlsConnector::from(Arc::new(config))
        .connect(irrelevant_domain, tcp_stream)
        .await
        .map_err(|e| TlsClientHandshakeError::HandshakeError {
            internal_error: format!("{}", e),
        })?;
    Ok(Box::new(RustlsTlsStream::new(
        tokio_rustls::TlsStream::from(tls_stream),
    )))
}

impl From<TlsCertFromRegistryError> for TlsClientHandshakeError {
    fn from(registry_error: TlsCertFromRegistryError) -> Self {
        match registry_error {
            TlsCertFromRegistryError::RegistryError(e) => TlsClientHandshakeError::RegistryError(e),
            TlsCertFromRegistryError::CertificateNotInRegistry {
                node_id,
                registry_version,
            } => TlsClientHandshakeError::CertificateNotInRegistry {
                node_id,
                registry_version,
            },
            TlsCertFromRegistryError::CertificateMalformed { internal_error } => {
                TlsClientHandshakeError::MalformedSelfCertificate { internal_error }
            }
        }
    }
}
