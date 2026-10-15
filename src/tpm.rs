//! Optional TPM 2.0 sealing of the face store (feature `tpm`, see `docs/tpm.md`).
//!
//! Envelope encryption: a random 256-bit data key encrypts the store JSON with AES-256-GCM, and
//! the TPM seals that key as a keyed-hash object under a primary key of the owner hierarchy. Only
//! the TPM that sealed the key can load it again. The key can also be bound to SHA-256 PCRs.
//!
//! The TPM is reached through the TCTI named by `TPM2TOOLS_TCTI` or `TCTI` (the tpm2-tools
//! variables), default `device:/dev/tpmrm0`.

use std::str::FromStr;

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tss_esapi::attributes::{ObjectAttributesBuilder, SessionAttributesBuilder};
use tss_esapi::constants::SessionType;
use tss_esapi::handles::KeyHandle;
use tss_esapi::interface_types::algorithm::{HashingAlgorithm, PublicAlgorithm};
use tss_esapi::interface_types::ecc::EccCurve;
use tss_esapi::interface_types::resource_handles::Hierarchy;
use tss_esapi::interface_types::session_handles::{AuthSession, PolicySession};
use tss_esapi::structures::{
    Digest,
    EccPoint,
    KeyedHashScheme,
    PcrSelectionListBuilder,
    PcrSlot,
    Private,
    Public,
    PublicBuilder,
    PublicEccParametersBuilder,
    PublicKeyedHashParameters,
    SensitiveData,
    SymmetricDefinition,
    SymmetricDefinitionObject,
};
use tss_esapi::tcti_ldr::TctiNameConf;
use tss_esapi::traits::{Marshall, UnMarshall};
use zeroize::Zeroizing;

/// `format` field of a sealed store file.
pub const FORMAT: &str = "linux_hello-tpm-v1";

const DEFAULT_TCTI: &str = "device:/dev/tpmrm0";

/// A sealed store file: the sealed data key and the store JSON encrypted under it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedFile {
    format: String,
    key: SealedKey,
    #[serde(with = "base64_bytes")]
    nonce: Vec<u8>,
    #[serde(with = "base64_bytes")]
    ciphertext: Vec<u8>,
}

impl SealedFile {
    pub fn key(&self) -> &SealedKey {
        &self.key
    }
}

/// The sealed data key as the TPM returned it (marshalled `TPM2B_PUBLIC` and `TPM2B_PRIVATE`),
/// plus the SHA-256 PCRs its policy requires (empty: no policy).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedKey {
    #[serde(with = "base64_bytes")]
    public: Vec<u8>,
    #[serde(with = "base64_bytes")]
    private: Vec<u8>,
    pcrs: Vec<u8>,
}

/// An unsealed data key. It stays in memory so that saving and reloading do not need the TPM again.
pub struct DataKey {
    sealed: SealedKey,
    key: Zeroizing<[u8; 32]>,
}

impl DataKey {
    /// Generate a new data key and seal it with the TPM, bound to `pcrs` if not empty.
    pub fn create(pcrs: &[u8]) -> Result<Self> {
        let mut key = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(&mut key[..]);
        let sealed = Tpm::open()?.seal(&key[..], pcrs)?;
        Ok(Self { sealed, key })
    }

    pub fn unseal(sealed: &SealedKey) -> Result<Self> {
        let key = Tpm::open()?.unseal(sealed)?;
        Ok(Self {
            sealed: sealed.clone(),
            key,
        })
    }

    pub fn sealed(&self) -> &SealedKey {
        &self.sealed
    }

    pub fn pcrs(&self) -> &[u8] {
        &self.sealed.pcrs
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<SealedFile> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher()
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: FORMAT.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("encrypting the face store failed"))?;
        Ok(SealedFile {
            format: FORMAT.to_string(),
            key: self.sealed.clone(),
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    pub fn decrypt(&self, file: &SealedFile) -> Result<Zeroizing<Vec<u8>>> {
        if file.format != FORMAT {
            bail!(
                "unsupported sealed store format {:?} (expected {FORMAT:?})",
                file.format
            );
        }
        if file.nonce.len() != 12 {
            bail!(
                "sealed store has a {}-byte nonce, expected 12",
                file.nonce.len()
            );
        }
        let plaintext = self
            .cipher()
            .decrypt(
                Nonce::from_slice(&file.nonce),
                Payload {
                    msg: &file.ciphertext,
                    aad: FORMAT.as_bytes(),
                },
            )
            .map_err(|_| {
                anyhow!(
                    "face store does not decrypt: it was modified, or its key does not belong to it"
                )
            })?;
        Ok(Zeroizing::new(plaintext))
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key[..]))
    }
}

/// A TPM connection with the storage primary key loaded. Dropping `ctx` flushes the objects and
/// sessions created through it, which matters without a resource manager (swtpm, `/dev/tpm0`).
struct Tpm {
    ctx: tss_esapi::Context,
    primary: KeyHandle,
}

impl Tpm {
    fn open() -> Result<Self> {
        let name = std::env::var("TPM2TOOLS_TCTI")
            .or_else(|_| std::env::var("TCTI"))
            .unwrap_or_else(|_| DEFAULT_TCTI.to_string());
        let tcti =
            TctiNameConf::from_str(&name).with_context(|| format!("invalid TCTI {name:?}"))?;
        let mut ctx = tss_esapi::Context::new(tcti).with_context(|| {
            format!("cannot reach the TPM through {name:?}: enable the TPM in the firmware, or set TPM2TOOLS_TCTI")
        })?;
        let template = primary_template()?;
        let primary = ctx
            .execute_with_nullauth_session(|c| {
                c.create_primary(Hierarchy::Owner, template, None, None, None, None)
            })
            .context("creating the TPM storage primary key")?
            .key_handle;
        Ok(Self { ctx, primary })
    }

    fn seal(&mut self, secret: &[u8], pcrs: &[u8]) -> Result<SealedKey> {
        let policy = if pcrs.is_empty() {
            Digest::default()
        } else {
            let trial = self.session(SessionType::Trial, SessionAttributesBuilder::new())?;
            let trial = PolicySession::try_from(trial)?;
            self.policy_pcr(trial, pcrs)?;
            self.ctx.policy_get_digest(trial)?
        };
        let attributes = ObjectAttributesBuilder::new()
            .with_fixed_tpm(true)
            .with_fixed_parent(true)
            .with_no_da(true)
            // With an empty auth value, password auth would bypass the PCR policy.
            .with_user_with_auth(pcrs.is_empty())
            .build()?;
        let public = PublicBuilder::new()
            .with_public_algorithm(PublicAlgorithm::KeyedHash)
            .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
            .with_object_attributes(attributes)
            .with_auth_policy(policy)
            .with_keyed_hash_parameters(PublicKeyedHashParameters::new(KeyedHashScheme::Null))
            .with_keyed_hash_unique_identifier(Digest::default())
            .build()?;
        let data = SensitiveData::try_from(secret)?;
        // Encrypt the command parameter so that the key does not cross the TPM bus in the clear.
        let session = self.session(
            SessionType::Hmac,
            SessionAttributesBuilder::new().with_decrypt(true),
        )?;
        let primary = self.primary;
        let created = self
            .ctx
            .execute_with_session(Some(session), |c| {
                c.create(primary, public, None, Some(data), None, None)
            })
            .context("sealing the data key")?;
        Ok(SealedKey {
            public: created.out_public.marshall()?,
            private: created.out_private.value().to_vec(),
            pcrs: pcrs.to_vec(),
        })
    }

    fn unseal(&mut self, sealed: &SealedKey) -> Result<Zeroizing<[u8; 32]>> {
        let public = Public::unmarshall(&sealed.public).context("malformed sealed key")?;
        let private =
            Private::try_from(sealed.private.as_slice()).context("malformed sealed key")?;
        let primary = self.primary;
        let object = self
            .ctx
            .execute_with_nullauth_session(|c| c.load(primary, private, public))
            .context("this TPM cannot load the sealed key: it was sealed by another TPM, the TPM was cleared, or the file was modified")?;

        // Encrypt the response parameter so that the key does not cross the TPM bus in the clear.
        let attributes = SessionAttributesBuilder::new().with_encrypt(true);
        let session = if sealed.pcrs.is_empty() {
            self.session(SessionType::Hmac, attributes)?
        } else {
            let session = self.session(SessionType::Policy, attributes)?;
            self.policy_pcr(PolicySession::try_from(session)?, &sealed.pcrs)?;
            session
        };
        let data = self
            .ctx
            .execute_with_session(Some(session), |c| c.unseal(object.into()))
            .with_context(|| match sealed.pcrs.as_slice() {
                [] => "unsealing the data key".to_string(),
                pcrs => format!("unsealing the data key: PCRs {pcrs:?} changed since sealing (firmware or Secure Boot change?); re-enroll"),
            })?;
        let key: [u8; 32] = data
            .value()
            .try_into()
            .context("sealed data key is not 32 bytes")?;
        Ok(Zeroizing::new(key))
    }

    /// Start a session salted with the primary key, which is what lets parameter encryption hide
    /// secrets from someone listening on the bus.
    fn session(
        &mut self,
        kind: SessionType,
        attributes: SessionAttributesBuilder,
    ) -> Result<AuthSession> {
        let session = self
            .ctx
            .start_auth_session(
                Some(self.primary),
                None,
                None,
                kind,
                SymmetricDefinition::AES_128_CFB,
                HashingAlgorithm::Sha256,
            )?
            .context("the TPM returned no session")?;
        let (attributes, mask) = attributes.with_continue_session(true).build();
        self.ctx.tr_sess_set_attributes(session, attributes, mask)?;
        Ok(session)
    }

    /// Require the current values of the SHA-256 `pcrs`.
    fn policy_pcr(&mut self, session: PolicySession, pcrs: &[u8]) -> Result<()> {
        let slots = pcrs
            .iter()
            .map(|&pcr| match pcr {
                0..=23 => Ok(PcrSlot::try_from(1u32 << pcr)?),
                _ => bail!("PCR {pcr} does not exist (0-23)"),
            })
            .collect::<Result<Vec<_>>>()?;
        let selection = PcrSelectionListBuilder::new()
            .with_selection(HashingAlgorithm::Sha256, &slots)
            .build()?;
        // An empty digest makes the TPM use the PCRs' current values.
        self.ctx.policy_pcr(session, Digest::default(), selection)?;
        Ok(())
    }
}

/// ECC P-256 storage key in the owner hierarchy. The TPM derives it from its owner seed, so the
/// same template gives the same key every time until the TPM is cleared.
fn primary_template() -> Result<Public> {
    let attributes = ObjectAttributesBuilder::new()
        .with_fixed_tpm(true)
        .with_fixed_parent(true)
        .with_sensitive_data_origin(true)
        .with_user_with_auth(true)
        .with_no_da(true)
        .with_restricted(true)
        .with_decrypt(true)
        .build()?;
    let parameters = PublicEccParametersBuilder::new_restricted_decryption_key(
        SymmetricDefinitionObject::AES_128_CFB,
        EccCurve::NistP256,
    )
    .build()?;
    Ok(PublicBuilder::new()
        .with_public_algorithm(PublicAlgorithm::Ecc)
        .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
        .with_object_attributes(attributes)
        .with_ecc_parameters(parameters)
        .with_ecc_unique_identifier(EccPoint::default())
        .build()?)
}

mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        STANDARD
            .decode(String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}
