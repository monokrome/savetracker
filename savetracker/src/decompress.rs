use std::io::Read;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompressionType {
    Gzip,
    Zlib,
    Zstd,
    Lz4,
}

impl std::fmt::Display for CompressionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gzip => write!(f, "gzip"),
            Self::Zlib => write!(f, "zlib"),
            Self::Zstd => write!(f, "zstd"),
            Self::Lz4 => write!(f, "lz4"),
        }
    }
}

#[derive(Debug, Error)]
pub enum DecompressError {
    #[error("{kind} decompression failed: {message}")]
    Failed { kind: CompressionType, message: String },
}

pub fn decompress(data: &[u8], compression: CompressionType) -> Result<Vec<u8>, DecompressError> {
    match compression {
        CompressionType::Gzip => decompress_gzip(data),
        CompressionType::Zlib => decompress_zlib(data),
        CompressionType::Zstd => decompress_zstd(data),
        CompressionType::Lz4 => decompress_lz4(data),
    }
}

fn fail(kind: CompressionType, e: impl std::fmt::Display) -> DecompressError {
    DecompressError::Failed {
        kind,
        message: e.to_string(),
    }
}

fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let mut decoder = flate2::read::GzDecoder::new(data);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf).map_err(|e| fail(CompressionType::Gzip, e))?;
    Ok(buf)
}

fn decompress_zlib(data: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf).map_err(|e| fail(CompressionType::Zlib, e))?;
    Ok(buf)
}

fn decompress_zstd(data: &[u8]) -> Result<Vec<u8>, DecompressError> {
    zstd::stream::decode_all(data).map_err(|e| fail(CompressionType::Zstd, e))
}

fn decompress_lz4(data: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let mut decoder = lz4_flex::frame::FrameDecoder::new(data);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf).map_err(|e| fail(CompressionType::Lz4, e))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_gzip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"hello world, this is a test";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let result = decompress(&compressed, CompressionType::Gzip).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn roundtrip_zlib() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"hello world, this is a test";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let result = decompress(&compressed, CompressionType::Zlib).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn roundtrip_zstd() {
        let original = b"hello world, this is a test";
        let compressed = zstd::stream::encode_all(&original[..], 3).unwrap();

        let result = decompress(&compressed, CompressionType::Zstd).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn roundtrip_lz4() {
        use lz4_flex::frame::FrameEncoder;
        use std::io::Write;

        let original = b"hello world, this is a test";
        let mut encoder = FrameEncoder::new(Vec::new());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let result = decompress(&compressed, CompressionType::Lz4).unwrap();
        assert_eq!(result, original);
    }
}
