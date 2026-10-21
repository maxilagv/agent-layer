use age::{secrecy::ExposeSecret, x25519};
use anyhow::{Context, Result, ensure};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroize;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signed<T> {
    pub kind: String,
    pub payload: T,
    pub signature: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keys {
    signing: String,
    encryption: String,
}
impl Drop for Keys {
    fn drop(&mut self) {
        self.signing.zeroize();
        self.encryption.zeroize();
    }
}

fn canonical(v: &serde_json::Value) -> Result<Vec<u8>> {
    fn order(v: &serde_json::Value) -> Result<serde_json::Value> {
        Ok(match v {
            serde_json::Value::Object(m) => {
                let sorted = m.iter().collect::<std::collections::BTreeMap<_, _>>();
                let mut result = serde_json::Map::new();
                for (k, v) in sorted {
                    result.insert(k.clone(), order(v)?);
                }
                serde_json::Value::Object(result)
            }
            serde_json::Value::Array(v) => {
                serde_json::Value::Array(v.iter().map(order).collect::<Result<_>>()?)
            }
            serde_json::Value::Number(n) => {
                ensure!(
                    n.is_i64() || n.is_u64(),
                    "floating-point signed fields unsupported"
                );
                v.clone()
            }
            _ => v.clone(),
        })
    }
    Ok(serde_json::to_vec(&order(v)?)?)
}
fn message<T: Serialize>(kind: &str, value: &T) -> Result<Vec<u8>> {
    canonical(&serde_json::json!({"domain":"layeragent-remote/v1","kind":kind,"payload":value}))
}
pub fn public_key(encoded: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = hex::decode(encoded)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid public key"))?;
    let key = VerifyingKey::from_bytes(&bytes)?;
    ensure!(!key.is_weak(), "weak signing key");
    Ok(key)
}
pub fn recipient(value: &str) -> Result<x25519::Recipient> {
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("native X25519 recipient required"))
}
impl Keys {
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
        Ok(Self {
            signing: hex::encode(bytes),
            encryption: x25519::Identity::generate()
                .to_string()
                .expose_secret()
                .to_owned(),
        })
    }
    fn signing(&self) -> Result<SigningKey> {
        Ok(SigningKey::from_bytes(
            &hex::decode(&self.signing)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid signing key"))?,
        ))
    }
    fn identity(&self) -> Result<x25519::Identity> {
        self.encryption
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid encryption identity"))
    }
    pub fn public(&self, certificate_sha256: String) -> Result<crate::PublicIdentity> {
        Ok(crate::PublicIdentity {
            signing_key: hex::encode(self.signing()?.verifying_key().as_bytes()),
            encryption_key: self.identity()?.to_public().to_string(),
            certificate_sha256,
        })
    }
    pub fn sign<T: Serialize>(&self, kind: &str, payload: T) -> Result<Signed<T>> {
        let signature = hex::encode(self.signing()?.sign(&message(kind, &payload)?).to_bytes());
        Ok(Signed {
            kind: kind.into(),
            payload,
            signature,
        })
    }
    pub fn decrypt<T: DeserializeOwned>(&self, ciphertext: &str) -> Result<T> {
        ensure!(
            ciphertext.len() <= crate::MAX_ENVELOPE,
            "ciphertext exceeds limit"
        );
        let bytes = hex::decode(ciphertext)?;
        let decryptor = age::Decryptor::new(&bytes[..])?;
        let identity = self.identity()?;
        let mut reader = decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity))?;
        let mut data = Vec::new();
        reader
            .by_ref()
            .take(crate::MAX_ENVELOPE as u64 + 1)
            .read_to_end(&mut data)?;
        ensure!(
            data.len() <= crate::MAX_ENVELOPE,
            "decrypted content exceeds limit"
        );
        Ok(serde_json::from_slice(&data)?)
    }
    pub fn load(path: &Path) -> Result<Self> {
        Ok(serde_json::from_slice(&private_read(path, 8192)?)?)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        private_write(path, &serde_json::to_vec(self)?)
    }
}
impl<T: Serialize> Signed<T> {
    pub fn verify(&self, kind: &str, key: &str) -> Result<()> {
        ensure!(self.kind == kind, "signature domain mismatch");
        let signature = Signature::from_slice(&hex::decode(&self.signature)?)?;
        public_key(key)?
            .verify_strict(&message(kind, &self.payload)?, &signature)
            .context("signature rejected")?;
        Ok(())
    }
}
pub fn encrypt<T: Serialize>(value: &T, recipients: &[String]) -> Result<String> {
    ensure!(
        !recipients.is_empty() && recipients.len() <= 8,
        "explicit bounded recipients required"
    );
    let data = serde_json::to_vec(value)?;
    ensure!(
        data.len() <= crate::MAX_ENVELOPE / 3,
        "task exceeds encryption limit"
    );
    let recipients = recipients
        .iter()
        .map(|r| recipient(r))
        .collect::<Result<Vec<_>>>()?;
    let encryptor =
        age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient))?;
    let mut output = Vec::new();
    let mut writer = encryptor.wrap_output(&mut output)?;
    writer.write_all(&data)?;
    writer.finish()?;
    Ok(hex::encode(output))
}
pub fn private_file(path: &Path, limit: u64) -> Result<std::fs::File> {
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = options.open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= limit,
        "invalid private file"
    );
    #[cfg(unix)]
    ensure!(
        meta.mode() & 0o077 == 0 && meta.uid() == unsafe { libc::geteuid() } && meta.nlink() == 1,
        "private file must be owned by this account, mode 600, without aliases"
    );
    Ok(file)
}
pub fn private_read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = private_file(path, limit)?;
    let mut data = Vec::new();
    file.take(limit + 1).read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= limit, "private file exceeds limit");
    Ok(data)
}
pub fn private_write(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
/// Atomic private checkpoint, including parent fsync. Only use in a private,
/// operator-owned directory outside the execution roots and database backups.
pub fn private_replace(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("private checkpoint parent required")?;
    let meta = std::fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "private checkpoint directory required"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.mode() & 0o077 == 0
                && meta.uid() == unsafe { libc::geteuid() },
            "checkpoint parent must be private and owned"
        );
    }
    if path.exists() {
        private_read(path, 65536)?;
    }
    let temporary = parent.join(format!(".checkpoint-{}", uuid::Uuid::new_v4()));
    private_write(&temporary, data)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signatures_cannot_cross_domains_or_change_recipients() {
        let a = Keys::generate().unwrap();
        let b = Keys::generate().unwrap();
        let pa = a.public(String::new()).unwrap();
        let pb = b.public(String::new()).unwrap();
        let mut signed = a
            .sign("job", serde_json::json!({"runner":pb.encryption_key}))
            .unwrap();
        signed.verify("job", &pa.signing_key).unwrap();
        assert!(signed.verify("enrollment", &pa.signing_key).is_err());
        signed.payload["runner"] = serde_json::json!("other");
        assert!(signed.verify("job", &pa.signing_key).is_err());
        let cipher = encrypt(&vec!["secret"], &[pb.encryption_key]).unwrap();
        assert!(!cipher.contains("secret"));
        assert!(a.decrypt::<Vec<String>>(&cipher).is_err());
        assert_eq!(b.decrypt::<Vec<String>>(&cipher).unwrap(), vec!["secret"]);
    }
}
