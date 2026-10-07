//! The native AVC profile uses avcC and four-byte NAL lengths. MediaCodec and
//! Media Foundation use Annex B; keep that conversion outside the core model.
use xavi_core::{Error, Result};

pub fn validate_access_unit(mut bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Err(Error::invalid("empty AVC access unit"));
    }
    while !bytes.is_empty() {
        if bytes.len() < 4 {
            return Err(Error::invalid("truncated AVC NAL length"));
        }
        let n = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        bytes = &bytes[4..];
        if n == 0 || n > bytes.len() {
            return Err(Error::invalid("invalid AVC NAL length"));
        }
        bytes = &bytes[n..];
    }
    Ok(())
}

pub fn parameter_sets(avcc: &[u8]) -> Result<(&[u8], &[u8])> {
    let bad = || Error::invalid("invalid AVC configuration");
    if avcc.len() < 11 || avcc[0] != 1 || (avcc[4] & 3) != 3 || (avcc[5] & 31) != 1 {
        return Err(bad());
    }
    let sn = usize::from(u16::from_be_bytes([avcc[6], avcc[7]]));
    if sn < 4 || sn > avcc.len() - 11 || avcc[8 + sn] != 1 {
        return Err(bad());
    }
    let pn = usize::from(u16::from_be_bytes([avcc[9 + sn], avcc[10 + sn]]));
    if pn == 0 || pn != avcc.len() - 11 - sn {
        return Err(bad());
    }
    if avcc[8] & 31 != 7 || avcc[11 + sn] & 31 != 8 || avcc[1..4] != avcc[9..12] {
        return Err(bad());
    }
    Ok((&avcc[8..8 + sn], &avcc[11 + sn..]))
}
#[cfg(any(test, target_os = "android", target_os = "windows"))]
pub fn avcc(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>> {
    if sps.len() < 4
        || sps.len() > (u16::MAX as usize)
        || pps.is_empty()
        || pps.len() > (u16::MAX as usize)
    {
        return Err(Error::invalid("invalid AVC parameter sets"));
    }
    let mut out = vec![1, sps[1], sps[2], sps[3], 255, 225];
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    Ok(out)
}
#[cfg(any(test, target_os = "android", target_os = "windows"))]
pub fn to_annex_b(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while !remaining.is_empty() {
        if remaining.len() < 4 {
            return Err(Error::invalid("truncated AVC NAL length"));
        }
        let len = u32::from_be_bytes(remaining[..4].try_into().unwrap()) as usize;
        remaining = &remaining[4..];
        if len == 0 || len > remaining.len() {
            return Err(Error::invalid("invalid AVC NAL length"));
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&remaining[..len]);
        remaining = &remaining[len..];
    }
    Ok(out)
}
#[cfg(any(test, target_os = "android", target_os = "windows"))]
pub fn nals(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let size = if bytes[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        starts.push((i, i + size));
        i += size;
    }
    if starts.is_empty() || bytes[..starts[0].0].iter().any(|b| *b != 0) {
        return Err(Error::invalid("missing Annex B start code"));
    }
    let mut units = Vec::new();
    for (index, &(_, start)) in starts.iter().enumerate() {
        let mut end = starts.get(index + 1).map_or(bytes.len(), |s| s.0);
        while end > start && bytes[end - 1] == 0 {
            end -= 1;
        }
        if end == start {
            return Err(Error::invalid("empty Annex B NAL"));
        }
        units.push(&bytes[start..end]);
    }
    Ok(units)
}
#[cfg(any(test, target_os = "android", target_os = "windows"))]
pub fn from_annex_b(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for nal in nals(bytes)? {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    Ok(out)
}
#[cfg(any(test, target_os = "android", target_os = "windows"))]
pub fn description(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.first() == Some(&1) {
        parameter_sets(bytes)?;
        return Ok(bytes.to_vec());
    }
    let units = nals(bytes)?;
    let sps = units
        .iter()
        .find(|n| (n[0] & 31) == 7)
        .ok_or_else(|| Error::invalid("missing AVC SPS"))?;
    let pps = units
        .iter()
        .find(|n| (n[0] & 31) == 8)
        .ok_or_else(|| Error::invalid("missing AVC PPS"))?;
    avcc(sps, pps)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn access_units_and_configuration_round_trip_without_touching_escaped_bytes() {
        let bytes = [
            0, 0, 0, 1, 0x67, 0x42, 0, 30, 0, 0, 3, 1, 0x80, 0, 0, 1, 0x68, 0x80,
        ];
        let config = description(&bytes).unwrap();
        let (sps, pps) = parameter_sets(&config).unwrap();
        assert_eq!(sps, &[0x67, 0x42, 0, 30, 0, 0, 3, 1, 0x80]);
        assert_eq!(pps, &[0x68, 0x80]);
        assert_eq!(
            nals(&to_annex_b(&from_annex_b(&bytes).unwrap()).unwrap()).unwrap(),
            nals(&bytes).unwrap()
        );
        for bad in [&[][..], &[0, 0, 0], &[0, 0, 0, 5, 1], &[0, 0, 0, 0]] {
            assert!(to_annex_b(bad).is_err() || bad.is_empty());
        }
    }
}
