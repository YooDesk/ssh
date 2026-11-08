//! 与 Gateway/Connector 同名的 TLS 证书校验开关；默认关闭证书校验。
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};
use std::sync::Arc;

/// 显式 true/1 开启；false/0 或缺省关闭，非法配置直接拒绝。
pub fn verification_enabled() -> Result<bool, String> {
    match std::env::var("YOODESK_VERIFY_TLS_CERTIFICATE") {
        Ok(value) => parse(Some(&value)),
        Err(std::env::VarError::NotPresent) => parse(None),
        Err(_) => Err("invalid YOODESK_VERIFY_TLS_CERTIFICATE".into()),
    }
}

fn parse(value: Option<&str>) -> Result<bool, String> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("false" | "0") => Ok(false),
        Some("true" | "1") => Ok(true),
        _ => Err("invalid YOODESK_VERIFY_TLS_CERTIFICATE: use true or false".into()),
    }
}

#[derive(Debug)]
struct IgnoreCertificateErrors(Arc<CryptoProvider>);
impl ServerCertVerifier for IgnoreCertificateErrors {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

pub(crate) fn ignore_certificate_errors() -> Arc<dyn ServerCertVerifier> {
    Arc::new(IgnoreCertificateErrors(Arc::new(
        rustls::crypto::ring::default_provider(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn certificate_policy_is_explicit_and_defaults_to_disabled() {
        assert!(!parse(None).unwrap());
        for value in ["0", "false", " FALSE "] {
            assert!(!parse(Some(value)).unwrap());
        }
        for value in ["1", "true", " TRUE "] {
            assert!(parse(Some(value)).unwrap());
        }
        for value in ["", "yes", "ignore"] {
            assert!(parse(Some(value)).is_err());
        }
    }

    #[tokio::test]
    async fn expired_wrong_name_client_only_certificate_still_connects_in_default_mode() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut parameters = rcgen::CertificateParams::new(vec!["other.example".into()]).unwrap();
        parameters.not_before = rcgen::date_time_ymd(2020, 1, 1);
        parameters.not_after = rcgen::date_time_ymd(2020, 1, 2);
        parameters
            .extended_key_usages
            .push(rcgen::ExtendedKeyUsagePurpose::ClientAuth);
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = parameters.self_signed(&key).unwrap();
        let server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio_rustls::TlsAcceptor::from(Arc::new(server))
                .accept(socket)
                .await
                .unwrap()
        });
        let client = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(ignore_certificate_errors())
            .with_no_client_auth();
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let connection = tokio_rustls::TlsConnector::from(Arc::new(client))
            .connect(ServerName::try_from("127.0.0.1").unwrap(), socket)
            .await
            .unwrap();
        assert!(connection.get_ref().1.peer_certificates().is_some());
        task.await.unwrap();
    }
}
