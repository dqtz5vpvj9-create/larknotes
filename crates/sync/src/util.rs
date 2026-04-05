/// Decode file content from any encoding.
/// 1. Check BOM (UTF-16 LE/BE, UTF-8 BOM)
/// 2. Try UTF-8
/// 3. Auto-detect encoding via chardetng (handles GBK, Shift-JIS, Latin-1, etc.)
pub fn decode_content(raw: &[u8]) -> String {
    // UTF-16 LE BOM
    if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
        let (decoded, _, _) = encoding_rs::UTF_16LE.decode(&raw[2..]);
        return decoded.into_owned();
    }
    // UTF-16 BE BOM
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        let (decoded, _, _) = encoding_rs::UTF_16BE.decode(&raw[2..]);
        return decoded.into_owned();
    }
    // UTF-8 BOM
    if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
        return String::from_utf8_lossy(&raw[3..]).into_owned();
    }
    // Try valid UTF-8 first
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    // Auto-detect encoding (GBK, Shift-JIS, EUC-KR, Latin-1, etc.)
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(raw, true);
    let encoding = detector.guess(None, true);
    tracing::info!("检测到文件编码: {}", encoding.name());
    let (decoded, _, _) = encoding.decode(raw);
    decoded.into_owned()
}
