use crate::model::{ConfigSource, VersionEvidence};
use std::io::{Read, Seek, SeekFrom};

fn number(data: &[u8], offset: usize, size: usize, little: bool) -> Option<u64> {
    let bytes = data.get(offset..offset + size)?;
    let mut out = 0u64;
    if little {
        for (i, b) in bytes.iter().enumerate() {
            out |= (*b as u64) << (i * 8);
        }
    } else {
        for b in bytes {
            out = (out << 8) | *b as u64;
        }
    }
    Some(out)
}
fn region<R: Read + Seek>(reader: &mut R, offset: u64, size: usize, cap: usize) -> Option<Vec<u8>> {
    if size > cap {
        return None;
    }
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut b = vec![0; size];
    reader.read_exact(&mut b).ok()?;
    Some(b)
}
pub(super) fn from_elf<R: Read + Seek>(reader: &mut R) -> Option<VersionEvidence> {
    let header = region(reader, 0, 64, 64)?;
    if &header[..4] != b"\x7fELF" || header[4] != 2 || ![1, 2].contains(&header[5]) {
        return None;
    }
    let le = header[5] == 1;
    let off = number(&header, 40, 8, le)?;
    let ent = number(&header, 58, 2, le)? as usize;
    let count = number(&header, 60, 2, le)? as usize;
    let stridx = number(&header, 62, 2, le)? as usize;
    if ent != 64 || count == 0 || count > 1024 || stridx >= count {
        return None;
    }
    let sections = region(reader, off, ent.checked_mul(count)?, 65536)?;
    let sh = &sections[stridx * ent..(stridx + 1) * ent];
    let names = region(
        reader,
        number(sh, 24, 8, le)?,
        number(sh, 32, 8, le)? as usize,
        262144,
    )?;
    for entry in sections.chunks_exact(ent) {
        let n = number(entry, 0, 4, le)? as usize;
        let name = names.get(n..)?.split(|b| *b == 0).next()?;
        if name == b".go.buildinfo" {
            let b = region(
                reader,
                number(entry, 24, 8, le)?,
                number(entry, 32, 8, le)? as usize,
                1048576,
            )?;
            return from_buildinfo(&b);
        }
    }
    None
}
fn string(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let (mut n, mut shift) = (0usize, 0u32);
    for (i, b) in data.iter().take(10).enumerate() {
        if shift >= usize::BITS {
            return None;
        }
        let payload = (b & 127) as usize;
        if payload > usize::MAX.checked_shr(shift)? {
            return None;
        }
        n |= payload.checked_shl(shift)?;
        if b & 128 == 0 {
            let begin = i.checked_add(1)?;
            let end = begin.checked_add(n)?;
            return Some((data.get(begin..end)?, data.get(end..)?));
        }
        shift += 7;
    }
    None
}
pub(super) fn from_buildinfo(data: &[u8]) -> Option<VersionEvidence> {
    if data.len() < 32 || &data[..14] != b"\xff Go buildinf:" || data[15] & 2 == 0 {
        return None;
    }
    let (_, rest) = string(&data[32..])?;
    let (module, _) = string(rest)?;
    let raw = std::str::from_utf8(module).ok();
    let text = raw.or_else(|| {
        if module.len() > 32 {
            std::str::from_utf8(&module[16..module.len() - 16]).ok()
        } else {
            None
        }
    })?;
    let mut version = None;
    let mut commit = None;
    let mut core = None;
    let mut is_app = false;
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.first() == Some(&"mod")
            && fields.len() >= 3
            && fields[1].starts_with("github.com/celestiaorg/celestia-app")
        {
            is_app = true;
            if fields[2].starts_with('v') {
                version = Some(fields[2].to_owned());
            }
        }
        if fields.len() >= 3
            && (fields[0] == "dep" || fields[0] == "=>")
            && fields[1] == "github.com/celestiaorg/celestia-core"
        {
            core = Some(fields[2].to_owned());
        }
        if let Some(v) = line.strip_prefix("build\tvcs.revision=") {
            if v.len() <= 64 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
                commit = Some(v.into());
            }
        }
        if line.starts_with("build\t-ldflags=") {
            for token in line.split_whitespace() {
                for key in [
                    "github.com/cosmos/cosmos-sdk/version.Version=",
                    "github.com/celestiaorg/cosmos-sdk/version.Version=",
                ] {
                    if let Some(v) = token.strip_prefix(key) {
                        let v = v.trim_matches(['\'', '"']);
                        if v.len() <= 64
                            && v.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
                        {
                            version = Some(if v.starts_with('v') {
                                v.into()
                            } else {
                                format!("v{v}")
                            });
                        }
                    }
                }
            }
        }
    }
    if !is_app {
        return None;
    }
    if version.is_none() && core.is_none() {
        return None;
    }
    Some(VersionEvidence {
        version: version.unwrap_or_else(|| "unknown".into()),
        commit,
        core_version: core,
        source: ConfigSource::ObservedRuntime,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn append_string(out: &mut Vec<u8>, s: &[u8]) {
        let mut n = s.len();
        while n >= 128 {
            out.push((n as u8) | 128);
            n >>= 7;
        }
        out.push(n as u8);
        out.extend_from_slice(s);
    }
    #[test]
    fn go_version_is_not_application_version() {
        let mut b = vec![0; 32];
        b[..14].copy_from_slice(b"\xff Go buildinf:");
        b[15] = 2;
        append_string(&mut b, b"go1.26.6");
        append_string(&mut b,b"mod\tgithub.com/celestiaorg/celestia-app/v10\tv10.4.0-mocha\n=>\tgithub.com/celestiaorg/celestia-core\tv0.42.3\n");
        let v = from_buildinfo(&b).unwrap();
        assert_eq!(v.version, "v10.4.0-mocha");
        assert_eq!(v.core_version.as_deref(), Some("v0.42.3"));
    }
    #[test]
    fn unknown_or_truncated_executable_does_not_invent_version() {
        assert!(from_elf(&mut std::io::Cursor::new(vec![0u8; 128])).is_none());
        assert!(from_buildinfo(b"\xff Go buildinf:").is_none());
    }
    #[test]
    fn malformed_buildinfo_length_does_not_overflow_or_panic() {
        assert!(string(&[255, 255, 255, 255, 255, 255, 255, 255, 255, 1]).is_none());
        assert!(string(&[255, 255, 255, 255, 255, 255, 255, 255, 255, 127]).is_none());
    }
}
