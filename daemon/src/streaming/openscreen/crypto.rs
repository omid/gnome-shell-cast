//! Cast Streaming frame encryption, ported from openscreen
//! `cast/streaming/impl/frame_crypto.cc`.
//!
//! Every encoded frame is encrypted as one unit with AES-128-CTR. The nonce
//! is the 16-byte IV mask (negotiated in the OFFER) `XORed` with the frame id,
//! written big-endian into bytes 8..12.

use aes::cipher::KeyIvInit;

pub type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

pub struct FrameCrypto {
    key: [u8; 16],
    iv_mask: [u8; 16],
}

impl FrameCrypto {
    pub fn new(key: [u8; 16], iv_mask: [u8; 16]) -> Self {
        Self { key, iv_mask }
    }

    /// Returns a CTR cipher positioned at the start of `frame_id`'s
    /// keystream. Feeding the frame's bytes through it in order (in any chunk
    /// sizes) encrypts the frame without an intermediate copy.
    pub fn cipher(&self, frame_id: u64) -> Aes128Ctr {
        let mut nonce = self.iv_mask;
        let id_bytes = (frame_id as u32).to_be_bytes();
        for (i, b) in id_bytes.iter().enumerate() {
            nonce[8 + i] ^= b;
        }
        Aes128Ctr::new(&self.key.into(), &nonce.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{BlockCipherEncrypt, KeyInit, StreamCipher};

    fn encrypt(crypto: &FrameCrypto, frame_id: u64, data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        crypto.cipher(frame_id).apply_keystream(&mut out);
        out
    }

    #[test]
    fn ctr_construction_matches_openssl_aes_ctr128() {
        // AES_ctr128_encrypt treats the whole 16-byte nonce as a big-endian
        // counter and XORs the AES-ECB of it into the plaintext. Verify our
        // first keystream block is AES-ECB(nonce) with the frame id folded in.
        let key = [7_u8; 16];
        let iv_mask = [0x55_u8; 16];
        let frame_id = 0x0102_0304_u64;

        let crypto = FrameCrypto::new(key, iv_mask);
        let encrypted = encrypt(&crypto, frame_id, &[0_u8; 16]);

        let mut nonce = iv_mask;
        nonce[8] ^= 0x01;
        nonce[9] ^= 0x02;
        nonce[10] ^= 0x03;
        nonce[11] ^= 0x04;
        let ecb = aes::Aes128::new(&key.into());
        let mut block = aes::Block::from(nonce);
        ecb.encrypt_block(&mut block);
        assert_eq!(encrypted, block.to_vec());
    }

    #[test]
    fn chunked_keystream_matches_one_shot() {
        // The packetizer feeds the frame through the cipher one payload chunk
        // at a time; the result must equal a single whole-frame pass.
        let crypto = FrameCrypto::new([3; 16], [4; 16]);
        let data: Vec<u8> = (0..100).map(|i| i as u8).collect();
        let whole = encrypt(&crypto, 9, &data);
        let mut cipher = crypto.cipher(9);
        let mut chunked = data;
        for chunk in chunked.chunks_mut(33) {
            cipher.apply_keystream(chunk);
        }
        assert_eq!(chunked, whole);
    }

    #[test]
    fn different_frames_use_different_keystreams() {
        let crypto = FrameCrypto::new([1; 16], [2; 16]);
        let a = encrypt(&crypto, 1, b"hello cast streaming");
        let b = encrypt(&crypto, 2, b"hello cast streaming");
        assert_ne!(a, b);
        // Same frame id decrypts (CTR is symmetric).
        assert_eq!(encrypt(&crypto, 1, &a), b"hello cast streaming");
    }
}
