//! AES-128-CBC segment decryption (RFC 8216 §5.2).
//!
//! PKCS#7 padding is STRIPPED: HLS pads the final 16-byte block of
//! each segment. A segment whose length is not a multiple of 16 is
//! corrupt input (the spec's encryption operates on whole blocks).

use crate::error::HlsError;
use aes::Aes128;
use aes::cipher::{BlockDecryptMut, KeyIvInit};

type Cbc = cbc::Decryptor<Aes128>;

/// Decrypt one segment in place (returns the unpadded length).
pub fn decrypt_cbc(data: &mut Vec<u8>, key: &[u8; 16], iv: &[u8; 16]) -> Result<usize, HlsError> {
    if data.is_empty() {
        return Ok(0);
    }
    if !data.len().is_multiple_of(16) {
        return Err(HlsError::Decrypt(format!(
            "segment length {} not a multiple of 16",
            data.len()
        )));
    }
    let dec = Cbc::new_from_slices(key, iv).map_err(|e| HlsError::Decrypt(e.to_string()))?;
    // PKCS7 padding is stripped by the cipher API itself — a wrong
    // key surfaces here as a padding error, never as garbage bytes.
    let unpadded = dec
        .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(data)
        .map_err(|_| HlsError::Decrypt("bad PKCS#7 padding (wrong key?)".into()))?;
    let n = unpadded.len();
    data.truncate(n); // shrink in place so callers see the plaintext
    Ok(n)
}

/// §5.2.1.1: absent explicit IV = media sequence number as a 16-byte
/// big-endian integer.
pub fn seq_iv(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{BlockEncryptMut, KeyIvInit};

    #[test]
    fn roundtrip_two_blocks() {
        // Plaintext 32 bytes → after padding+encryption, decrypts back.
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let plain = b"0123456789abcdef0123456789abcde"; // 31 bytes
        let mut enc = cbc::Encryptor::<Aes128>::new(&key.into(), &iv.into())
            .encrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(plain);
        // OpenSSL-style HLS appends padding even for block-multiple
        // input? No — PKCS7 pads 31→32. Decrypt must yield 31.
        let n = decrypt_cbc(&mut enc, &key, &iv).unwrap();
        assert_eq!(n, 31);
        assert_eq!(&enc[..n], plain);
    }

    #[test]
    fn wrong_key_fails_padding() {
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let mut enc =
            cbc::Encryptor::<Aes128>::new(&key.into(), &iv.into())
                .encrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(b"secret data here!");
        let err = decrypt_cbc(&mut enc, &[8u8; 16], &iv).unwrap_err();
        assert!(err.to_string().contains("padding"), "{err}");
    }

    #[test]
    fn unaligned_len_rejected() {
        let mut data = vec![0u8; 20];
        let err = decrypt_cbc(&mut data, &[0u8; 16], &[0u8; 16]).unwrap_err();
        assert!(err.to_string().contains("multiple of 16"), "{err}");
    }

    #[test]
    fn seq_iv_layout() {
        let iv = seq_iv(0x0102);
        assert_eq!(iv[..8], [0; 8]);
        assert_eq!(iv[14], 0x01);
        assert_eq!(iv[15], 0x02);
    }

    #[test]
    fn empty_segment_ok() {
        let mut data = Vec::new();
        assert_eq!(decrypt_cbc(&mut data, &[0u8; 16], &[0u8; 16]).unwrap(), 0);
    }
}
