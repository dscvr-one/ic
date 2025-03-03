use super::*;
use async_trait::async_trait;
use ic_crypto_internal_logmon::metrics::{MetricsDomain, MetricsResult, MetricsScope};
use ic_crypto_tls_interfaces::{
    AllowedClients, AuthenticatedPeer, MalformedPeerCertificateError, TlsClientHandshakeError,
    TlsHandshake, TlsPublicKeyCert, TlsServerHandshakeError, TlsStream,
};
use ic_logger::{debug, new_logger};
use ic_types::registry::RegistryClientError;
use ic_types::{NodeId, PrincipalId, RegistryVersion};
use openssl::nid::Nid;
use openssl::string::OpensslString;
use openssl::x509::{X509NameEntries, X509NameEntryRef};
use std::str::FromStr;
use tokio::net::TcpStream;

mod rustls;

#[async_trait]
impl<CSP> TlsHandshake for CryptoComponentFatClient<CSP>
where
    CSP: CryptoServiceProvider + Send + Sync,
{
    async fn perform_tls_server_handshake(
        &self,
        tcp_stream: TcpStream,
        allowed_clients: AllowedClients,
        registry_version: RegistryVersion,
    ) -> Result<(Box<dyn TlsStream>, AuthenticatedPeer), TlsServerHandshakeError> {
        let log_id = get_log_id(&self.logger, module_path!());
        let logger = new_logger!(&self.logger;
            crypto.log_id => log_id,
            crypto.trait_name => "TlsHandshake",
            crypto.method_name => "perform_tls_server_handshake",
        );
        debug!(logger;
            crypto.description => "start",
            crypto.registry_version => registry_version.get(),
            crypto.allowed_tls_clients => format!("{:?}", allowed_clients),
        );
        let start_time = self.metrics.now();
        let result = rustls::server_handshake::perform_tls_server_handshake(
            &self.csp,
            self.node_id,
            Arc::clone(&self.registry_client),
            tcp_stream,
            allowed_clients,
            registry_version,
        )
        .await;
        self.metrics.observe_duration_seconds(
            MetricsDomain::TlsHandshake,
            MetricsScope::Full,
            "perform_tls_server_handshake",
            MetricsResult::from(&result),
            start_time,
        );
        debug!(logger;
            crypto.description => "end",
            crypto.is_ok => result.is_ok(),
            crypto.error => log_err(result.as_ref().err()),
        );
        result
    }

    async fn perform_tls_server_handshake_without_client_auth(
        &self,
        tcp_stream: TcpStream,
        registry_version: RegistryVersion,
    ) -> Result<Box<dyn TlsStream>, TlsServerHandshakeError> {
        let log_id = get_log_id(&self.logger, module_path!());
        let logger = new_logger!(&self.logger;
            crypto.log_id => log_id,
            crypto.trait_name => "TlsHandshake",
            crypto.method_name => "perform_tls_server_handshake_without_client_auth",
        );
        debug!(logger;
            crypto.description => "start",
            crypto.registry_version => registry_version.get(),
            crypto.allowed_tls_clients => "all clients allowed",
        );
        let start_time = self.metrics.now();
        let result = rustls::server_handshake::perform_tls_server_handshake_without_client_auth(
            &self.csp,
            self.node_id,
            self.registry_client.as_ref(),
            tcp_stream,
            registry_version,
        )
        .await;
        self.metrics.observe_duration_seconds(
            MetricsDomain::TlsHandshake,
            MetricsScope::Full,
            "perform_tls_server_handshake_without_client_auth",
            MetricsResult::from(&result),
            start_time,
        );
        debug!(logger;
            crypto.description => "end",
            crypto.is_ok => result.is_ok(),
            crypto.error => log_err(result.as_ref().err()),
        );
        result
    }

    async fn perform_tls_client_handshake(
        &self,
        tcp_stream: TcpStream,
        server: NodeId,
        registry_version: RegistryVersion,
    ) -> Result<Box<dyn TlsStream>, TlsClientHandshakeError> {
        let log_id = get_log_id(&self.logger, module_path!());
        let logger = new_logger!(&self.logger;
            crypto.log_id => log_id,
            crypto.trait_name => "TlsHandshake",
            crypto.method_name => "perform_tls_client_handshake",
        );
        debug!(logger;
            crypto.description => "start",
            crypto.registry_version => registry_version.get(),
            crypto.tls_server => format!("{}", server),
        );
        let start_time = self.metrics.now();
        let result = rustls::client_handshake::perform_tls_client_handshake(
            &self.csp,
            self.node_id,
            Arc::clone(&self.registry_client),
            tcp_stream,
            server,
            registry_version,
        )
        .await;
        self.metrics.observe_duration_seconds(
            MetricsDomain::TlsHandshake,
            MetricsScope::Full,
            "perform_tls_client_handshake",
            MetricsResult::from(&result),
            start_time,
        );
        debug!(logger;
            crypto.description => "end",
            crypto.is_ok => result.is_ok(),
            crypto.error => log_err(result.as_ref().err()),
        );
        result
    }
}

fn node_id_from_cert_subject_common_name(
    cert: &TlsPublicKeyCert,
) -> Result<NodeId, MalformedPeerCertificateError> {
    let common_name_entry = ensure_exactly_one_subject_common_name_entry(cert)?;
    let common_name = common_name_entry_as_string(common_name_entry)?;
    let principal_id = parse_principal_id(common_name)?;
    Ok(NodeId::from(principal_id))
}

fn ensure_exactly_one_subject_common_name_entry(
    cert: &TlsPublicKeyCert,
) -> Result<&X509NameEntryRef, MalformedPeerCertificateError> {
    if common_name_entries(cert).count() > 1 {
        return Err(MalformedPeerCertificateError::new(
            "Too many X509NameEntryRefs",
        ));
    }
    common_name_entries(cert)
        .next()
        .ok_or_else(|| MalformedPeerCertificateError::new("Missing X509NameEntryRef"))
}

fn common_name_entry_as_string(
    common_name_entry: &X509NameEntryRef,
) -> Result<OpensslString, MalformedPeerCertificateError> {
    common_name_entry.data().as_utf8().map_err(|e| {
        MalformedPeerCertificateError::new(&format!("ASN1 to UTF-8 conversion error: {}", e))
    })
}

fn parse_principal_id(
    common_name: OpensslString,
) -> Result<PrincipalId, MalformedPeerCertificateError> {
    PrincipalId::from_str(common_name.as_ref()).map_err(|e| {
        MalformedPeerCertificateError::new(&format!("Principal ID parse error: {}", e))
    })
}

fn common_name_entries(cert: &TlsPublicKeyCert) -> X509NameEntries {
    cert.as_x509()
        .subject_name()
        .entries_by_nid(Nid::COMMONNAME)
}

fn tls_cert_from_registry(
    registry: &dyn RegistryClient,
    node_id: NodeId,
    registry_version: RegistryVersion,
) -> Result<TlsPublicKeyCert, TlsCertFromRegistryError> {
    use ic_registry_client_helpers::crypto::CryptoRegistry;
    registry
        .get_tls_certificate(node_id, registry_version)?
        .map_or_else(
            || {
                Err(TlsCertFromRegistryError::CertificateNotInRegistry {
                    node_id,
                    registry_version,
                })
            },
            |cert| {
                let cert = TlsPublicKeyCert::new_from_der(cert.certificate_der).map_err(|e| {
                    TlsCertFromRegistryError::CertificateMalformed {
                        internal_error: e.internal_error,
                    }
                })?;
                Ok(cert)
            },
        )
}

#[derive(Debug)]
enum TlsCertFromRegistryError {
    RegistryError(RegistryClientError),
    CertificateNotInRegistry {
        node_id: NodeId,
        registry_version: RegistryVersion,
    },
    CertificateMalformed {
        internal_error: String,
    },
}

impl From<RegistryClientError> for TlsCertFromRegistryError {
    fn from(registry_error: RegistryClientError) -> Self {
        TlsCertFromRegistryError::RegistryError(registry_error)
    }
}

fn log_err<T: fmt::Display>(error_option: Option<&T>) -> String {
    if let Some(error) = error_option {
        return format!("{}", error);
    }
    "none".to_string()
}
