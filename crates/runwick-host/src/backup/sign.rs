//! Signing backups so a swapped archive is noticed.
//!
//! The sidecar of an archive keeps its SHA-256, which anyone who can replace the archive can
//! recompute. So the sidecar also keeps an HMAC-SHA256 of that hash and the computer that made
//! it, keyed with a random key that never leaves this computer: the login Keychain for the real
//! data folder, a 0600 file inside the data folder otherwise (tests and second instances never
//! touch the person's Keychain). A backup is only restored on the computer that made it, so a
//! signature that does not match means the archive was changed after it was made.

use crate::env::secrets::SecretStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use uuid::Uuid;

const ACCOUNT: &str = "backup-signing-key";
const LABEL: &[u8] = b"runwick-backup-signature-v1";
const KEY_BYTES: usize = 32;

/// Stored in the sidecar next to the archive hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signature {
    /// Which key signed (so a replaced key reads as "another key", not as tampering).
    pub key_id: String,
    pub hmac: String,
}

/// What the signature of an archive says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Signed with this computer's key and unchanged.
    Valid,
    /// No signature (no sidecar, or made before signing existed).
    Unsigned,
    /// Signed with a key this computer does not have now.
    OtherKey,
    /// The key could not be read (a locked or refused Keychain).
    NoKey,
    /// Signed with this computer's key, but the archive or its sidecar changed since.
    Bad,
}

impl Check {
    /// Why the signature cannot vouch for the archive, for the restore preview.
    pub fn unconfirmed(self) -> Option<String> {
        let why = match self {
            Check::Valid | Check::Bad => return None,
            Check::Unsigned => {
                "이 백업에는 이 컴퓨터의 서명이 없어요 (서명 기능 전에 만들었거나 함께 둔 정보 파일이 없어요)"
            }
            Check::OtherKey => {
                "지금 이 컴퓨터의 서명 키로 만든 백업이 아니에요 (키체인을 새로 만들었거나 다른 곳에서 온 파일일 수 있어요)"
            }
            Check::NoKey => "키체인에서 백업 서명 키를 읽지 못해 서명을 확인하지 못했어요",
        };
        Some(why.to_string())
    }
}

pub const TAMPERED: &str = "이 컴퓨터의 서명과 맞지 않아요. 만든 뒤에 바뀐 백업일 수 있어서 열지 않았어요";

#[derive(Clone)]
pub struct Signer {
    key: Vec<u8>,
}

impl Signer {
    pub fn from_key(key: Vec<u8>) -> Signer {
        Signer { key }
    }

    pub fn key_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(LABEL);
        h.update(b"\0key-id\0");
        h.update(&self.key);
        hex(&h.finalize()[..8])
    }

    pub fn sign(&self, host: Uuid, archive_sha256: &str) -> Signature {
        Signature { key_id: self.key_id(), hmac: hex(&hmac_sha256(&self.key, &message(host, archive_sha256))) }
    }
}

fn message(host: Uuid, archive_sha256: &str) -> Vec<u8> {
    let mut m = LABEL.to_vec();
    m.push(0);
    m.extend_from_slice(host.as_bytes());
    m.push(0);
    m.extend_from_slice(archive_sha256.as_bytes());
    m
}

/// Check the signature of an archive whose hash is `archive_sha256` (already compared with the
/// archive itself) made by `host`.
pub fn check(signer: Result<Option<&Signer>, ()>, sig: Option<&Signature>, host: Uuid, archive_sha256: &str) -> Check {
    let Some(sig) = sig else {
        return Check::Unsigned;
    };
    let signer = match signer {
        Ok(Some(s)) => s,
        Ok(None) => return Check::OtherKey,
        Err(()) => return Check::NoKey,
    };
    if signer.key_id() != sig.key_id {
        return Check::OtherKey;
    }
    let want = signer.sign(host, archive_sha256).hmac;
    if same(want.as_bytes(), sig.hmac.as_bytes()) { Check::Valid } else { Check::Bad }
}

/// Equal without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(not(test))]
fn store() -> Box<dyn SecretStore> {
    use crate::env::secrets::FileStore;
    use runwick_core::paths;
    const SERVICE: &str = "dev.runwick.backup";
    if paths::home() == paths::user_home().join(".runwick")
        && let Some(k) = crate::env::secrets::keychain(SERVICE)
    {
        return k;
    }
    Box::new(FileStore { path: paths::home().join("secrets").join("backup-key.json") })
}

/// Unit tests never reach the Keychain or a data folder.
#[cfg(test)]
fn store() -> Box<dyn SecretStore> {
    Box::new(crate::env::secrets::FakeStore::default())
}

static CACHE: Mutex<Option<Signer>> = Mutex::new(None);

/// This computer's signer. `make`: create the key when there is none yet (making a backup);
/// otherwise `Ok(None)` when there is no key. `Err` when the key store could not be read.
pub fn signer(make: bool) -> Result<Option<Signer>, String> {
    let mut cache = CACHE.lock().map_err(|_| "백업 서명 키를 읽지 못했어요".to_string())?;
    if let Some(s) = cache.as_ref() {
        return Ok(Some(s.clone()));
    }
    let store = store();
    let key = match store.get(ACCOUNT)? {
        Some(text) => unhex(&text).filter(|k| k.len() >= KEY_BYTES).ok_or("백업 서명 키가 올바르지 않아요")?,
        None if make => {
            let key = random_key()?;
            store.set(ACCOUNT, &hex(&key))?;
            key
        }
        None => return Ok(None),
    };
    let s = Signer::from_key(key);
    *cache = Some(s.clone());
    Ok(Some(s))
}

fn random_key() -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut key = vec![0u8; KEY_BYTES];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut key))
        .map_err(|e| format!("백업 서명 키를 만들지 못했어요: {e}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_the_rfc_4231_vectors() {
        // Test case 2 and test case 6 (a key longer than one block).
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_changed_hash_host_or_key_is_told_apart() {
        let host = Uuid::now_v7();
        let me = Signer::from_key(vec![7; 32]);
        let sig = me.sign(host, "aa");
        assert_eq!(check(Ok(Some(&me)), Some(&sig), host, "aa"), Check::Valid);
        assert_eq!(check(Ok(Some(&me)), Some(&sig), host, "ab"), Check::Bad);
        assert_eq!(check(Ok(Some(&me)), Some(&sig), Uuid::now_v7(), "aa"), Check::Bad);
        let forged = Signature { key_id: sig.key_id.clone(), hmac: me.sign(host, "ab").hmac };
        assert_eq!(check(Ok(Some(&me)), Some(&forged), host, "aa"), Check::Bad);
        let other = Signer::from_key(vec![8; 32]);
        assert_eq!(check(Ok(Some(&other)), Some(&sig), host, "aa"), Check::OtherKey);
        assert_eq!(check(Ok(None), Some(&sig), host, "aa"), Check::OtherKey);
        assert_eq!(check(Err(()), Some(&sig), host, "aa"), Check::NoKey);
        assert_eq!(check(Ok(Some(&me)), None, host, "aa"), Check::Unsigned);
        assert!(Check::Valid.unconfirmed().is_none());
        assert!(Check::Unsigned.unconfirmed().is_some());
    }

    #[test]
    fn the_key_is_made_once_and_kept() {
        let a = signer(true).unwrap().unwrap();
        let b = signer(false).unwrap().unwrap();
        assert_eq!(a.key_id(), b.key_id());
        assert_eq!(a.key.len(), KEY_BYTES);
        assert_ne!(a.key, vec![0u8; KEY_BYTES]);
    }
}
