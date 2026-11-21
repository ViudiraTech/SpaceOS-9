//
//       user/spk/spk-core/src/keys.rs
//       ed25519 repository keys and detached signatures
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fsutil::{atomic_write, sha256_bytes};
use crate::{Error, Result};

pub const KEY_SCHEMA: u32 = 1;
pub const SIGNATURE_SCHEMA: u32 = 1;
pub const ALGORITHM: &str = "ed25519";
pub const DEFAULT_COMMENT: &str = "SpaceOS Build <build@spaceos>";
const MAX_KEY_FILE_SIZE: u64 = 64 * 1024;
const MAX_SIGNATURE_FILE_SIZE: u64 = 16 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SecretKeyFile {
    pub schema: u32,
    pub algorithm: String,
    pub key_id: String,
    #[serde(default)]
    pub comment: String,
    pub secret_key: String,
    pub public_key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PublicKeyFile {
    pub schema: u32,
    pub algorithm: String,
    pub key_id: String,
    #[serde(default)]
    pub comment: String,
    pub public_key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SignatureFile {
    pub schema: u32,
    pub algorithm: String,
    pub key_id: String,
    pub sha256: String,
    pub signature: String,
}

pub struct KeyPair {
    pub key_id: String,
    pub comment: String,
    signing: SigningKey,
}

pub struct PublicKey {
    pub key_id: String,
    pub comment: String,
    pub bytes: [u8; 32],
    verifying: VerifyingKey,
}

impl KeyPair {
    pub fn generate(comment: &str) -> Result<Self> {
        let mut seed = [0u8; 32];
        let mut urandom = fs::File::open("/dev/urandom")?;
        urandom.read_exact(&mut seed)?;
        let pair = Self::from_seed(&seed, comment);
        seed.fill(0);
        Ok(pair)
    }

    pub fn from_seed(seed: &[u8; 32], comment: &str) -> Self {
        let signing = SigningKey::from_bytes(seed);
        let public = signing.verifying_key().to_bytes();
        Self {
            key_id: key_id_from_public(&public),
            comment: comment.to_string(),
            signing,
        }
    }

    pub fn public(&self) -> PublicKey {
        let bytes = self.signing.verifying_key().to_bytes();
        PublicKey {
            key_id: self.key_id.clone(),
            comment: self.comment.clone(),
            bytes,
            verifying: self.signing.verifying_key(),
        }
    }

    pub fn sign(&self, data: &[u8]) -> SignatureFile {
        let signature = self.signing.sign(data);
        SignatureFile {
            schema: SIGNATURE_SCHEMA,
            algorithm: ALGORITHM.to_string(),
            key_id: self.key_id.clone(),
            sha256: sha256_bytes(data),
            signature: BASE64.encode(signature.to_bytes()),
        }
    }

    pub fn secret_file(&self) -> SecretKeyFile {
        SecretKeyFile {
            schema: KEY_SCHEMA,
            algorithm: ALGORITHM.to_string(),
            key_id: self.key_id.clone(),
            comment: self.comment.clone(),
            secret_key: BASE64.encode(self.signing.to_bytes()),
            public_key: BASE64.encode(self.signing.verifying_key().to_bytes()),
        }
    }

    pub fn public_file(&self) -> PublicKeyFile {
        PublicKeyFile {
            schema: KEY_SCHEMA,
            algorithm: ALGORITHM.to_string(),
            key_id: self.key_id.clone(),
            comment: self.comment.clone(),
            public_key: BASE64.encode(self.signing.verifying_key().to_bytes()),
        }
    }
}

impl PublicKey {
    pub fn file(&self) -> PublicKeyFile {
        PublicKeyFile {
            schema: KEY_SCHEMA,
            algorithm: ALGORITHM.to_string(),
            key_id: self.key_id.clone(),
            comment: self.comment.clone(),
            public_key: BASE64.encode(self.bytes),
        }
    }

    pub fn to_json(&self) -> Result<String> {
        pretty_json(&self.file())
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let size = fs::metadata(path)?.len();
        if size > MAX_KEY_FILE_SIZE {
            return Err(Error::Key(format!(
                "{}: key file is unexpectedly large ({size} bytes)",
                path.display()
            )));
        }
        let text = fs::read_to_string(path)?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        if value.get("secret_key").is_some() {
            let secret: SecretKeyFile = serde_json::from_value(value)?;
            validate_key_file_header(secret.schema, &secret.algorithm, &secret.key_id)?;
            let seed = decode_exact::<32>(&secret.secret_key, "secret key")?;
            let pair = KeyPair::from_seed(&seed, &secret.comment);
            let public = pair.public();
            if public.bytes != decode_exact::<32>(&secret.public_key, "public key")? {
                return Err(Error::Key("public key does not match secret key".into()));
            }
            if public.key_id != secret.key_id {
                return Err(Error::Key(format!(
                    "key id mismatch: file says {}, derived {}",
                    secret.key_id, public.key_id
                )));
            }
            Ok(public)
        } else {
            let public: PublicKeyFile = serde_json::from_value(value)?;
            validate_key_file_header(public.schema, &public.algorithm, &public.key_id)?;
            let bytes = decode_exact::<32>(&public.public_key, "public key")?;
            let verifying = VerifyingKey::from_bytes(&bytes)
                .map_err(|e| Error::Key(format!("invalid ed25519 public key: {e}")))?;
            let derived = key_id_from_public(&bytes);
            if derived != public.key_id {
                return Err(Error::Key(format!(
                    "key id mismatch: file says {}, derived {}",
                    public.key_id, derived
                )));
            }
            Ok(PublicKey {
                key_id: public.key_id,
                comment: public.comment,
                bytes,
                verifying,
            })
        }
    }

    pub fn verify(&self, data: &[u8], signature: &SignatureFile) -> Result<()> {
        if signature.schema != SIGNATURE_SCHEMA {
            return Err(Error::Signature(format!(
                "unsupported signature schema {}",
                signature.schema
            )));
        }
        if signature.algorithm != ALGORITHM {
            return Err(Error::Signature(format!(
                "unsupported signature algorithm '{}'",
                signature.algorithm
            )));
        }
        if signature.key_id != self.key_id {
            return Err(Error::Signature(format!(
                "signature was made by {}, not {}",
                signature.key_id, self.key_id
            )));
        }
        let digest = sha256_bytes(data);
        if signature.sha256 != digest {
            return Err(Error::Signature(format!(
                "file digest mismatch: signature covers {}, file is {}",
                signature.sha256, digest
            )));
        }
        let raw = BASE64
            .decode(&signature.signature)
            .map_err(|e| Error::Signature(format!("malformed signature encoding: {e}")))?;
        let raw: [u8; 64] = raw
            .try_into()
            .map_err(|_| Error::Signature("signature must be 64 bytes".into()))?;
        let sig = Signature::from_bytes(&raw);
        self.verifying
            .verify_strict(data, &sig)
            .map_err(|e| Error::Signature(format!("ed25519 verification failed: {e}")))
    }
}

fn validate_key_file_header(schema: u32, algorithm: &str, key_id: &str) -> Result<()> {
    if schema != KEY_SCHEMA {
        return Err(Error::Key(format!("unsupported key schema {schema}")));
    }
    if algorithm != ALGORITHM {
        return Err(Error::Key(format!(
            "unsupported key algorithm '{algorithm}'"
        )));
    }
    if !is_valid_key_id(key_id) {
        return Err(Error::Key(format!("malformed key id '{key_id}'")));
    }
    Ok(())
}

fn decode_exact<const N: usize>(text: &str, what: &str) -> Result<[u8; N]> {
    let bytes = BASE64
        .decode(text)
        .map_err(|e| Error::Key(format!("malformed {what} encoding: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Key(format!("{what} must be {N} bytes")))
}

pub fn key_id_from_public(public: &[u8; 32]) -> String {
    let digest = Sha256::digest(public);
    format!("{}:{}", ALGORITHM, hex::encode(&digest[..8]))
}

pub fn is_valid_key_id(key_id: &str) -> bool {
    let Some(rest) = key_id.strip_prefix("ed25519:") else {
        return false;
    };
    rest.len() == 16
        && rest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn load_secret_key(path: &Path) -> Result<KeyPair> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_KEY_FILE_SIZE {
        return Err(Error::Key(format!(
            "{}: key file is unexpectedly large ({} bytes)",
            path.display(),
            metadata.len()
        )));
    }
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(Error::Key(format!(
            "{}: private key permissions are too open (found {:04o}, need 0600)",
            path.display(),
            mode & 0o7777
        )));
    }
    let text = fs::read_to_string(path)?;
    let file: SecretKeyFile = serde_json::from_str(&text)?;
    validate_key_file_header(file.schema, &file.algorithm, &file.key_id)?;
    let seed = decode_exact::<32>(&file.secret_key, "secret key")?;
    let pair = KeyPair::from_seed(&seed, &file.comment);
    if pair.public().bytes != decode_exact::<32>(&file.public_key, "public key")? {
        return Err(Error::Key("public key does not match secret key".into()));
    }
    if pair.key_id != file.key_id {
        return Err(Error::Key(format!(
            "key id mismatch: file says {}, derived {}",
            file.key_id, pair.key_id
        )));
    }
    Ok(pair)
}

pub fn write_keypair(
    directory: &Path,
    name: &str,
    comment: &str,
    force: bool,
) -> Result<(PathBuf, PathBuf, KeyPair)> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(Error::Key(format!("invalid key name '{name}'")));
    }
    fs::create_dir_all(directory)?;
    let secret_path = directory.join(format!("{name}.key"));
    let public_path = directory.join(format!("{name}.pub"));
    if !force && (secret_path.exists() || public_path.exists()) {
        return Err(Error::Key(format!(
            "{} already exists (use --force to replace it)",
            secret_path.display()
        )));
    }
    let pair = KeyPair::generate(comment)?;
    let secret_json = pretty_json(&pair.secret_file())?;
    let public_json = pretty_json(&pair.public_file())?;
    atomic_write(&secret_path, secret_json.as_bytes(), 0o600)?;
    atomic_write(&public_path, public_json.as_bytes(), 0o644)?;
    Ok((secret_path, public_path, pair))
}

pub fn pretty_json<T: Serialize>(value: &T) -> Result<String> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(text)
}

pub fn signature_path(data_path: &Path) -> PathBuf {
    let mut name = data_path.as_os_str().to_os_string();
    name.push(".sig");
    PathBuf::from(name)
}

pub fn read_signature(path: &Path) -> Result<SignatureFile> {
    let size = fs::metadata(path)?.len();
    if size > MAX_SIGNATURE_FILE_SIZE {
        return Err(Error::Signature(format!(
            "{}: signature file is unexpectedly large ({size} bytes)",
            path.display()
        )));
    }
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

pub fn write_signature(path: &Path, signature: &SignatureFile) -> Result<()> {
    atomic_write(path, pretty_json(signature)?.as_bytes(), 0o644)?;
    Ok(())
}

pub fn sign_file(key: &KeyPair, data_path: &Path) -> Result<SignatureFile> {
    let data = fs::read(data_path)?;
    let signature = key.sign(&data);
    write_signature(&signature_path(data_path), &signature)?;
    Ok(signature)
}

pub fn verify_file(
    public: &PublicKey,
    data_path: &Path,
    sig_path: Option<&Path>,
) -> Result<SignatureFile> {
    let data = fs::read(data_path)?;
    let default_sig;
    let sig_path = match sig_path {
        Some(path) => path,
        None => {
            default_sig = signature_path(data_path);
            &default_sig
        }
    };
    let signature = read_signature(sig_path)?;
    public.verify(&data, &signature)?;
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> KeyPair {
        KeyPair::from_seed(&[7u8; 32], DEFAULT_COMMENT)
    }

    #[test]
    fn sign_and_verify() {
        let key = pair();
        let data = b"index bytes\n";
        let sig = key.sign(data);
        key.public().verify(data, &sig).unwrap();
        assert!(key.public().verify(b"tampered", &sig).is_err());

        let other = KeyPair::from_seed(&[8u8; 32], DEFAULT_COMMENT);
        assert!(other.public().verify(data, &sig).is_err());
    }

    #[test]
    fn key_ids_are_stable() {
        let key = pair();
        assert_eq!(key.key_id, key.public().key_id);
        assert!(is_valid_key_id(&key.key_id));
        assert_eq!(key.key_id.len(), "ed25519:".len() + 16);
        assert_eq!(
            error_kind(&key.public().verify(b"x", &key.sign(b"y"))),
            "signature"
        );
    }

    #[test]
    fn key_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let (secret, public, key) =
            write_keypair(dir.path(), "test", DEFAULT_COMMENT, false).unwrap();
        let loaded = load_secret_key(&secret).unwrap();
        assert_eq!(loaded.key_id, key.key_id);
        let loaded_public = PublicKey::from_file(&public).unwrap();
        assert_eq!(loaded_public.key_id, key.key_id);
        let loaded_from_secret = PublicKey::from_file(&secret).unwrap();
        assert_eq!(loaded_from_secret.key_id, key.key_id);
        assert!(write_keypair(dir.path(), "test", DEFAULT_COMMENT, false).is_err());
        assert!(write_keypair(dir.path(), "test", DEFAULT_COMMENT, true).is_ok());
    }

    #[test]
    fn rejects_world_readable_secret_key() {
        let dir = tempfile::tempdir().unwrap();
        let (secret, _, _) = write_keypair(dir.path(), "test", DEFAULT_COMMENT, false).unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_secret_key(&secret).is_err());
    }

    fn error_kind(result: &Result<()>) -> &'static str {
        match result {
            Ok(()) => "ok",
            Err(Error::Signature(_)) => "signature",
            Err(_) => "other",
        }
    }
}
