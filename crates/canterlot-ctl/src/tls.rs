//! Собственный УЦ установки и сертификат gRPC хаба. Ключ УЦ живёт только в
//! `/etc/canterlot/ca/` (root, 0600) и в контейнеры не монтируется.

use std::{fmt::Write as _, fs, net::IpAddr};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::{error::CtlError, fsx, layout::Layout};

const CA_DAYS: i64 = 3650;
const HUB_DAYS: i64 = 730;

pub struct Ca {
    pub cert_der: Vec<u8>,
    pub cert_pem: String,
    key_pem: String,
}

impl std::fmt::Debug for Ca {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Ключ не попадает ни в журналы, ни в отладочный вывод.
        f.debug_struct("Ca")
            .field("cert_pem", &self.cert_pem)
            .finish_non_exhaustive()
    }
}

fn tls_error(err: rcgen::Error) -> CtlError {
    CtlError::Other(format!("tls_error: {err}"))
}

fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "canterlot installation CA");
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(CA_DAYS);
    params
}

pub fn new_ca() -> Result<Ca, CtlError> {
    let key = KeyPair::generate().map_err(tls_error)?;
    let cert = ca_params().self_signed(&key).map_err(tls_error)?;
    Ok(Ca {
        cert_der: cert.der().to_vec(),
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

/// SAN хаба: известные установщику имена и адреса плюс `localhost` и `127.0.0.1`
/// для воркера на том же хосте; без повторов, в порядке появления.
pub fn hub_names(known: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in known
        .iter()
        .map(String::as_str)
        .chain(["localhost", "127.0.0.1"])
    {
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    }
    names
}

/// Возвращает (сертификат PEM, ключ PEM) хаба, подписанный УЦ.
pub fn issue_hub_cert(ca: &Ca, names: &[String]) -> Result<(String, String), CtlError> {
    let ca_key = KeyPair::from_pem(&ca.key_pem).map_err(tls_error)?;
    let issuer = Issuer::from_ca_cert_pem(&ca.cert_pem, ca_key).map_err(tls_error)?;
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(
        DnType::CommonName,
        names.first().map(String::as_str).unwrap_or("canterlot-hub"),
    );
    params.distinguished_name = dn;
    params.subject_alt_names = names
        .iter()
        .map(|name| match name.parse::<IpAddr>() {
            Ok(ip) => Ok(SanType::IpAddress(ip)),
            Err(_) => name.clone().try_into().map(SanType::DnsName),
        })
        .collect::<Result<_, _>>()
        .map_err(tls_error)?;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(HUB_DAYS);
    let key = KeyPair::generate().map_err(tls_error)?;
    let cert = params.signed_by(&key, &issuer).map_err(tls_error)?;
    Ok((cert.pem(), key.serialize_pem()))
}

pub fn load_ca(layout: &Layout) -> Result<Ca, CtlError> {
    let dir = layout.ca_dir();
    let cert_pem = fs::read_to_string(dir.join("ca.crt"))?;
    let key_pem = fs::read_to_string(dir.join("ca.key"))?;
    let parsed = pem_der(&cert_pem)?;
    Ok(Ca {
        cert_der: parsed,
        cert_pem,
        key_pem,
    })
}

fn pem_der(pem: &str) -> Result<Vec<u8>, CtlError> {
    let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map_err(|_| CtlError::Other("tls_error: bad certificate PEM".into()))?;
    Ok(parsed.contents)
}

/// Записывает УЦ, только если его ещё нет: повторная установка не меняет доверие воркеров.
pub fn save_ca(layout: &Layout, ca: &Ca) -> Result<(), CtlError> {
    let dir = layout.ca_dir();
    fsx::ensure_dir(&layout.root, &dir, 0o700)?;
    fsx::write_new(&dir.join("ca.key"), ca.key_pem.as_bytes(), 0o600)?;
    fsx::write_new(&dir.join("ca.crt"), ca.cert_pem.as_bytes(), 0o644)?;
    Ok(())
}

/// Отпечаток УЦ в форме `--pin`: `sha256:` и hex SHA-256 от DER.
pub fn pin_of(der: &[u8]) -> String {
    let mut out = String::from("sha256:");
    for byte in Sha256::digest(der) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_parser::{extensions::GeneralName, prelude::*};

    #[test]
    fn hub_cert_has_all_names_and_chains_to_ca() {
        let ca = new_ca().unwrap();
        let names = hub_names(&["hub.example.internal".into(), "192.0.2.10".into()]);
        assert_eq!(
            names,
            [
                "hub.example.internal",
                "192.0.2.10",
                "localhost",
                "127.0.0.1"
            ]
        );
        let (cert_pem, key_pem) = issue_hub_cert(&ca, &names).unwrap();
        assert!(key_pem.contains("PRIVATE KEY"));

        let (_, ca_cert) = X509Certificate::from_der(&ca.cert_der).unwrap();
        assert!(ca_cert.is_ca());
        let ca_years = (ca_cert.validity().not_after.timestamp()
            - ca_cert.validity().not_before.timestamp())
            / 86_400;
        assert!(ca_years >= 3650, "CA lifetime {ca_years} days");

        let der = pem_der(&cert_pem).unwrap();
        let (_, cert) = X509Certificate::from_der(&der).unwrap();
        cert.verify_signature(Some(ca_cert.public_key())).unwrap();
        assert_eq!(cert.issuer(), ca_cert.subject());
        assert!(!cert.is_ca());
        let san = cert.subject_alternative_name().unwrap().unwrap();
        let mut dns = Vec::new();
        let mut ips = Vec::new();
        for name in &san.value.general_names {
            match name {
                GeneralName::DNSName(n) => dns.push(n.to_string()),
                GeneralName::IPAddress(bytes) => ips.push(bytes.to_vec()),
                _ => {}
            }
        }
        assert_eq!(dns, ["hub.example.internal", "localhost"]);
        assert_eq!(ips, [vec![192, 0, 2, 10], vec![127, 0, 0, 1]]);
    }

    #[test]
    fn save_and_load_ca_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().to_path_buf(), None);
        let ca = new_ca().unwrap();
        save_ca(&layout, &ca).unwrap();
        let loaded = load_ca(&layout).unwrap();
        assert_eq!(loaded.cert_der, ca.cert_der);
        assert_eq!(pin_of(&loaded.cert_der), pin_of(&ca.cert_der));
        // Повторное сохранение другого УЦ не заменяет существующий.
        save_ca(&layout, &new_ca().unwrap()).unwrap();
        assert_eq!(load_ca(&layout).unwrap().cert_der, ca.cert_der);
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(layout.ca_dir().join("ca.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // Новый сертификат хаба подписывается загруженным УЦ.
        issue_hub_cert(&loaded, &hub_names(&[])).unwrap();
    }
}
