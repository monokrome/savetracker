use std::collections::HashMap;

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes256;
use thiserror::Error;

use super::definition::PipelineLayer;
use crate::decompress::{self, CompressionType};

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("layer {layer} ({kind}): {message}")]
    LayerFailed {
        layer: usize,
        kind: &'static str,
        message: String,
    },

    #[error("missing required parameter: {0}")]
    MissingParam(String),

    #[error("invalid hex in key at layer {0}")]
    InvalidKeyHex(usize),
}

pub fn execute(
    layers: &[PipelineLayer],
    data: &[u8],
    params: &HashMap<String, String>,
) -> Result<Vec<u8>, PipelineError> {
    layers
        .iter()
        .enumerate()
        .try_fold(data.to_vec(), |buf, (i, layer)| {
            layer.execute(i, &buf, params)
        })
}

impl PipelineLayer {
    fn execute(
        &self,
        idx: usize,
        data: &[u8],
        params: &HashMap<String, String>,
    ) -> Result<Vec<u8>, PipelineError> {
        match self {
            Self::GzipDecompress => run_decompress(idx, data, CompressionType::Gzip),
            Self::ZlibDecompress => run_decompress(idx, data, CompressionType::Zlib),
            Self::ZstdDecompress => run_decompress(idx, data, CompressionType::Zstd),
            Self::Lz4Decompress => run_decompress(idx, data, CompressionType::Lz4),
            Self::AesEcbDecrypt {
                key_hex,
                key_transform,
                key_transform_param,
                key_transform_bytes,
            } => aes_ecb_decrypt(
                idx,
                data,
                &KeySpec::new(key_hex, key_transform, key_transform_param, *key_transform_bytes),
                params,
            ),
            Self::AesCbcDecrypt {
                key_hex,
                iv_hex,
                key_transform,
                key_transform_param,
                key_transform_bytes,
            } => aes_cbc_decrypt(
                idx,
                data,
                &KeySpec::new(key_hex, key_transform, key_transform_param, *key_transform_bytes),
                iv_hex,
                params,
            ),
            Self::Pkcs7Unpad => pkcs7_unpad(idx, data),
            Self::Xor { key_hex } => xor_layer(idx, data, key_hex),
            Self::SkipBytes { count } => skip_bytes(idx, data, *count),
            Self::TakeBytes { offset, length } => take_bytes(idx, data, *offset, *length),
        }
    }
}

fn run_decompress(idx: usize, data: &[u8], ct: CompressionType) -> Result<Vec<u8>, PipelineError> {
    decompress::decompress(data, ct).map_err(|e| layer_err(idx, "decompress", e.to_string()))
}

struct KeySpec<'a> {
    key_hex: &'a str,
    transform: Option<&'a str>,
    transform_param: Option<&'a str>,
    transform_bytes: Option<usize>,
}

impl<'a> KeySpec<'a> {
    fn new(
        key_hex: &'a str,
        transform: &'a Option<String>,
        transform_param: &'a Option<String>,
        transform_bytes: Option<usize>,
    ) -> Self {
        Self {
            key_hex,
            transform: transform.as_deref(),
            transform_param: transform_param.as_deref(),
            transform_bytes,
        }
    }
}

fn layer_err(idx: usize, kind: &'static str, message: String) -> PipelineError {
    PipelineError::LayerFailed {
        layer: idx,
        kind,
        message,
    }
}

fn resolve_key(
    idx: usize,
    spec: &KeySpec,
    params: &HashMap<String, String>,
) -> Result<Vec<u8>, PipelineError> {
    let mut key = hex::decode(spec.key_hex).map_err(|_| PipelineError::InvalidKeyHex(idx))?;

    if let Some(name) = spec.transform {
        apply_key_transform(idx, &mut key, name, spec, params)?;
    }

    Ok(key)
}

fn apply_key_transform(
    idx: usize,
    key: &mut [u8],
    name: &str,
    spec: &KeySpec,
    params: &HashMap<String, String>,
) -> Result<(), PipelineError> {
    match name {
        "xor_prefix" => xor_prefix_transform(idx, key, spec, params),
        other => Err(layer_err(idx, "key_transform", format!("unknown transform: {other}"))),
    }
}

fn xor_prefix_transform(
    idx: usize,
    key: &mut [u8],
    spec: &KeySpec,
    params: &HashMap<String, String>,
) -> Result<(), PipelineError> {
    let param_name = spec
        .transform_param
        .ok_or_else(|| PipelineError::MissingParam("key_transform_param".into()))?;
    let param_value = params
        .get(param_name)
        .ok_or_else(|| PipelineError::MissingParam(param_name.to_string()))?;

    let digits: String = param_value.chars().filter(|c| c.is_ascii_digit()).collect();
    let num: u64 = digits
        .parse()
        .map_err(|_| layer_err(idx, "key_transform", format!("cannot parse '{param_value}' as u64")))?;

    let n = spec.transform_bytes.unwrap_or(8).min(key.len()).min(8);
    let num_bytes = num.to_le_bytes();
    for i in 0..n {
        key[i] ^= num_bytes[i];
    }

    Ok(())
}

fn require_block_aligned(idx: usize, kind: &'static str, data: &[u8]) -> Result<(), PipelineError> {
    if !data.len().is_multiple_of(16) {
        return Err(layer_err(idx, kind, format!("data length {} not a multiple of 16", data.len())));
    }
    Ok(())
}

fn aes_ecb_decrypt(
    idx: usize,
    data: &[u8],
    spec: &KeySpec,
    params: &HashMap<String, String>,
) -> Result<Vec<u8>, PipelineError> {
    require_block_aligned(idx, "aes_ecb_decrypt", data)?;

    let key = resolve_key(idx, spec, params)?;
    if key.len() != 32 {
        return Err(layer_err(idx, "aes_ecb_decrypt", format!("key length {} != 32", key.len())));
    }

    let cipher = Aes256::new(GenericArray::from_slice(&key));
    let mut buf = data.to_vec();
    for chunk in buf.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }

    Ok(buf)
}

fn aes_cbc_decrypt(
    idx: usize,
    data: &[u8],
    spec: &KeySpec,
    iv_hex: &str,
    params: &HashMap<String, String>,
) -> Result<Vec<u8>, PipelineError> {
    require_block_aligned(idx, "aes_cbc_decrypt", data)?;

    let key = resolve_key(idx, spec, params)?;
    let iv = hex::decode(iv_hex).map_err(|_| layer_err(idx, "aes_cbc_decrypt", "invalid IV hex".into()))?;

    if key.len() != 32 || iv.len() != 16 {
        return Err(layer_err(
            idx,
            "aes_cbc_decrypt",
            format!("key len {} (need 32), iv len {} (need 16)", key.len(), iv.len()),
        ));
    }

    let cipher = Aes256::new(GenericArray::from_slice(&key));
    let mut buf = data.to_vec();
    let mut prev_block = iv;

    for chunk in buf.chunks_exact_mut(16) {
        let ciphertext: Vec<u8> = chunk.to_vec();
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
        for j in 0..16 {
            chunk[j] ^= prev_block[j];
        }
        prev_block = ciphertext;
    }

    Ok(buf)
}

fn pkcs7_unpad(idx: usize, data: &[u8]) -> Result<Vec<u8>, PipelineError> {
    if data.is_empty() {
        return Err(layer_err(idx, "pkcs7_unpad", "empty data".into()));
    }

    let pad_len = *data.last().unwrap() as usize;
    if pad_len == 0 || pad_len > data.len() || pad_len > 16 {
        return Err(layer_err(idx, "pkcs7_unpad", format!("invalid padding byte: {pad_len}")));
    }

    if data[data.len() - pad_len..].iter().any(|&b| b as usize != pad_len) {
        return Err(layer_err(idx, "pkcs7_unpad", "padding verification failed".into()));
    }

    Ok(data[..data.len() - pad_len].to_vec())
}

fn xor_layer(idx: usize, data: &[u8], key_hex: &str) -> Result<Vec<u8>, PipelineError> {
    let key = hex::decode(key_hex).map_err(|_| PipelineError::InvalidKeyHex(idx))?;
    if key.is_empty() {
        return Err(layer_err(idx, "xor", "empty key".into()));
    }

    Ok(data.iter().enumerate().map(|(i, &b)| b ^ key[i % key.len()]).collect())
}

fn skip_bytes(idx: usize, data: &[u8], count: usize) -> Result<Vec<u8>, PipelineError> {
    if count > data.len() {
        return Err(layer_err(idx, "skip_bytes", format!("skip {count} but data is only {} bytes", data.len())));
    }
    Ok(data[count..].to_vec())
}

fn take_bytes(idx: usize, data: &[u8], offset: usize, length: usize) -> Result<Vec<u8>, PipelineError> {
    let end = offset.saturating_add(length);
    if end > data.len() {
        return Err(layer_err(idx, "take_bytes", format!("range {offset}..{end} exceeds data length {}", data.len())));
    }
    Ok(data[offset..end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    use aes::cipher::BlockEncrypt;

    #[test]
    fn pkcs7_unpad_valid() {
        let data = [0x41, 0x41, 0x41, 0x05, 0x05, 0x05, 0x05, 0x05];
        let result = pkcs7_unpad(0, &data).unwrap();
        assert_eq!(result, vec![0x41, 0x41, 0x41]);
    }

    #[test]
    fn pkcs7_unpad_full_block_padding() {
        let data = [16u8; 16];
        let result = pkcs7_unpad(0, &data).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn pkcs7_unpad_invalid() {
        let data = [0x41, 0x41, 0x03, 0x02];
        assert!(pkcs7_unpad(0, &data).is_err());
    }

    #[test]
    fn xor_roundtrip() {
        let original = b"hello world";
        let key = "ab";
        let encrypted = xor_layer(0, original, key).unwrap();
        let decrypted = xor_layer(0, &encrypted, key).unwrap();
        assert_eq!(decrypted, original);
    }

    #[test]
    fn skip_bytes_works() {
        let data = [1, 2, 3, 4, 5];
        let result = skip_bytes(0, &data, 2).unwrap();
        assert_eq!(result, vec![3, 4, 5]);
    }

    #[test]
    fn take_bytes_works() {
        let data = [1, 2, 3, 4, 5];
        let result = take_bytes(0, &data, 1, 3).unwrap();
        assert_eq!(result, vec![2, 3, 4]);
    }

    #[test]
    fn aes_ecb_roundtrip() {
        let key_hex = "35ec3377f35db0eabe6b83115403ebfb2725642ed54906290578bd60ba4aa787";
        let key_bytes = hex::decode(key_hex).unwrap();
        let cipher = Aes256::new(GenericArray::from_slice(&key_bytes));

        let plaintext = b"0123456789abcdef";
        let mut block = *plaintext;
        let ga = GenericArray::from_mut_slice(&mut block);
        cipher.encrypt_block(ga);

        let params = HashMap::new();
        let spec = KeySpec {
            key_hex,
            transform: None,
            transform_param: None,
            transform_bytes: None,
        };
        let decrypted = aes_ecb_decrypt(0, &block, &spec, &params).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn aes_ecb_with_key_transform() {
        let base_key = "35ec3377f35db0eabe6b83115403ebfb2725642ed54906290578bd60ba4aa787";
        let steam_id = "76561198012345678";
        let mut params = HashMap::new();
        params.insert("steam_id".to_string(), steam_id.to_string());

        let mut key_bytes = hex::decode(base_key).unwrap();
        let id_num: u64 = steam_id.parse().unwrap();
        let id_bytes = id_num.to_le_bytes();
        for i in 0..8 {
            key_bytes[i] ^= id_bytes[i];
        }

        let cipher = Aes256::new(GenericArray::from_slice(&key_bytes));
        let plaintext = b"test data block!";
        let mut block = *plaintext;
        let ga = GenericArray::from_mut_slice(&mut block);
        cipher.encrypt_block(ga);

        let spec = KeySpec {
            key_hex: base_key,
            transform: Some("xor_prefix"),
            transform_param: Some("steam_id"),
            transform_bytes: Some(8),
        };
        let decrypted = aes_ecb_decrypt(0, &block, &spec, &params).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn full_bl4_pipeline_roundtrip() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let yaml_data = b"player:\n  name: Test\n  level: 42\n";

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(yaml_data).unwrap();
        let compressed = encoder.finish().unwrap();

        let pad_len = 16 - (compressed.len() % 16);
        let mut padded = compressed;
        padded.extend(std::iter::repeat_n(pad_len as u8, pad_len));

        let steam_id = "76561198012345678";
        let base_key = "35ec3377f35db0eabe6b83115403ebfb2725642ed54906290578bd60ba4aa787";
        let mut key_bytes = hex::decode(base_key).unwrap();
        let id_num: u64 = steam_id.parse().unwrap();
        let id_bytes = id_num.to_le_bytes();
        for i in 0..8 {
            key_bytes[i] ^= id_bytes[i];
        }
        let cipher = Aes256::new(GenericArray::from_slice(&key_bytes));
        for chunk in padded.chunks_exact_mut(16) {
            let block = GenericArray::from_mut_slice(chunk);
            cipher.encrypt_block(block);
        }

        let toml_str = include_str!("../../etc/formats/borderlands4.toml");
        let def: super::super::definition::FormatDefinition = toml::from_str(toml_str).unwrap();

        let mut params = HashMap::new();
        params.insert("steam_id".to_string(), steam_id.to_string());

        let result = execute(&def.pipeline, &padded, &params).unwrap();
        assert_eq!(result, yaml_data);
    }
}
