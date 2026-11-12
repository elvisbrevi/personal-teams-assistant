//! Images posted inside a Teams message as Graph hosted contents: no external host, no link.
//!
//! The `pta` process that receives the call reads a path (the host never opens one) and sends
//! the bytes to the host, which checks them again. The type comes only from the bytes (PNG,
//! JPEG, GIF), never from a name, an extension or a declared type. Graph caps a write request at
//! 4 MB and base64 adds a third, so all the images of a message share [`MAX_TOTAL`] bytes.
use anyhow::Result;
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Images in one message.
pub const MAX_IMAGES: usize = 4;
/// Bytes of all the images of one message together (about 3.9 MB once encoded).
pub const MAX_TOTAL: usize = 2_900_000;
/// Characters of an image's alternative text.
pub const MAX_ALT: usize = 200;
/// Characters of the name shown in the confirmation.
const MAX_NAME: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
}
impl Kind {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
        }
    }
}

/// Why an image was not accepted: fixed explanations that are safe to show as they are.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageRefusal {
    TooMany,
    TooLarge,
    Unsupported,
    Unreadable,
    InvalidAlt,
    Invalid,
}
impl std::fmt::Display for ImageRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooMany => "A message carries at most 4 images.",
            Self::TooLarge => {
                "The images exceed 2.9 MB together, the most one Teams message can carry."
            }
            Self::Unsupported => {
                "Only PNG, JPEG and GIF images can be posted (read from the file's bytes)."
            }
            Self::Unreadable => "The image could not be read: give the path of a regular file.",
            Self::InvalidAlt => {
                "An image's alt text must be at most 200 characters, without control characters."
            }
            Self::Invalid => "Each image needs exactly one of path or data (base64).",
        })
    }
}
impl std::error::Error for ImageRefusal {}

/// One checked image, with the bytes that are shown for confirmation and then sent.
#[derive(Clone, Debug)]
pub struct Image {
    pub name: String,
    pub alt: String,
    pub kind: Kind,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}
impl Image {
    /// Checks `bytes` as an image: its type and size come from its own header.
    pub fn new(name: &str, alt: &str, bytes: Vec<u8>) -> Result<Self, ImageRefusal> {
        if bytes.len() > MAX_TOTAL {
            return Err(ImageRefusal::TooLarge);
        }
        let (kind, width, height) = detect(&bytes).ok_or(ImageRefusal::Unsupported)?;
        let alt = alt.trim();
        if alt.chars().count() > MAX_ALT || alt.chars().any(char::is_control) {
            return Err(ImageRefusal::InvalidAlt);
        }
        let name: String = name
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_NAME)
            .collect();
        let name = if name.trim().is_empty() {
            format!("image.{}", kind.extension())
        } else {
            name.trim().to_owned()
        };
        Ok(Self {
            name,
            alt: alt.to_owned(),
            kind,
            width,
            height,
            bytes,
        })
    }
    /// Reads a regular file, refusing one larger than an image may be before reading it.
    pub fn read(path: &Path, alt: &str) -> Result<Self, ImageRefusal> {
        let file = std::fs::File::open(path).map_err(|_| ImageRefusal::Unreadable)?;
        let metadata = file.metadata().map_err(|_| ImageRefusal::Unreadable)?;
        if !metadata.is_file() {
            return Err(ImageRefusal::Unreadable);
        }
        if metadata.len() > MAX_TOTAL as u64 {
            return Err(ImageRefusal::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take(MAX_TOTAL as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ImageRefusal::Unreadable)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::new(&name, alt, bytes)
    }
    /// The first 8 hexadecimal digits of the SHA-256 of the bytes.
    pub fn digest(&self) -> String {
        Sha256::digest(&self.bytes)
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    /// What the person approves: `homero.gif · GIF 480×400 · 1.05 MB · sha256 1a2b3c4d`.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "{} · {} {}×{} · {} · sha256 {}",
            self.name,
            self.kind.label(),
            self.width,
            self.height,
            size(self.bytes.len()),
            self.digest()
        );
        if !self.alt.is_empty() {
            line.push_str(&format!(" · alt «{}»", self.alt));
        }
        line
    }
    /// The form the `pta` process sends to the host, which checks it again with [`from_wire`].
    pub fn wire(&self) -> Value {
        json!({"name": self.name, "alt": self.alt,
            "data": base64::engine::general_purpose::STANDARD.encode(&self.bytes)})
    }
    /// What a result and the confirmation report of an image: never its bytes.
    pub fn describe(&self) -> Value {
        json!({"name": self.name, "type": self.kind.content_type(), "width": self.width,
            "height": self.height, "bytes": self.bytes.len(), "sha256": self.digest()})
    }
}

/// `1.05 MB`, `830.2 KB`, `512 B`.
pub fn size(bytes: usize) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1e3),
        _ => format!("{:.2} MB", bytes as f64 / 1e6),
    }
}

/// An image as a tool argument: exactly one of `path` (read here) or `data` (base64), and an
/// optional `alt`. `name` names an image given as data.
pub fn from_argument(value: &Value) -> Result<Image, ImageRefusal> {
    let alt = match value.get("alt") {
        None | Some(Value::Null) => "",
        Some(Value::String(alt)) => alt,
        Some(_) => return Err(ImageRefusal::InvalidAlt),
    };
    match (value.get("path"), value.get("data")) {
        (Some(Value::String(path)), None) if !path.trim().is_empty() => {
            Image::read(Path::new(path.trim()), alt)
        }
        (None, Some(Value::String(_))) => from_wire(value),
        _ => Err(ImageRefusal::Invalid),
    }
}

/// An image as the host receives it: `{name, alt, data}`, with `data` in base64.
pub fn from_wire(value: &Value) -> Result<Image, ImageRefusal> {
    let data = value["data"].as_str().ok_or(ImageRefusal::Invalid)?;
    // A string longer than the largest encoding is refused before decoding it.
    if data.len() > MAX_TOTAL.div_ceil(3) * 4 + 4 {
        return Err(ImageRefusal::TooLarge);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|_| ImageRefusal::Unsupported)?;
    let alt = match &value["alt"] {
        Value::Null => "",
        Value::String(alt) => alt,
        _ => return Err(ImageRefusal::InvalidAlt),
    };
    Image::new(value["name"].as_str().unwrap_or_default(), alt, bytes)
}

/// The images of an argument list, checked one by one and together.
pub fn from_list(
    value: Option<&Value>,
    each: impl Fn(&Value) -> Result<Image, ImageRefusal>,
) -> Result<Vec<Image>, ImageRefusal> {
    let list = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(ImageRefusal::Invalid),
    };
    if list.len() > MAX_IMAGES {
        return Err(ImageRefusal::TooMany);
    }
    let images = list.iter().map(each).collect::<Result<Vec<_>, _>>()?;
    check(&images)?;
    Ok(images)
}

/// The limits of one message's images together.
pub fn check(images: &[Image]) -> Result<(), ImageRefusal> {
    if images.len() > MAX_IMAGES {
        return Err(ImageRefusal::TooMany);
    }
    if images.iter().map(|i| i.bytes.len()).sum::<usize>() > MAX_TOTAL {
        return Err(ImageRefusal::TooLarge);
    }
    Ok(())
}

/// The type and size of an image, read from its header; `None` for anything else.
pub fn detect(bytes: &[u8]) -> Option<(Kind, u32, u32)> {
    let be16 = |at: usize| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let (kind, width, height) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        if bytes.get(12..16)? != b"IHDR" {
            return None;
        }
        let be32 = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        (Kind::Png, be32(16)?, be32(20)?)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let le16 = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
        (Kind::Gif, u32::from(le16(6)?), u32::from(le16(8)?))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        // Walk the segments up to the frame header that holds the size.
        let mut at = 2;
        loop {
            if *bytes.get(at)? != 0xFF {
                return None;
            }
            let marker = *bytes.get(at + 1)?;
            match marker {
                0xFF => at += 1,
                0x01 | 0xD0..=0xD7 => at += 2,
                0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                    break (
                        Kind::Jpeg,
                        u32::from(be16(at + 7)?),
                        u32::from(be16(at + 5)?),
                    );
                }
                0xD9 | 0xDA => return None,
                _ => at += 2 + usize::from(be16(at + 2)?),
            }
        }
    } else {
        return None;
    };
    (width > 0 && height > 0).then_some((kind, width, height))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid header of each kind, `width`×`height`, padded to `len` bytes.
    pub(crate) fn png(width: u32, height: u32, len: usize) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.resize(len.max(bytes.len()), 0);
        bytes
    }
    pub(crate) fn gif(width: u16, height: u16, len: usize) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend(width.to_le_bytes());
        bytes.extend(height.to_le_bytes());
        bytes.resize(len.max(bytes.len()), 0);
        bytes
    }
    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8];
        // An APP0 segment before the frame header, as JFIF files have.
        bytes.extend([0xFF, 0xE0, 0x00, 0x10]);
        bytes.extend(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        bytes.extend([0xFF, 0xC0, 0x00, 0x11, 0x08]);
        bytes.extend(height.to_be_bytes());
        bytes.extend(width.to_be_bytes());
        bytes.extend([0x03, 0x01, 0x22, 0x00]);
        bytes
    }

    #[test]
    fn the_type_and_size_come_from_the_bytes_only() {
        assert_eq!(detect(&png(640, 480, 64)), Some((Kind::Png, 640, 480)));
        assert_eq!(detect(&gif(480, 400, 64)), Some((Kind::Gif, 480, 400)));
        assert_eq!(detect(&jpeg(1024, 768)), Some((Kind::Jpeg, 1024, 768)));
        // WebP, SVG, text and truncated or empty headers are not images here.
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.resize(64, 0);
        for bytes in [
            webp,
            b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
            b"GIF89a".to_vec(),
            png(0, 10, 64),
            vec![0xFF, 0xD8, 0xFF, 0xD9],
            Vec::new(),
        ] {
            assert_eq!(detect(&bytes), None, "{bytes:?}");
        }
        // A file named .png holding a GIF is a GIF.
        let image = Image::new("photo.png", "", gif(2, 2, 32)).unwrap();
        assert_eq!(image.kind, Kind::Gif);
    }

    #[test]
    fn data_in_any_other_shape_is_refused() {
        let data = base64::engine::general_purpose::STANDARD.encode(gif(2, 2, 32));
        let both = json!({"path": "/tmp/x.gif", "data": data});
        assert_eq!(from_argument(&both).unwrap_err(), ImageRefusal::Invalid);
        assert_eq!(
            from_argument(&json!({})).unwrap_err(),
            ImageRefusal::Invalid
        );
        assert_eq!(
            from_argument(&json!({"data": "%%%"})).unwrap_err(),
            ImageRefusal::Unsupported
        );
        let text = base64::engine::general_purpose::STANDARD.encode("hola");
        assert_eq!(
            from_argument(&json!({"data": text})).unwrap_err(),
            ImageRefusal::Unsupported
        );
        let named = from_argument(&json!({"data": data})).unwrap();
        assert_eq!(named.name, "image.gif");
    }

    #[test]
    fn count_size_and_alt_are_limited_in_code() {
        let one = Image::new("a.gif", "", gif(2, 2, 32)).unwrap();
        assert!(check(&vec![one.clone(); MAX_IMAGES]).is_ok());
        assert_eq!(
            check(&vec![one; MAX_IMAGES + 1]).unwrap_err(),
            ImageRefusal::TooMany
        );
        // One image over the limit, and two that pass alone but not together.
        assert_eq!(
            Image::new("big.png", "", png(9, 9, MAX_TOTAL + 1)).unwrap_err(),
            ImageRefusal::TooLarge
        );
        let half = Image::new("half.png", "", png(9, 9, MAX_TOTAL / 2 + 1)).unwrap();
        assert_eq!(
            check(&[half.clone(), half]).unwrap_err(),
            ImageRefusal::TooLarge
        );
        let long = "a".repeat(MAX_ALT + 1);
        assert_eq!(
            Image::new("a.gif", &long, gif(2, 2, 32)).unwrap_err(),
            ImageRefusal::InvalidAlt
        );
        assert_eq!(
            Image::new("a.gif", "line\nbreak", gif(2, 2, 32)).unwrap_err(),
            ImageRefusal::InvalidAlt
        );
        let list: Vec<Value> = (0..=MAX_IMAGES).map(|_| json!({"data": ""})).collect();
        assert_eq!(
            from_list(Some(&json!(list)), from_wire).unwrap_err(),
            ImageRefusal::TooMany
        );
    }

    #[test]
    fn a_path_is_read_only_when_it_is_a_small_enough_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("homero.gif");
        std::fs::write(&file, gif(480, 400, 1_050_994)).unwrap();
        let image = Image::read(&file, "Homero").unwrap();
        assert_eq!(
            image.summary(),
            format!(
                "homero.gif · GIF 480×400 · 1.05 MB · sha256 {} · alt «Homero»",
                image.digest()
            )
        );
        assert_eq!(image.digest().len(), 8);
        // The wire form carries the same bytes back.
        let back = from_wire(&image.wire()).unwrap();
        assert_eq!(back.bytes, image.bytes);
        assert_eq!(
            (back.name.as_str(), back.alt.as_str()),
            ("homero.gif", "Homero")
        );
        assert_eq!(
            Image::read(dir.path(), "").unwrap_err(),
            ImageRefusal::Unreadable
        );
        assert_eq!(
            Image::read(&dir.path().join("missing.gif"), "").unwrap_err(),
            ImageRefusal::Unreadable
        );
        let big = dir.path().join("big.png");
        std::fs::write(&big, png(9, 9, MAX_TOTAL + 1)).unwrap();
        assert_eq!(Image::read(&big, "").unwrap_err(), ImageRefusal::TooLarge);
    }
}
