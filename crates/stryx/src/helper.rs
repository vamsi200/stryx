use xorf::{BinaryFuse8, BinaryFuse16, Descriptor};

use crate::{
    builder::{FilterData8, FilterData16, FilterSection, FilterTypeRepr, XorFilter},
    decoder::Decoder,
    error::{DecodeError, StryxError},
};

pub(crate) fn filter_encode(filter: XorFilter) -> Result<Option<FilterSection>, StryxError> {
    match filter {
        XorFilter::None => Ok(None),
        XorFilter::BinaryFuse8(bf) => {
            let mut encoded = vec![];

            let data = FilterData8 {
                seed: bf.descriptor.seed,
                segment_length: bf.descriptor.segment_length,
                segment_length_mask: bf.descriptor.segment_length_mask,
                segment_count_length: bf.descriptor.segment_count_length,
                fingerprint_length: u32::try_from(bf.fingerprints.len())?,
                fingerprints: bf.fingerprints,
            };

            encoded.extend_from_slice(&data.seed.to_le_bytes());
            encoded.extend_from_slice(&data.segment_length.to_le_bytes());
            encoded.extend_from_slice(&data.segment_length_mask.to_le_bytes());
            encoded.extend_from_slice(&data.segment_count_length.to_le_bytes());
            encoded.extend_from_slice(&data.fingerprint_length.to_le_bytes());
            encoded.extend_from_slice(&*data.fingerprints);

            let checksum = crc32c::crc32c(&encoded);

            Ok(Some(FilterSection {
                data: encoded,
                checksum,
            }))
        }
        XorFilter::BinaryFuse16(bf) => {
            let mut encoded = vec![];

            let data = FilterData16 {
                seed: bf.descriptor.seed,
                segment_length: bf.descriptor.segment_length,
                segment_length_mask: bf.descriptor.segment_length_mask,
                segment_count_length: bf.descriptor.segment_count_length,
                fingerprint_length: u32::try_from(bf.fingerprints.len())?,
                fingerprints: bf.fingerprints,
            };

            encoded.extend_from_slice(&data.seed.to_le_bytes());
            encoded.extend_from_slice(&data.segment_length.to_le_bytes());
            encoded.extend_from_slice(&data.segment_length_mask.to_le_bytes());
            encoded.extend_from_slice(&data.segment_count_length.to_le_bytes());
            encoded.extend_from_slice(&data.fingerprint_length.to_le_bytes());
            for fingerprint in data.fingerprints.iter() {
                encoded.extend_from_slice(&fingerprint.to_le_bytes());
            }

            let checksum = crc32c::crc32c(&encoded);

            Ok(Some(FilterSection {
                data: encoded,
                checksum,
            }))
        }
    }
}

fn decode_fuse8(data: &[u8]) -> Result<BinaryFuse8, StryxError> {
    let mut d = Decoder::new(data);
    let seed = d.read_u64()?;
    let segment_length = d.read_u32()?;
    let segment_length_mask = d.read_u32()?;
    let segment_count_length = d.read_u32()?;
    let fp_len = d.read_u32()? as usize;

    if !segment_length.is_power_of_two() || segment_length_mask != segment_length - 1 {
        return Err(DecodeError::InvalidLength.into());
    }

    // fingerprints must cover (segment_count + 2) segments; segment_count_length = segment_count * segment_length
    let min_len = (segment_count_length as usize)
        .checked_add(2 * segment_length as usize)
        .ok_or(DecodeError::InvalidLength)?;

    if fp_len < min_len {
        return Err(DecodeError::InvalidLength.into());
    }

    if d.remaining() != fp_len {
        return Err(DecodeError::InvalidLength)?;
    }
    let fingerprints: Box<[u8]> = data[data.len() - fp_len..].into();

    Ok(BinaryFuse8 {
        fingerprints,
        descriptor: Descriptor {
            seed,
            segment_length_mask,
            segment_length,
            segment_count_length,
        },
    })
}

fn decode_fuse16(data: &[u8]) -> Result<BinaryFuse16, StryxError> {
    let mut d = Decoder::new(data);
    let seed = d.read_u64()?;
    let segment_length = d.read_u32()?;
    let segment_length_mask = d.read_u32()?;
    let segment_count_length = d.read_u32()?;
    let fp_len = d.read_u32()? as usize;

    if !segment_length.is_power_of_two() || segment_length_mask != segment_length - 1 {
        return Err(DecodeError::InvalidLength.into());
    }

    // fingerprints must cover (segment_count + 2) segments; segment_count_length = segment_count * segment_length
    let min_len = (segment_count_length as usize)
        .checked_add(2 * segment_length as usize)
        .ok_or(DecodeError::InvalidLength)?;

    if fp_len < min_len {
        return Err(DecodeError::InvalidLength.into());
    }

    let byte_len = fp_len.checked_mul(2).ok_or(DecodeError::InvalidLength)?;
    if d.remaining() != byte_len {
        return Err(DecodeError::InvalidLength.into());
    }

    let fingerprints: Box<[u16]> = data[data.len() - byte_len..]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();

    Ok(BinaryFuse16 {
        fingerprints,
        descriptor: Descriptor {
            seed,
            segment_length_mask,
            segment_length,
            segment_count_length,
        },
    })
}

pub fn filter_decode(data: &[u8], selected_type: FilterTypeRepr) -> Result<XorFilter, StryxError> {
    match selected_type {
        FilterTypeRepr::None => Ok(XorFilter::None),
        FilterTypeRepr::BinaryFuse8 => Ok(XorFilter::BinaryFuse8(decode_fuse8(data)?)),
        FilterTypeRepr::BinaryFuse16 => Ok(XorFilter::BinaryFuse16(decode_fuse16(data)?)),
    }
}

#[cfg(test)]
mod filter_codec_tests {
    use super::*;
    use xorf::Filter as XorfFilter;

    // Header layout: seed(8) | seg_len(4) | mask(4) | seg_count_len(4) | fp_len(4) | payload
    const OFF_SEG_LEN: usize = 8;
    const OFF_MASK: usize = 12;
    const OFF_FP_LEN: usize = 20;
    const HEADER: usize = 24;

    fn hashes(n: u64) -> Vec<u64> {
        (0..n)
            .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15)) // golden ratio mentioned
            .collect()
    }

    fn absent_keys() -> Vec<u64> {
        (0..1000u64)
            .map(|i| (i + 1_000_000).wrapping_mul(0xD6E8_FEB8_6659_FD93))
            .collect()
    }

    fn encode8(keys: &[u64]) -> (BinaryFuse8, Vec<u8>) {
        let bf = BinaryFuse8::try_from(&keys.to_vec()).unwrap();
        let section = filter_encode(XorFilter::BinaryFuse8(bf.clone()))
            .unwrap()
            .unwrap();
        (bf, section.data)
    }

    fn encode16(keys: &[u64]) -> (BinaryFuse16, Vec<u8>) {
        let bf = BinaryFuse16::try_from(&keys.to_vec()).unwrap();
        let section = filter_encode(XorFilter::BinaryFuse16(bf.clone()))
            .unwrap()
            .unwrap();
        (bf, section.data)
    }

    fn put_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn fuse8_round_trip() {
        let keys = hashes(10_000);
        let (orig, bytes) = encode8(&keys);
        let decoded = decode_fuse8(&bytes).unwrap();

        assert_eq!(decoded.fingerprints, orig.fingerprints);
        for k in &keys {
            assert!(decoded.contains(k), "false negative for {k}");
        }

        // false positives are expected
        for k in absent_keys() {
            assert_eq!(decoded.contains(&k), orig.contains(&k));
        }
    }

    #[test]
    fn fuse16_round_trip() {
        let keys = hashes(10_000);
        let (orig, bytes) = encode16(&keys);
        let decoded = decode_fuse16(&bytes).unwrap();

        assert_eq!(decoded.fingerprints, orig.fingerprints);
        for k in &keys {
            assert!(decoded.contains(k), "false negative for {k}");
        }
        for k in absent_keys() {
            assert_eq!(decoded.contains(&k), orig.contains(&k));
        }
    }

    #[test]
    fn round_trip_small_sets() {
        for n in [1u64, 2, 3, 10, 100] {
            let keys = hashes(n);

            let (_, b8) = encode8(&keys);
            let d8 = decode_fuse8(&b8).unwrap();
            assert!(keys.iter().all(|k| d8.contains(k)), "fuse8 n={n}");

            let (_, b16) = encode16(&keys);
            let d16 = decode_fuse16(&b16).unwrap();
            assert!(keys.iter().all(|k| d16.contains(k)), "fuse16 n={n}");
        }
    }

    #[test]
    fn round_trip_many_sizes() {
        for n in [1usize, 2, 5, 50, 500, 5_000, 50_000, 500_000, 2_000_000] {
            let keys = hashes(n as u64);

            let (_, b8) = encode8(&keys);
            let d8 =
                decode_fuse8(&b8).unwrap_or_else(|e| panic!("fuse8 decode failed at n={n}: {e:?}"));
            assert!(
                keys.iter().all(|k| d8.contains(k)),
                "fuse8 false negative at n={n}"
            );

            let (_, b16) = encode16(&keys);
            let d16 = decode_fuse16(&b16)
                .unwrap_or_else(|e| panic!("fuse16 decode failed at n={n}: {e:?}"));
            assert!(
                keys.iter().all(|k| d16.contains(k)),
                "fuse16 false negative at n={n}"
            );
        }
    }

    #[test]
    fn fuse8_rejects_truncated_payload() {
        let (_, mut bytes) = encode8(&hashes(1000));
        bytes.pop();
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_trailing_bytes() {
        let (_, mut bytes) = encode8(&hashes(1000));
        bytes.push(0);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_truncated_header() {
        let (_, bytes) = encode8(&hashes(1000));
        assert!(decode_fuse8(&bytes[..HEADER - 1]).is_err());
        assert!(decode_fuse8(&[]).is_err());
    }

    #[test]
    fn fuse8_rejects_oversized_fp_len() {
        let (_, mut bytes) = encode8(&hashes(1000));
        put_u32(&mut bytes, OFF_FP_LEN, u32::MAX);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_non_power_of_two_segment_length() {
        let (_, mut bytes) = encode8(&hashes(1000));
        put_u32(&mut bytes, OFF_SEG_LEN, 3);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_mask_mismatch() {
        let (_, mut bytes) = encode8(&hashes(1000));
        put_u32(&mut bytes, OFF_MASK, 0);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_too_few_fingerprints() {
        let (_, mut bytes) = encode8(&hashes(1000));
        put_u32(&mut bytes, OFF_FP_LEN, 0);
        bytes.truncate(HEADER);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_truncated_payload() {
        let (_, mut bytes) = encode16(&hashes(1000));
        bytes.pop();
        assert!(decode_fuse16(&bytes).is_err());
        bytes.pop();
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_trailing_bytes() {
        let (_, mut bytes) = encode16(&hashes(1000));
        bytes.extend_from_slice(&[0, 0]);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_element_count_used_as_byte_count() {
        let (_, mut bytes) = encode16(&hashes(1000));
        let fp_len = u32::from_le_bytes(bytes[OFF_FP_LEN..OFF_FP_LEN + 4].try_into().unwrap());
        bytes.truncate(HEADER + fp_len as usize);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_oversized_fp_len() {
        let (_, mut bytes) = encode16(&hashes(1000));
        put_u32(&mut bytes, OFF_FP_LEN, u32::MAX);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_non_power_of_two_segment_length() {
        let (_, mut bytes) = encode16(&hashes(1000));
        put_u32(&mut bytes, OFF_SEG_LEN, 3);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_too_few_fingerprints() {
        let (_, mut bytes) = encode16(&hashes(1000));
        put_u32(&mut bytes, OFF_FP_LEN, 0);
        bytes.truncate(HEADER);
        assert!(decode_fuse16(&bytes).is_err());
    }
}
