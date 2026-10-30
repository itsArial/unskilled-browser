//! TPM 2.0 sealing. The data key is sealed into a TPM object whose password is derived from the
//! PIN. Wrong guesses count against the TPM's dictionary-attack lockout, and the sealed blob is
//! useless on any other machine — which is what makes a short PIN safe.

use std::str::FromStr;
use tss_esapi::{
    Context, TctiNameConf,
    attributes::ObjectAttributesBuilder,
    constants::response_code::Tss2ResponseCodeKind,
    handles::{KeyHandle, ObjectHandle},
    interface_types::{
        algorithm::{HashingAlgorithm, PublicAlgorithm},
        ecc::EccCurve,
        resource_handles::Hierarchy,
        session_handles::AuthSession,
    },
    structures::{
        Auth, Digest, EccPoint, KeyedHashScheme, Private, Public, PublicBuilder, PublicEccParametersBuilder,
        PublicKeyedHashParameters, SensitiveData, SymmetricDefinitionObject,
    },
    traits::{Marshall, UnMarshall},
};

const DEVICE: &str = "device:/dev/tpmrm0";

#[derive(Debug)]
pub enum TpmError {
    /// No TPM, or no permission to use it (the user isn't in the `tss` group).
    Unavailable(String),
    WrongPin,
    /// Too many wrong PINs: the TPM refuses until its lockout interval passes.
    LockedOut,
    Other(String),
}

impl std::fmt::Display for TpmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) => write!(f, "TPM unavailable: {e}"),
            Self::WrongPin => write!(f, "wrong PIN"),
            Self::LockedOut => write!(f, "TPM locked after too many wrong PINs"),
            Self::Other(e) => write!(f, "TPM error: {e}"),
        }
    }
}

impl From<tss_esapi::Error> for TpmError {
    fn from(e: tss_esapi::Error) -> Self {
        if let tss_esapi::Error::Tss2Error(rc) = e {
            match rc.kind() {
                Some(Tss2ResponseCodeKind::AuthFail | Tss2ResponseCodeKind::BadAuth) => return Self::WrongPin,
                Some(Tss2ResponseCodeKind::Lockout) => return Self::LockedOut,
                _ => {}
            }
        }
        Self::Other(e.to_string())
    }
}

/// A sealed data key: the TPM object's public and (TPM-encrypted) private parts.
pub struct Sealed {
    pub public: Vec<u8>,
    pub private: Vec<u8>,
}

fn context() -> Result<Context, TpmError> {
    let tcti = TctiNameConf::from_str(DEVICE).map_err(|e| TpmError::Unavailable(e.to_string()))?;
    Context::new(tcti).map_err(|e| TpmError::Unavailable(e.to_string()))
}

/// Whether we can talk to the TPM right now.
pub fn available() -> bool {
    std::fs::OpenOptions::new().read(true).write(true).open("/dev/tpmrm0").is_ok() && context().is_ok()
}

/// The storage root key: recreated deterministically from the owner hierarchy's seed each time.
/// ECC P-256 (the TCG's standard SRK template) — deriving an RSA key took the TPM ~1 s per unlock.
fn primary(ctx: &mut Context) -> Result<KeyHandle, TpmError> {
    let attributes = ObjectAttributesBuilder::new()
        .with_fixed_tpm(true)
        .with_fixed_parent(true)
        .with_sensitive_data_origin(true)
        .with_user_with_auth(true)
        .with_decrypt(true)
        .with_restricted(true)
        .build()?;
    let template = PublicBuilder::new()
        .with_public_algorithm(PublicAlgorithm::Ecc)
        .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
        .with_object_attributes(attributes)
        .with_ecc_parameters(
            PublicEccParametersBuilder::new_restricted_decryption_key(SymmetricDefinitionObject::AES_128_CFB, EccCurve::NistP256)
                .build()?,
        )
        .with_ecc_unique_identifier(EccPoint::default())
        .build()?;
    let result = ctx.execute_with_nullauth_session(|ctx| ctx.create_primary(Hierarchy::Owner, template, None, None, None, None))?;
    Ok(result.key_handle)
}

fn sealed_template() -> Result<Public, TpmError> {
    let attributes = ObjectAttributesBuilder::new()
        .with_fixed_tpm(true)
        .with_fixed_parent(true)
        .with_user_with_auth(true)
        // Leaving no_da unset keeps the object under dictionary-attack protection.
        .build()?;
    Ok(PublicBuilder::new()
        .with_public_algorithm(PublicAlgorithm::KeyedHash)
        .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
        .with_object_attributes(attributes)
        .with_keyed_hash_parameters(PublicKeyedHashParameters::new(KeyedHashScheme::Null))
        .with_keyed_hash_unique_identifier(Digest::default())
        .build()?)
}

pub fn seal(secret: &[u8], auth: &[u8]) -> Result<Sealed, TpmError> {
    let mut ctx = context()?;
    let parent = primary(&mut ctx)?;
    let template = sealed_template()?;
    let auth = Auth::try_from(auth.to_vec())?;
    let data = SensitiveData::try_from(secret.to_vec())?;
    let created =
        ctx.execute_with_nullauth_session(|ctx| ctx.create(parent, template, Some(auth), Some(data), None, None));
    let _ = ctx.flush_context(parent.into());
    let created = created?;
    Ok(Sealed { public: created.out_public.marshall()?, private: created.out_private.value().to_vec() })
}

pub fn unseal(sealed: &Sealed, auth: &[u8]) -> Result<Vec<u8>, TpmError> {
    let mut ctx = context()?;
    let parent = primary(&mut ctx)?;
    let public = Public::unmarshall(&sealed.public)?;
    let private = Private::try_from(sealed.private.clone())?;
    let loaded = ctx.execute_with_nullauth_session(|ctx| ctx.load(parent, private, public));
    let _ = ctx.flush_context(parent.into());
    let handle = loaded?;
    let object: ObjectHandle = handle.into();
    ctx.tr_set_auth(object, Auth::try_from(auth.to_vec())?)?;
    let secret = ctx.execute_with_session(Some(AuthSession::Password), |ctx| ctx.unseal(object));
    let _ = ctx.flush_context(object);
    Ok(secret?.value().to_vec())
}
